#!/usr/bin/env python3
"""Provision per-project test virtualenvs for the remove-and-test oracle (#338).

For every project in scripts/oss-test-env.manifest that is cloned, create
target/oss-envs/<slug> with `uv venv` and install the project plus its test
requirements with `uv pip install`, running in the pristine clone; the
git-ignored build outputs that leaves behind are removed afterwards
(`git clean -fdX`) so the clone stays as pinned.

THIS BUILDS AND INSTALLS UNTRUSTED THIRD-PARTY PROJECTS (their build backends
run). Use the same throwaway, credential-free runner as `make oss-oracle`; it
needs network access to PyPI, while the oracle's test runs do not.

Resolution is pinned in time: `uv pip install --exclude-newer` is set to the
commit date of the clone's HEAD, so unpinned test requirements resolve to what
was current at the pinned tag (a newer click/pytest otherwise breaks the
baseline of several projects).

Usage:
  scripts/oss-provision-envs.py [OPTIONS]

Options:
  -e, --env-manifest PATH  default: scripts/oss-test-env.manifest
  -c, --clones DIR         default: target/oss-clones
  -o, --output DIR         default: target/oss-envs
  --python VERSION         interpreter for `uv venv` (default: 3.11)
  --projects a,b           only these slugs
  --force                  recreate existing venvs
  -h, --help               show help

Writes <output>/provision.tsv (slug, status, seconds, python) and per-project
logs; exits 1 if any install failed (the others are still usable).
"""

from __future__ import annotations

import argparse
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

import oss_corpus as oc


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-e", "--env-manifest", type=Path, default=oc.DEFAULT_ENV_MANIFEST)
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("-o", "--output", type=Path, default=oc.DEFAULT_ENVS)
    p.add_argument("--python", default="3.11")
    p.add_argument("--projects", default="")
    p.add_argument("--force", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def commit_date(clone: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(clone), "log", "-1", "--format=%cI"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()


def main() -> int:
    args = parse_args()
    if shutil.which("uv") is None:
        print("uv is required (https://docs.astral.sh/uv/)", file=sys.stderr)
        return 2
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    only = {s for s in args.projects.split(",") if s}
    rows = []
    for slug, env in oc.read_env_manifest(args.env_manifest).items():
        if only and slug not in only:
            continue
        clone = args.clones / slug
        if not clone.is_dir():
            print(f"skip (not cloned): {slug}", file=sys.stderr)
            continue
        venv = args.output / slug
        log = args.output / f"{slug}.log"
        if (venv / "bin/python").exists() and not args.force:
            print(f"==> {slug}: exists (use --force to recreate)")
            rows.append([slug, "exists", "0", ""])
            continue
        shutil.rmtree(venv, ignore_errors=True)
        pin = ["--exclude-newer", commit_date(clone)]
        print(f"==> {slug}: {' '.join(pin)} {env['install']}", flush=True)
        start = time.monotonic()
        with log.open("w", encoding="utf-8") as fh:
            ok = (
                subprocess.run(
                    ["uv", "venv", "-q", "--python", args.python, str(venv)],
                    stdout=fh, stderr=subprocess.STDOUT, check=False,
                ).returncode == 0
                and subprocess.run(
                    ["uv", "pip", "install", "--python", str(venv / "bin/python"),
                     *pin, *shlex.split(env["install"])],
                    cwd=clone, stdout=fh, stderr=subprocess.STDOUT, check=False,
                ).returncode == 0
            )
        # The build backend wrote build/, *.egg-info, _version.py and the like
        # into the clone; drop those (git-ignored) outputs so later static runs
        # and the differential tools see the clone exactly as pinned.
        subprocess.run(["git", "-C", str(clone), "clean", "-q", "-fdX"], check=False)
        if not ok:
            shutil.rmtree(venv, ignore_errors=True)  # so the next run retries it
        secs = time.monotonic() - start
        status = "ok" if ok else "install-failed"
        py = subprocess.run([str(venv / "bin/python"), "--version"], capture_output=True,
                            text=True, check=False).stdout.strip() if ok else ""
        print(f"    {status} ({secs:.0f}s){'' if ok else f' — see {log}'}", flush=True)
        rows.append([slug, status, f"{secs:.0f}", py])
    with (args.output / "provision.tsv").open("w", encoding="utf-8") as fh:
        fh.write("slug\tstatus\tseconds\tpython\n")
        fh.writelines("\t".join(r) + "\n" for r in rows)
    return 1 if any(r[1] == "install-failed" for r in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
