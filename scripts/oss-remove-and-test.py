#!/usr/bin/env python3
"""CHK001 remove-and-test oracle over the pinned OSS corpus (issue #114, #85 WS2).

For every CHK001 (unused file) finding in a disposable copy of each cloned
project, delete the flagged file, run the project's configured test command,
and compare against a baseline run on the untouched tree.

THIS RUNS UNTRUSTED THIRD-PARTY TEST CODE. It is opt-in and never part of the
chokkin CLI/library pipeline, release jobs, or default CI. Without --execute it
is a dry run: chokkin analysis and test-command detection only, nothing from
the analyzed projects is executed. See docs/dev/chk001-remove-and-test.md for
the isolation this needs.

Usage:
  scripts/oss-remove-and-test.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  -o, --output DIR      Output directory (default: target/oss-oracle)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  --python PATH         Interpreter for test runs (default: python3). Point at a
                        venv you provisioned yourself; nothing is installed here.
  --wrap CMD            Prefix every test run (e.g. "unshare -rn" for no network)
  --timeout SECS        Per test run timeout (default: 600)
  --max-findings N      Cap delete-and-test runs per project; rest are not-run
  --projects a,b        Only these slugs
  --build               cargo build --release before running
  --execute             Actually run project tests (otherwise dry run)
  -h, --help            Show help

Test command detection (first match wins; otherwise `no-test-command`):
  pyproject.toml [tool.pytest.ini_options], pytest.ini, setup.cfg [tool:pytest],
  tox.ini [pytest]  ->  <python> -m pytest -q -x -p no:cacheprovider
tox/nox are not used: they install dependencies.

Per-finding status:
  pass           baseline passed and the suite still passes after deletion
  break          baseline passed, suite fails after deletion, and a baseline
                 re-run passes again (so the failure is attributed to deletion)
  baseline-fail  the untouched tree already fails (or times out), or the
                 baseline re-run after a post-delete failure fails (flaky)
  not-run        no-test-command, dry-run, or over --max-findings

Outputs (under --output):
  results.tsv     one row per CHK001 finding
  summary.json    per-project and per-rule counts, corpus revisions, command
  report.md       human-readable summary
  logs/<slug>/    test run output
"""

from __future__ import annotations

