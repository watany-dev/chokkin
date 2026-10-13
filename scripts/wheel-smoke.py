#!/usr/bin/env python3
"""Install a built chokkin wheel and run the binary inside it (issue #834).

The Release workflow builds the wheels with the pinned toolchain and the `dist`
profile (fat LTO, opt-level "s", panic = "abort"); the CI tests run neither.
This installs one wheel into a fresh venv with pip, which also rejects a wheel
whose tags do not fit the running interpreter, then checks:

  version   `chokkin --version` prints the Cargo.toml version
  analyze   `chokkin --no-cache --reporter json` on a copy of
            tests/fixtures/wheel_smoke exits 1 and prints
            tests/fixtures/wheel_smoke.json (ignoring the "version" field).
            tests/cli_analyze.rs keeps that file equal to the tested build's
            output and valid against docs/schema/chokkin-report.schema.json.

Standard library only, so it also runs in emulated containers that have
nothing but Python.

Usage:
  scripts/wheel-smoke.py WHEEL

Exit status: 0 all checks pass, 1 a check failed.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import venv
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "tests/fixtures/wheel_smoke"
EXPECTED = ROOT / "tests/fixtures/wheel_smoke.json"
ISSUES_FOUND = 1


def cargo_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version = "([^"]+)"', text, re.MULTILINE)
    if match is None:
        sys.exit("Cargo.toml has no version")
    return match.group(1)


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(cmd), flush=True)
    # chokkin writes UTF-8; text=True alone decodes with cp1252 on Windows.
    return subprocess.run(cmd, capture_output=True, encoding="utf-8", errors="replace",
                          check=False, **kwargs)


def install(wheel: Path, env_dir: Path) -> Path:
    venv.create(env_dir, with_pip=True)
    bin_dir = env_dir / ("Scripts" if os.name == "nt" else "bin")
    python = bin_dir / ("python.exe" if os.name == "nt" else "python")
    done = run(
        [str(python), "-m", "pip", "install", "--no-index", "--no-deps",
         "--disable-pip-version-check", str(wheel)]
    )
    if done.returncode != 0:
        sys.exit(f"pip install failed:\n{done.stdout}{done.stderr}")
    return bin_dir / ("chokkin.exe" if os.name == "nt" else "chokkin")


def check_version(chokkin: Path, version: str) -> list[str]:
    done = run([str(chokkin), "--version"])
    if done.returncode != 0 or done.stdout.strip() != f"chokkin {version}":
        return [f"--version: exit {done.returncode}, stdout {done.stdout!r}, "
                f"stderr {done.stderr!r}; want 'chokkin {version}'"]
    return []


def check_analyze(chokkin: Path, version: str, work: Path) -> list[str]:
    project = work / "wheel_smoke"
    shutil.copytree(FIXTURE, project)
    done = run([str(chokkin), "--no-cache", "--reporter", "json", "."], cwd=project)
    if done.returncode != ISSUES_FOUND:
        return [f"analyze: exit {done.returncode}, want {ISSUES_FOUND}; "
                f"stderr {done.stderr!r}"]
    try:
        report = json.loads(done.stdout)
    except json.JSONDecodeError as err:
        return [f"analyze: stdout is not JSON ({err}): {done.stdout!r}"]
    errors = []
    if report.pop("version", None) != version:
        errors.append(f"analyze: report version is not {version}")
    expected = json.loads(EXPECTED.read_text(encoding="utf-8"))
    if report != expected:
        errors.append(
            "analyze: report differs from "
            f"{EXPECTED.relative_to(ROOT).as_posix()}:\n"
            + json.dumps(report, indent=2, sort_keys=True)
        )
    return errors


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    wheel = Path(sys.argv[1]).resolve()
    version = cargo_version()
    with tempfile.TemporaryDirectory(prefix="chokkin-wheel-smoke-") as tmp:
        work = Path(tmp)
        chokkin = install(wheel, work / "venv")
        errors = check_version(chokkin, version)
        errors += check_analyze(chokkin, version, work)
    for error in errors:
        print(f"FAIL {error}", file=sys.stderr)
    if not errors:
        print(f"ok {wheel.name}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
