# Remove-and-Test Oracle: CHK001 / CHK006 (#338, #339)

Date: 2026-10-04  
chokkin: `0.6.0` (local `cargo build --release`)  
Test interpreters: per-project venvs under `target/oss-envs/` (CPython 3.11.15,
`uv 0.8.17`), from `scripts/oss-provision-envs.py`  
Harness: `scripts/oss-remove-and-test.py` (`make oss-oracle`)  
Related: parent #346 / #85 WS2, first slice #114, summary in
`docs/dev/oss-validation-report.md`

## Summary

The oracle now produces real numbers. Every project baseline passes offline.
"Precision" below means pass / (pass + break) over the findings that reached a
post-removal test run.

| Rule | Projects | Total | Tested | pass | break | baseline-fail | not-run | Precision |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| CHK001 | 19 (no django) | 226 | 50 | 28 | 22 | 0 | 176 (sampled-out) | **56.0%** |
| CHK006 | see [CHK006](#chk006-symbol-removal-339) | | | | | | | |

The 22 CHK001 breaks fall into three causes, none of them an import chokkin
failed to see:

| Cause | Breaks | Projects | Evidence |
|---|---:|---|---|
| Public modules of a library classified as **app** mode | 7 | fastapi | `fastapi/middleware/*.py`, `staticfiles.py`, `templating.py`: 0/7 pass |
| Test data read as files (not imported) | 7 | black | `read_data("cases", "pep_572")`, `Path(DATA_DIR / "nested_gitignore_tests")` |
| Modules loaded by path or import string | 8 | werkzeug, uvicorn | `tests/live_apps/*` started in a subprocess; `tests/importer/*` passed to `import_from_string("tests.importer...")` |

So the 56% is a floor on "chokkin's import graph is right". It is not a
measure of how often deleting a flagged file is safe. What the oracle can show
is where chokkin's model of "used" (static imports from entry roots) is
narrower than the project's.

## Corpus and per-project results

The pinned `scripts/oss-clones.manifest` set. SHAs are from
`target/oss-clones/clones.lock.tsv`. Black is sampled to 30 findings with
`--sample 30`, chosen by a stable hash of the target. Every other project ran
all of its findings.

| Project | SHA | Baseline | CHK001 | pass | break | not-run | Precision |
|---|---|---|---:|---:|---:|---:|---:|
| requests | `0e322af87745` | — | 0 | | | | n/a |
| urllib3 | `2458bfcd3dac` | — | 0 | | | | n/a |
| click | `874ca2bc1c30` | — | 0 | | | | n/a |
| jinja | `dd4a8b5466d8` | — | 0 | | | | n/a |
| werkzeug | `b933ccb1f5ea` | ok (19.9s) | 6 | 1 | 5 | 0 | 16.7% |
| flask | `c12a5d874c5a` | ok (3.6s) | 3 | 3 | 0 | 0 | 100.0% |
| httpx | `609df7ecc0f7` | — | 0 | | | | n/a |
| starlette | `8d0cff820f89` | — | 0 | | | | n/a |
| uvicorn | `7dc027d5fb98` | ok (83.9s) | 4 | 1 | 3 | 0 | 25.0% |
| attrs | `6771a0489378` | — | 0 | | | | n/a |
| anyio | `8cce74917ffc` | — | 0 | | | | n/a |
| python-dotenv | `d6c0b9638349` | — | 0 | | | | n/a |
| tenacity | `31fe2d0cf250` | — | 0 | | | | n/a |
| structlog | `42fca8c440d4` | — | 0 | | | | n/a |
| pluggy | `f8aa4a009716` | — | 0 | | | | n/a |
| typer | `88aefd449269` | — | 0 | | | | n/a |
| black | `b965c2a5026f` | ok (54.7s) | 206 | 23 | 7 | 176 | 76.7% |
| fastapi | `40e33e492dbf` | ok (43.9s) | 7 | 0 | 7 | 0 | 0.0% |
| djangorestframework | `c7a7eae55152` | — | 0 | | | | n/a |
| django | `1e1d791787e2` | not run | 988 | | | | — ([below](#django)) |

These counts are at the default confidence (`likely`). In library mode CHK001
is capped at `maybe`, so 15 of the 20 projects report no CHK001 at all by
default. The oracle measures what users actually see.

The 0.4.0 run had 1265 findings. This run has 226 plus django's 988. The fall
comes from the work between 0.4.0 and 0.6.0: httpx 4 → 0, werkzeug 7 → 6,
fastapi 9 → 7, black 207 → 206, django 1026 → 988.

## Public-API candidates (triage)

The 0.4.0 report listed four shipped modules that looked like public API:

| Module | 0.6.0 | Oracle | Verdict |
|---|---|---|---|
| `fastapi/middleware/*.py` (+ `staticfiles.py`, `templating.py`) | reported (`error`, likely) | **break** 7/7 | **False positive.** These are re-export shims (`from starlette.middleware.cors import CORSMiddleware as CORSMiddleware`) that users and fastapi's own tests import. fastapi ships a `console_scripts` entry, so `mode = "auto"` resolves to **app**. In app mode the package is not a public surface and CHK001 is uncapped. |
| `httpx/_api.py` | not reported | — | No longer reported in 0.6.0. `httpx/__init__.py` star-imports it (`from ._api import *`), and that edge now resolves. |
| `werkzeug/local.py` | not reported | — | No longer reported in 0.6.0. `tests/test_local.py` imports it as a submodule (`from werkzeug import local`), and that edge now resolves. The werkzeug finding that remains in `src/` is `werkzeug/testapp.py`, which users run as `python -m werkzeug.testapp`. It **passes** only because no test imports it. |
| `uvicorn/workers.py` | reported | **pass** | **False positive in practice.** Users reference the gunicorn worker class as the string `-k uvicorn.workers.UvicornWorker`, and uvicorn's suite never imports it, so deletion passes. uvicorn also resolves to app mode through its `console_scripts`. |

The common root cause: **a library that also ships a CLI is classified as an
app**, so its importable modules are judged by app reachability. Follow-up:
treat `[project.scripts]` together with an importable top-level package as
library (or "library + entry") in `auto` mode, or at least cap CHK001
confidence for modules inside the distributed package. The oracle cannot catch
`uvicorn/workers.py` and `werkzeug/testapp.py`, because a `pass` only means the
project's *own* suite does not need the file.

## Django

Django stays `not-run` in the committed numbers. The env and command exist in
`scripts/oss-test-env.manifest` (`{python} tests/runtests.py --noinput
--failfast --parallel 1`), so the decision can be revisited with more compute:

- **Cost.** One offline baseline is about 6 minutes (`Ran 17384 tests in
  315s` plus startup) with `--parallel 1`. 988 CHK001 findings × 6 min is about
  100 CPU-hours. Even `--sample 30` is three hours for one project.
- **Baseline does not pass in the sandbox.** Four tests fail on the untouched
  tree for environmental reasons. Two are root-only chmod tests:
  `file_uploads...test_readonly_root` and
  `template_tests...test_permissions_error`. Two depend on the CPython patch
  release's `html.parser`: `utils_tests...test_strip_tags` and
  `test_utils...test_parsing_errors`. They would need deselecting through
  runtests' `--exclude-tag`/label lists before a break can be told apart from
  the environment.
- **Expected yield is low.** 742 of the 988 findings are under `tests/`, where
  Django discovers test modules by label, which is the same dynamic-loading
  class as werkzeug and uvicorn above. The 246 under `django/` are mostly
  modules that Django loads by name:
  - locale `formats.py`: 84, loaded via `import_module(f"...{lang}.formats")`
  - `contrib.gis` backends: 35
  - management commands: found via `pkgutil`
  - db, cache, mail and session backends: named by dotted paths in settings

  The plugin route for string module references (spec §9) is the fix to
  pursue, not more oracle runs.

## Method

1. `scripts/oss-provision-envs.py` (`make oss-envs`) creates
   `target/oss-envs/<slug>` with `uv venv`. It then installs the project plus
   its test requirements (`install` column of `scripts/oss-test-env.manifest`)
   from the pristine clone, using `uv pip install --exclude-newer <HEAD commit
   date>`, so unpinned requirements resolve to what existed at the pinned tag.
   Ignored build outputs are cleaned afterwards with `git clean -fdX`.
2. For each project, copy the clone to `target/oss-oracle/<rule>/work/<slug>.<k>`
   (`--jobs` copies) and run chokkin to collect findings.
3. Choose the test command. That is the manifest's `test` column. Otherwise it
   is `<python> -m pytest -q -x` when a pytest configuration exists, or
   `no-test-command` when none does. The working copy is put first on
   `PYTHONPATH` (`pythonpath` column), so the mutated tree shadows the
   installed package.
4. Run a **baseline** on the untouched copy. If it fails, every finding is
   `baseline-fail`.
5. For each finding: `git reset --hard` and `git clean -fdx`, remove the
   target, and run the tests. If they pass, the finding is `pass`. A failure
   triggers a baseline re-run: if the re-run passes the finding is `break`,
   otherwise `baseline-fail (flaky-baseline)`.
6. `--sample N` picks N findings per project by a stable hash of the target.
   The others are `not-run (sampled-out)`, so reruns test the same subset.

Per-project deselections in the manifest drop only tests that fail on the
**untouched** tree in the offline sandbox. Examples: chmod tests that pass as
root, tests that need an external host or IPv6, and type-checker golden output.
The manifest comments list each one with its reason.

## Isolation

`--execute` runs **untrusted third-party test code**, and provisioning runs
their **build backends**.

- **Never in release or default CI.** No workflow calls these scripts. The
  chokkin CLI and library never execute analyzed code.
- Use a throwaway VM or container without credentials. Test runs get a minimal
  environment (`PATH`, a temporary `HOME`/`TMPDIR`, `LANG`,
  `PYTHONDONTWRITEBYTECODE`).
- `--offline` runs every test in a fresh user and network namespace
  (`unshare -rn`). `scripts/oss_netns_exec.py` brings up loopback only, so
  servers the tests start on 127.0.0.1 work, but nothing leaves the host.
  Provisioning is the only step that needs network (PyPI).

## Commands

```bash
make oss-clones
make oss-envs                                   # once; needs PyPI
make oss-oracle ARGS="--jobs 3 --sample 30"     # CHK001 -> target/oss-oracle/chk001/
make oss-oracle ARGS="--rule CHK006 --jobs 3 --sample 15"   # -> target/oss-oracle/chk006/
```

`make oss-oracle` adds `--build --envs target/oss-envs --offline --execute`.
For the numbers above, django was left out with `--projects`, which listed the
other 19 slugs.

Outputs (in `target/oss-oracle/<rule>/`, not committed): `results.tsv` (one row
per finding: slug, rule, target, status, detail, post_exit, seconds, strategy),
`summary.json`, `report.md`, `logs/<slug>/`.

## CHK006: symbol removal (#339)

Run in progress (`make oss-oracle ARGS="--rule CHK006 --jobs 3 --sample 15"`); results will be added here.

## Limitations

- **`pass` is a lower bound on harm.** It means the project's own suite does
  not need the file or symbol. Downstream users of a library module or a
  string-referenced class (`uvicorn.workers.UvicornWorker`) are invisible to
  it.
- **`break` is not always a chokkin bug.** Files read as data (black's
  `tests/data`) or launched by path are "unused" in chokkin's import sense
  while the suite still depends on them. Exclude such data directories in
  `[tool.chokkin]` (`exclude = ["tests/data/**"]`).
- `-x` and one shared baseline per project: the oracle compares pass/fail,
  not individual test IDs.
- Sampling: black (206 findings) and every CHK006 project are sampled, so their
  precision carries sampling error (±~15 pp at n = 30).