import argparse
import configparser
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RULE = "CHK001"
STATUSES = ("pass", "break", "baseline-fail", "not-run")
LOG_TAIL_BYTES = 20_000


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument(
        "-m", "--manifest", type=Path, default=ROOT / "scripts/oss-clones.manifest"
    )
    p.add_argument("-c", "--clones", type=Path, default=ROOT / "target/oss-clones")
    p.add_argument("-o", "--output", type=Path, default=ROOT / "target/oss-oracle")
    p.add_argument("-b", "--bin", type=Path, default=ROOT / "target/release/chokkin")
    p.add_argument("--python", default="python3")
    p.add_argument("--wrap", default="")
    p.add_argument("--timeout", type=int, default=600)
    p.add_argument("--max-findings", type=int, default=0)
    p.add_argument("--projects", default="")
    p.add_argument("--build", action="store_true")
    p.add_argument("--execute", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def read_manifest(path: Path) -> list[str]:
    slugs = []
    for line in path.read_text(encoding="utf-8").splitlines():
        slug = line.split("\t", 1)[0].split("#", 1)[0].strip()
        if slug:
            slugs.append(slug)
    return slugs


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


def has_pytest_config(proj: Path) -> bool:
    pyproject = proj / "pyproject.toml"
    if pyproject.is_file() and "[tool.pytest.ini_options]" in pyproject.read_text(
        encoding="utf-8", errors="replace"
    ):
        return True
    if (proj / "pytest.ini").is_file():
        return True
    for name, section in (("setup.cfg", "tool:pytest"), ("tox.ini", "pytest")):
        cfg_path = proj / name
        if not cfg_path.is_file():
            continue
        cfg = configparser.RawConfigParser(strict=False, interpolation=None)
        try:
            cfg.read(cfg_path, encoding="utf-8")
        except configparser.Error:
            continue
        if cfg.has_section(section):
            return True
    return False


def chk001_paths(bin_path: Path, proj: Path) -> list[str]:
    out = subprocess.run(
        [str(bin_path), "--reporter", "json", "--no-exit-code", str(proj)],
        capture_output=True,
        text=True,
        check=False,
    )
    report = json.loads(out.stdout)
    return sorted(
        {i["path"] for i in report.get("issues", []) if i.get("code") == RULE}
    )


def reset_tree(work: Path) -> None:
    # Undo the deletion and anything the test run wrote (caches, artifacts).
    subprocess.run(["git", "-C", str(work), "reset", "-q", "--hard"], check=True)
    subprocess.run(["git", "-C", str(work), "clean", "-q", "-fdx"], check=True)


def run_tests(
    cmd: list[str], work: Path, timeout: int, log: Path
) -> tuple[str, int | None, float]:
    """Return (outcome, exit_code, seconds); outcome is ok | fail | timeout."""
    with tempfile.TemporaryDirectory(prefix="oracle-home-") as home:
        # Minimal environment: no inherited tokens or credentials reach the tests.
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": home,
            "TMPDIR": home,
            "LANG": "C.UTF-8",
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        start = time.monotonic()
        with log.open("wb") as fh:
            proc = subprocess.Popen(
                cmd,
                cwd=work,
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
        _truncate_head(log)
        return ("ok" if code == 0 else "fail"), code, time.monotonic() - start


def _truncate_head(log: Path) -> None:
    data = log.read_bytes()
    if len(data) > LOG_TAIL_BYTES:
        log.write_bytes(b"[... truncated ...]\n" + data[-LOG_TAIL_BYTES:])


def measure_project(
    slug: str, args: argparse.Namespace, rows: list[dict], projects: dict
) -> None:
    src = args.clones / slug
    work = args.output / "work" / slug
    logs = args.output / "logs" / slug
    shutil.rmtree(work, ignore_errors=True)
    shutil.rmtree(logs, ignore_errors=True)
    logs.mkdir(parents=True)
    shutil.copytree(src, work, symlinks=True)

    info: dict = {"findings": 0, "test_command": None, "baseline": None}
    projects[slug] = info
    try:
        paths = chk001_paths(args.bin, work)
    except (json.JSONDecodeError, KeyError) as err:
        info["error"] = f"chokkin output unreadable: {err}"
        print(f"    {info['error']}", file=sys.stderr)
        shutil.rmtree(work, ignore_errors=True)
        return
    info["findings"] = len(paths)
    print(f"==> {slug}: {len(paths)} {RULE} finding(s)", flush=True)

    def record(
        path: str,
        status: str,
        detail: str,
        post_exit: int | None = None,
        secs: float = 0.0,
    ) -> None:
        rows.append(
            {
                "slug": slug,
                "rule": RULE,
                "path": path,
                "status": status,
                "detail": detail,
                "post_exit": post_exit,
                "seconds": round(secs, 2),
            }
        )

    cmd = shlex.split(args.wrap) + [
        args.python,
        "-m",
        "pytest",
        "-q",
        "-x",
        "-p",
        "no:cacheprovider",
    ]
    if has_pytest_config(work):
        info["test_command"] = shlex.join(cmd)
    if not paths:
        shutil.rmtree(work, ignore_errors=True)
        return
    if info["test_command"] is None:
        for p in paths:
            record(p, "not-run", "no-test-command")
        shutil.rmtree(work, ignore_errors=True)
        return
    if not args.execute:
        for p in paths:
            record(p, "not-run", "dry-run")
        shutil.rmtree(work, ignore_errors=True)
        return

    reset_tree(work)
    outcome, code, secs = run_tests(cmd, work, args.timeout, logs / "baseline.log")
    info["baseline"] = {"outcome": outcome, "exit": code, "seconds": round(secs, 2)}
    print(f"    baseline: {outcome} (exit {code}, {secs:.1f}s)", flush=True)
    if outcome != "ok":
        detail = "baseline-timeout" if outcome == "timeout" else f"baseline-exit-{code}"
        for p in paths:
            record(p, "baseline-fail", detail)
        shutil.rmtree(work, ignore_errors=True)
        return

    for n, path in enumerate(paths):
        if args.max_findings and n >= args.max_findings:
            record(path, "not-run", "over-max-findings")
            continue
        reset_tree(work)
        (work / path).unlink(missing_ok=True)
        outcome, code, secs = run_tests(cmd, work, args.timeout, logs / f"{n:04d}.log")
        if outcome == "ok":
            record(path, "pass", "", code, secs)
            continue
        # Confirm the failure is caused by the deletion, not flakiness.
        reset_tree(work)
        again, _, _ = run_tests(
            cmd, work, args.timeout, logs / f"{n:04d}.rebaseline.log"
        )
        detail = "post-timeout" if outcome == "timeout" else f"post-exit-{code}"
        if again == "ok":
            record(path, "break", detail, code, secs)
        else:
            record(path, "baseline-fail", f"flaky-baseline;{detail}", code, secs)
        print(f"    {rows[-1]['status']}: {path} ({detail})", flush=True)
    shutil.rmtree(work, ignore_errors=True)


def counts(rows: list[dict]) -> dict[str, int]:
    c = {s: 0 for s in STATUSES}
    for r in rows:
        c[r["status"]] += 1
    c["total"] = len(rows)
    return c


def write_outputs(
    args: argparse.Namespace, rows: list[dict], projects: dict, meta: dict
) -> None:
    out = args.output
    with (out / "results.tsv").open("w", encoding="utf-8") as fh:
        fh.write("slug\trule\tpath\tstatus\tdetail\tpost_exit\tseconds\n")
        for r in rows:
            fh.write(
                f"{r['slug']}\t{r['rule']}\t{r['path']}\t"
                f"{r['status']}\t{r['detail']}\t{'' if r['post_exit'] is None else r['post_exit']}\t"
                f"{r['seconds']}\n"
            )

    for slug, info in projects.items():
        info["counts"] = counts([r for r in rows if r["slug"] == slug])
    summary = {
        **meta,
        "rules": {RULE: counts(rows)},
        "projects": projects,
    }
    (out / "summary.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8"
    )

    lines = [
        f"# {RULE} remove-and-test oracle",
        "",
        f"- chokkin: `{meta['chokkin_version']}`",
        f"- python: `{meta['python']}`",
        f"- generated: {meta['generated']}",
        f"- mode: {'execute' if meta['execute'] else 'dry-run'}",
        f"- command: `{meta['command']}`",
        "",
        "| Rule | Total | pass | break | baseline-fail | not-run |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    t = summary["rules"][RULE]
    lines.append(
        f"| {RULE} | {t['total']} | {t['pass']} | {t['break']} | {t['baseline-fail']} | {t['not-run']} |"
    )
    lines += [
        "",
        "| Project | SHA | Test command | Baseline | Total | pass | break | baseline-fail | not-run |",
        "|---|---|---|---|---:|---:|---:|---:|---:|",
    ]
    for slug, info in projects.items():
        c = info["counts"]
        base = info["baseline"]
        base_s = "-" if base is None else f"{base['outcome']} (exit {base['exit']})"
        sha = meta["corpus"].get(slug, {}).get("sha", "?")[:12]
        cmd = "no-test-command" if info["test_command"] is None else "pytest"
        lines.append(
            f"| {slug} | `{sha}` | {cmd} | {base_s} | {c['total']} | {c['pass']} | "
            f"{c['break']} | {c['baseline-fail']} | {c['not-run']} |"
        )
    breaks = [r for r in rows if r["status"] == "break"]
    lines += ["", "## Breaks", ""]
    if breaks:
        lines += ["| Project | Path | Detail |", "|---|---|---|"]
        lines += [f"| {r['slug']} | `{r['path']}` | {r['detail']} |" for r in breaks]
    else:
        lines.append("_None._")
    (out / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    if args.build:
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "--bin", "chokkin"],
            cwd=ROOT,
            check=True,
        )
    if not (args.bin.is_file() and os.access(args.bin, os.X_OK)):
        print(f"chokkin binary not found: {args.bin} (use --build)", file=sys.stderr)
        return 2
    if not args.manifest.is_file():
        print(f"manifest not found: {args.manifest}", file=sys.stderr)
        return 2
    args.output = args.output.resolve()
    if os.sep in args.python:
        # Tests run with cwd inside the working copy.
        args.python = str(Path(args.python).resolve())
    args.output.mkdir(parents=True, exist_ok=True)

    only = {s for s in args.projects.split(",") if s}
    lock = read_lock(args.clones)
    rows: list[dict] = []
    projects: dict = {}
    skipped: list[str] = []
    for slug in read_manifest(args.manifest):
        if only and slug not in only:
            continue
        if not (args.clones / slug).is_dir():
            print(f"skip (not cloned): {slug}", file=sys.stderr)
            skipped.append(slug)
            continue
        measure_project(slug, args, rows, projects)
    shutil.rmtree(args.output / "work", ignore_errors=True)

    if not projects:
        print("no projects measured — run clone-oss-fixtures.sh first", file=sys.stderr)
        return 2

    version = subprocess.run(
        [str(args.bin), "--version"], capture_output=True, text=True, check=False
    )
    py = (
        subprocess.run(
            [args.python, "--version"], capture_output=True, text=True, check=False
        )
        if shutil.which(args.python)
        else None
    )
    meta = {
        "chokkin_version": version.stdout.strip(),
        "python": (py.stdout or py.stderr).strip()
        if py
        else f"{args.python} (not found)",
        "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "execute": args.execute,
        "command": shlex.join([Path(sys.argv[0]).name, *sys.argv[1:]]),
        "timeout_seconds": args.timeout,
        "corpus": {s: lock[s] for s in projects if s in lock},
        "skipped_not_cloned": skipped,
    }
    write_outputs(args, rows, projects, meta)
    print(Path(args.output / "report.md").read_text(encoding="utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
