# CHK001 Remove-and-Test Oracle (#85 WS2, first slice)

Date: 2026-09-25  
chokkin: `0.4.0` (PyPI wheel `chokkin-0.4.0-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl`,
sha256 `7fed29bfb0843c3fffab58c8d51a67018d8546578a48a7a238c466e1b5b0ca14`)  
Test interpreter: host `python3` (CPython 3.11.15, **pytest not installed**)  
Harness: `scripts/oss-remove-and-test.py` (`make oss-oracle`)  
Related: issue #114, parent #85, WS1 baseline `docs/dev/v0.3-stocktake-coverage.md`

## Summary

The harness works end to end, but **this run did not measure real-world CHK001
precision**. Every finding landed in `baseline-fail` or `not-run`, so no
finding reached a post-deletion test run:

| Rule | Total | pass | break | baseline-fail | not-run |
|---|---:|---:|---:|---:|---:|
| CHK001 | 1265 | 0 | 0 | 239 | 1026 |

- **239 baseline-fail**: the 7 projects with a pytest configuration fail their
  untouched baseline with `No module named pytest`. The first iteration does not
  install dependencies (#114 safety constraint), so no deletion was evaluated.
  The harness does not count these as breaks.
- **1026 not-run**: all Django findings. Django has no pytest configuration
  (it uses `tests/runtests.py`), so it gets `no-test-command`. The harness does
  not invent a command for it.
- The CHK001 total (1265) matches the WS1 stocktake count.

Deletion-caused `break` and deletion-safe `pass` rates are still **unmeasured**.
Getting them needs a pre-provisioned test environment (see
[Next run](#next-run-producing-real-numbers)).

## Corpus

This run used the pinned `scripts/oss-clones.manifest`. All 20 clones
succeeded, and `target/oss-clones/clones.lock.tsv` recorded their SHAs.

| Project | Ref | SHA | Test command | Baseline | CHK001 | Status |
|---|---|---|---|---|---:|---|
| requests | v2.32.3 | `0e322af87745` | pytest | fail (exit 1) | 3 | baseline-fail 3 |
| urllib3 | 2.2.3 | `2458bfcd3dac` | pytest | — | 0 | — |
| click | 8.1.7 | `874ca2bc1c30` | pytest | — | 0 | — |
| jinja | 3.1.4 | `dd4a8b5466d8` | pytest | — | 0 | — |
| werkzeug | 3.0.4 | `b933ccb1f5ea` | pytest | fail (exit 1) | 7 | baseline-fail 7 |
| flask | 3.0.3 | `c12a5d874c5a` | pytest | fail (exit 1) | 3 | baseline-fail 3 |
| httpx | 0.27.2 | `609df7ecc0f7` | pytest | fail (exit 1) | 4 | baseline-fail 4 |
| starlette | 0.38.6 | `8d0cff820f89` | pytest | — | 0 | — |
| uvicorn | 0.30.6 | `7dc027d5fb98` | pytest | fail (exit 1) | 6 | baseline-fail 6 |
| attrs | 24.2.0 | `6771a0489378` | pytest | — | 0 | — |
| anyio | 4.6.0 | `8cce74917ffc` | pytest | — | 0 | — |
| python-dotenv | v1.0.1 | `d6c0b9638349` | pytest | — | 0 | — |
| tenacity | 8.5.0 | `31fe2d0cf250` | pytest | — | 0 | — |
| structlog | 24.4.0 | `42fca8c440d4` | pytest | — | 0 | — |
| pluggy | 1.5.0 | `f8aa4a009716` | pytest | — | 0 | — |
| typer | 0.12.5 | `88aefd449269` | pytest | — | 0 | — |
| black | 24.8.0 | `b965c2a5026f` | pytest | fail (exit 1) | 207 | baseline-fail 207 |
| fastapi | 0.115.0 | `40e33e492dbf` | pytest | fail (exit 1) | 9 | baseline-fail 9 |
| djangorestframework | 3.15.2 | `c7a7eae55152` | pytest | — | 0 | — |
| django | 5.1.1 | `1e1d791787e2` | no-test-command | — | 1026 | not-run 1026 |

The harness runs a baseline only for projects that have at least one CHK001
finding.

Where the findings are: Django has 747 under `tests/` and 279 under `django/`.
Black has 205 under `tests/` and 2 under `src/`. The other 32 are mostly test
helpers (`tests/utils.py`, `tests/live_apps/*`). A few are shipped modules, for
example `fastapi/middleware/*.py`, `httpx/_api.py`, `werkzeug/local.py` and
`uvicorn/workers.py`. These look like public-API files that users import, so
they are likely CHK001 false positives in library mode. The oracle cannot
confirm that until the baselines pass.

## Commands

```bash
scripts/clone-oss-fixtures.sh            # = make oss-clones
scripts/oss-remove-and-test.py \
  --bin <chokkin 0.4.0 from the PyPI wheel> \
  --execute --wrap "unshare -rn"
```

The PyPI binary was used because this environment could not reach crates.io,
so `cargo build` was not possible. With a local build, the equivalent one-shot
command is `make oss-oracle ARGS='--wrap "unshare -rn"'`.

Outputs (in `target/oss-oracle/`, generated and not committed):
`results.tsv` (one row per finding: slug, rule, path, status, detail,
post_exit, seconds), `summary.json` (per-rule and per-project counts, corpus
SHAs, command, chokkin/python versions, timeout), `report.md` and
`logs/<slug>/`.

## Method

For each project in the manifest:

1. Copy the clone to `target/oss-oracle/work/<slug>` (the clone itself is never
   modified) and run chokkin on the copy to collect CHK001 paths.
2. Detect the test command. It is `<python> -m pytest -q -x -p no:cacheprovider`
   when `pyproject.toml [tool.pytest.ini_options]`, `pytest.ini`,
   `setup.cfg [tool:pytest]` or `tox.ini [pytest]` exists. Otherwise the result
   is `no-test-command`. tox and nox are never used because they install
   dependencies.
3. Run the test command once on the untouched tree (**baseline**). If it fails
   or times out, every finding in that project is `baseline-fail`.
4. If the baseline passes, then for each finding: run `git reset --hard` and
   `git clean -fdx`, delete the file, and run the tests. A pass is `pass`. A
   failure or timeout triggers a baseline re-run. The result is `break` only if
   the re-run passes again; otherwise it is `baseline-fail`
   (`flaky-baseline;…`). A failure therefore counts as deletion-caused only
   when the same tree without the deletion passes both before and after it.
5. Remove the working copy.

Without `--execute`, the harness does a dry run: it runs the analysis and
detects test commands, but executes nothing from the analyzed projects.

The status logic was checked against a synthetic project with stand-in test
commands: one command fails when a specific flagged file is missing, one is
flaky and one hangs. They produced `break`/`pass`, `flaky-baseline` and
`baseline-timeout` respectively, and timeouts killed the whole process group.

## Isolation requirements

`--execute` runs **untrusted third-party test code**.

- **Never in release or default CI.** No workflow calls `oss-remove-and-test.py`
  or `make oss-oracle`. The chokkin CLI and library never execute analyzed code.
- Run it on a throwaway VM or container with no credentials. Test runs get a
  minimal environment (`PATH`, a temporary `HOME`/`TMPDIR`, `LANG`,
  `PYTHONDONTWRITEBYTECODE`), so tokens in the caller's environment are not
  passed through. Files the process can reach are still exposed.
- Use `--wrap` for network or filesystem isolation, e.g. `--wrap "unshare -rn"`
  (no network, used for this run) or a `bwrap`/container invocation.
- Nothing is installed. For real numbers, point `--python` at a virtualenv you
  provisioned yourself inside the sandbox.

## Limitations

- **No dependencies, so no signal.** This run gives no information about
  CHK001 precision.
- Only pytest configurations are detected. Django's `runtests.py` and custom
  `Makefile`/`nox` targets are reported as `no-test-command` instead of
  guessed.
- One shared baseline per project. `-x` stops at the first failure, so the
  harness compares pass/fail, not individual test IDs. A partially failing
  baseline makes the whole project `baseline-fail`.
- Deleting a file that the test suite does not import (for example an unused
  test helper) can only produce `pass`. A `pass` means the suite does not
  depend on the file. It does not prove that downstream users do not import it,
  which matters for library-mode public modules.
- The cost is one full test run per finding: 207 runs for Black and 1026 for
  Django if a runner is added. Use `--max-findings N` to cap it; findings over
  the cap are `not-run` (`over-max-findings`).

## Next run (producing real numbers)

In an isolated runner:

```bash
make oss-clones
python3 -m venv /sandbox/venv
# provision per-project test deps by hand, e.g. pip install -e <clone>[test] pytest
make oss-oracle ARGS='--python /sandbox/venv/bin/python --wrap "unshare -rn" --projects requests,flask,httpx,uvicorn,werkzeug,fastapi'
```

The small projects (32 findings in total) are the cheapest place to see
whether the oracle separates `pass` from `break` before scaling to Black and
Django.

## Follow-ups (out of scope for #114)

- **CHK006 (unused export) is deliberately left for a follow-up.** Deleting
  symbols needs AST-level edits instead of file removal, and it should wait
  until the CHK001 harness has produced a useful non-baseline-fail signal.
- Per-project dependency provisioning (opt-in, sandboxed) and a Django
  `runtests.py` command mapping.
