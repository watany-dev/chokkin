"""Shared helpers for the OSS corpus measurement scripts (#338-#342).

Imported by scripts/oss-*.py; the scripts directory is on sys.path because the
importing script lives next to this file.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = ROOT / "scripts/oss-clones.manifest"
DEFAULT_RECALL = ROOT / "scripts/oss-recall.manifest"
DEFAULT_CLONES = ROOT / "target/oss-clones"
DEFAULT_BIN = ROOT / "target/release/chokkin"
DEFAULT_ENV_MANIFEST = ROOT / "scripts/oss-test-env.manifest"
DEFAULT_ENVS = ROOT / "target/oss-envs"
DEFAULT_TEST_CMD = "{python} -m pytest -q -x"
CORE_SECTION_END = "# --- R-01"
LOG_TAIL_BYTES = 20_000


def read_manifest(path: Path, core_only: bool = False) -> list[dict[str, str]]:
    """Rows of a clone manifest. `core_only` stops at the R-01..R-07 block so
    only the pinned 20-project §17 set is returned."""
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if core_only and line.startswith(CORE_SECTION_END):
            break
        line = line.split("#", 1)[0].rstrip()
        if not line.strip():
            continue
        cols = line.split("\t")
        cols += [""] * (5 - len(cols))
        rows.append(
            {
                "slug": cols[0].strip(),
                "category": cols[1],
                "size": cols[2],
                "ref": cols[3],
                "url": cols[4],
            }
        )
    return rows


def read_recall(path: Path) -> list[tuple[str, Path]]:
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].rstrip()
        if not line.strip():
            continue
        slug, rel = line.split("\t", 1)
        p = Path(rel)
        out.append((slug.strip(), p if p.is_absolute() else ROOT / p))
    return out


def read_env_manifest(path: Path) -> dict[str, dict[str, str]]:
    """slug -> {pythonpath, install, test} from scripts/oss-test-env.manifest."""
    envs = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        slug, pythonpath, install, test = line.split("\t")
        envs[slug] = {
            "pythonpath": pythonpath,
            "install": install,
            "test": DEFAULT_TEST_CMD if test.strip() == "-" else test,
        }
    return envs


def read_lock(clones: Path) -> dict[str, dict[str, str]]:
    lock = clones / "clones.lock.tsv"
    rows: dict[str, dict[str, str]] = {}
    if not lock.is_file():
        return rows
    for line in lock.read_text(encoding="utf-8").splitlines()[1:]:
        cols = line.split("\t")
        if len(cols) == 4:
            rows[cols[0]] = {"ref": cols[1], "url": cols[2], "sha": cols[3]}
    return rows


def build(bin_path: Path) -> None:
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "--bin", "chokkin"],
        cwd=ROOT,
        check=True,
    )
    if not (bin_path.is_file() and os.access(bin_path, os.X_OK)):
        raise SystemExit(f"chokkin binary not found after build: {bin_path}")


def require_bin(bin_path: Path) -> None:
    if not (bin_path.is_file() and os.access(bin_path, os.X_OK)):
        raise SystemExit(f"chokkin binary not found: {bin_path} (use --build)")


def chokkin_raw(bin_path: Path, proj: Path, *extra: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(bin_path), "--reporter", "json", "--no-exit-code", *extra, str(proj)],
        capture_output=True,
        check=False,
    )


def chokkin_report(bin_path: Path, proj: Path, *extra: str) -> dict:
    out = chokkin_raw(bin_path, proj, *extra)
    return json.loads(out.stdout)


def chokkin_version(bin_path: Path) -> str:
    out = subprocess.run(
        [str(bin_path), "--version"], capture_output=True, text=True, check=False
    )
    return out.stdout.strip()


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def fresh_copy(src: Path, dest: Path) -> None:
    """Disposable copy of a clone; the clone itself is never modified."""
    shutil.rmtree(dest, ignore_errors=True)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(src, dest, symlinks=True, ignore=shutil.ignore_patterns(".chokkin"))


def reset_tree(work: Path) -> None:
    # Undo the mutation and anything a previous run wrote (caches, artifacts).
    subprocess.run(["git", "-C", str(work), "reset", "-q", "--hard"], check=True)
    subprocess.run(["git", "-C", str(work), "clean", "-q", "-fdx"], check=True)


def run_isolated(
    cmd: list[str],
    cwd: Path,
    timeout: int,
    log: Path,
    extra_env: dict[str, str] | None = None,
) -> tuple[str, int | None, float]:
    """Run untrusted code with a minimal environment.

    Returns (outcome, exit_code, seconds); outcome is ok | fail | timeout.
    """
    with tempfile.TemporaryDirectory(prefix="oracle-home-") as home:
        # No inherited tokens or credentials reach the analyzed project's code.
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": home,
            "TMPDIR": home,
            "LANG": "C.UTF-8",
            "PYTHONDONTWRITEBYTECODE": "1",
            **(extra_env or {}),
        }
        start = time.monotonic()
        with log.open("wb") as fh:
            proc = subprocess.Popen(
                cmd,
                cwd=cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=fh,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                code = proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
                return "timeout", None, time.monotonic() - start
        data = log.read_bytes()
        if len(data) > LOG_TAIL_BYTES:
            log.write_bytes(b"[... truncated ...]\n" + data[-LOG_TAIL_BYTES:])
        return ("ok" if code == 0 else "fail"), code, time.monotonic() - start


def md_table(header: list[str], rows: list[list], align: str = "") -> list[str]:
    """Markdown table lines; `align` is one char per column (l or r)."""
    align = align or "l" * len(header)
    sep = ["---:" if a == "r" else "---" for a in align]
    lines = ["| " + " | ".join(header) + " |", "|" + "|".join(sep) + "|"]
    lines += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return lines
