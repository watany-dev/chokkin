# OSS validation report — Phase 1 §17 release gate

Measured for the v0.3 release to verify the §17 exit criteria over a fixed set
of 20 real OSS Python projects.

- chokkin version: `0.3.0`
- date: 2026-07-01 (re-measured for the v0.3 release)
- harness: `scripts/clone-oss-fixtures.sh` + `scripts/oss-metrics.py`
  (`make oss-metrics`)
- validation set: `scripts/oss-clones.manifest` (pinned tags; resolved SHAs in
  `target/oss-clones/clones.lock.tsv`)
- ground truth: `scripts/oss-fixtures.labels.tsv`
- recall sentinels: `scripts/oss-recall.manifest`

## Verdict: ✅ release gate met

| §17 criterion | Target | Measured | Result |
|---|---|---|---|
| Unused-dependency FP rate (CHK002) | < 5% | **0.0%** (0 FP / 2 reported) | ✅ PASS |
| Unused-dependency recall (`tp` labels) | all detected | **3/3** detected | ✅ PASS |
| Crashes (chokkin internal error, exit 3) | 0 | 0 | ✅ PASS |
| Cold run, medium project | ≤ 2000 ms | all within budget | ✅ PASS |

Phase 1.5 workstreams 4.A–4.D cleared the CHK002 false-positive backlog
(155/155 before remediation → 0 false positives after). Crash-free and
performance criteria were already passing.

### Recall guard (why "0 reported" is not enough)

A pure FP-rate gate is satisfied by reporting nothing: with no findings the
rate is `n/a` and passes trivially. To stop the remediation from collapsing
into silent over-suppression, the harness also measures in-repo **recall
sentinels** — fixtures with a deliberately-unused dependency that chokkin must
keep flagging, labelled `tp` in the ground truth:

| Sentinel | Rule | Target | Guards |
|---|---|---|---|
| `unused_boto3` | CHK002 | `boto3` | a declared runtime dep with no import anywhere stays detected |
| `optional_try_import` | CHK002 | `requests` | an unused dep coexisting with a correctly-suppressed optional import — 4.C must not over-suppress |
| `missing_yaml` | CHK003 | `src/acme/main.py:yaml` | an undeclared third-party import stays detected as missing dependency |
| `include_group` | CHK002 | `boto3` | R-01: a dep declared only in a group pulled in through `include-group` is still reported |
| `pep723_unused_script` | CHK002 | `script:scripts/tool.py:boto3` | R-02: an unused PEP 723 script dependency is still reported |
| `lock_unused_pylock` / `lock_unused_poetry` / `lock_unused_pdm` | CHK002 | `boto3` | R-03: reading `pylock.toml` / `poetry.lock` / `pdm.lock` does not hide an unused direct dep |
| `lock_unused_pylock` / `lock_unused_poetry` / `lock_unused_pdm` | CHK004 | `src/acme/main.py:urllib3` | R-03: an import only locked as a transitive of `requests` stays CHK004 for each lockfile format |
| `uv_dev_dependencies_unused` | CHK002 | `boto3` | R-04: `[tool.uv] dev-dependencies` and `constraint-dependencies` on the same name do not hide an unused runtime dep |
| `build_plugin_declared` | CHK002 | `hatch-vcs` | R-05: a build plugin declared as a runtime dep in a project with `[build-system]` is still reported |

Every `tp` label must appear in the run's findings or the recall gate fails
(`pass_recall=0`, exit 1). The CHK002 sentinels also keep the FP-rate denominator
non-zero (0 FP / 2 reported on the 20-project set), so the precision figure
reflects real true-positive detection rather than an empty set.

`tests/recall_sentinels.rs` replays the same check in `cargo test`: it runs the
binary on every sentinel in `scripts/oss-recall.manifest`, requires each `tp`
label to be reported, and requires every CHK002 on a sentinel to carry a `tp`
or `fp` label, so CI catches a silenced sentinel without OSS clones (#324).

## Phase 3.x Step 0 — per-rule label coverage (2026-08-11)

The metrics harness now emits **CHK001–CHK010** findings (not only CHK002/CHK003)
and prints a per-rule label-coverage table in `target/oss-metrics/report.md`.
§17 gate conditions are unchanged (CHK002 FP / crash / speed / recall).

Full stocktake (CHK003 baseline, blind spots for CHK001/CHK004–010): see
[`v0.3-stocktake-coverage.md`](./v0.3-stocktake-coverage.md).

## Per-rule precision samples (#656, 2026-10-08)

CHK001 / CHK004 / CHK006 / CHK010 now have a stratified hand-labelled sample
(about 50 per rule, at most 6 per project, `scripts/sample-precision-labels.py`).
Labels were set by reading the pinned clones; nothing was executed. Precision is
recorded in `report.md` and has no threshold yet. Info-severity findings are not
counted as hits: optional try-imports (CHK003) are labelled `info-expected`.

| Rule | Sample | tp | fp | Precision | Main FP buckets |
|---|---:|---:|---:|---:|---|
| CHK001 | 50 | 3 | 47 | 6% | script-entry 13, other 11 (paths in strings, subprocess scripts), dynamic-import 8, framework-loaded 6, library-public 6, type-stub 3 |
| CHK004 | 54 | 54 | 0 | 100% | — |
| CHK006 | 50 | 25 | 25 | 50% | name-convention 10 (getattr / dotted strings / entry points), library-public-api 9, external-reference 6 (star re-export, `mod.name`) |
| CHK010 | 50 | 21 | 29 | 42% | declared-third-party 14 (nested requirements files, import≠dist names, namespace pkgs, markers), first-party-missed 5, other 5, stdlib 4, generated 1 |

CHK010 counts imports reachable only through a declared package's dependencies
(`mkdocs` via mkdocs-material, `zope.interface` via twisted) as tp.

## Validation set (20 projects)

Mix per §17 (library / app / server / framework / Django / FastAPI):

requests, urllib3, click, jinja, werkzeug, flask, httpx, starlette, uvicorn,
attrs, anyio, python-dotenv, tenacity, structlog, pluggy, typer, black,
fastapi, django-rest-framework, django.

## R-01..R-07 feature corpus (#323)

The 20-project set predates uv workspaces, PEP 735 groups, PEP 723 scripts and
lockfile reading, so it cannot show whether R-01..R-07 reduce false positives
on real projects. Nine tag-pinned projects were added to
`scripts/oss-clones.manifest`; each was checked at its pinned tag.

| Slug | Tag | Category / size | Why it is in the corpus |
|---|---|---|---|
| `pydantic` | `v2.13.5` | library / medium | uv workspace + `[dependency-groups]` with 6 `include-group`; PEP 695 syntax in 4 files |
| `mlflow` | `v3.16.1` | app / large | uv workspace + `[dependency-groups]` + `include-group`; 3 PEP 723 scripts |
| `airflow` | `3.3.2` | app / large | large uv workspace (provider packages) + `[dependency-groups]` + `include-group` |
| `fastmcp` | `v4.0.9` | framework / large | 8 PEP 723 scripts; uv workspace + `[dependency-groups]` |
| `mcp-python-sdk` | `v2.2.0` | library / large | 2 PEP 723 scripts; uv workspace + `[dependency-groups]` |
| `virtualenv` | `21.12.1` | cli / medium | root `pylock.zipapp.toml` (named `pylock.<name>.toml`); 8 `include-group` |
| `poetry` | `2.5.1` | cli / medium | `poetry.lock` at the root |
| `pdm` | `2.29.2` | cli / medium | `pdm.lock` at the root |
| `mitmproxy` | `v12.2.3` | app / large | `requires-python >= 3.12` with PEP 695 syntax in 4 files; `include-group` |

Coverage against the issue's minimums:

- uv workspace + `[dependency-groups]` + `include-group`: `pydantic`, `mlflow`,
  `airflow` (3; `fastmcp` and `mcp-python-sdk` add workspace + groups without
  `include-group`).
- PEP 723 scripts: `fastmcp`, `mlflow`, `mcp-python-sdk` (3).
- Lockfiles: `pylock` (`virtualenv`), `poetry.lock` (`poetry`), `pdm.lock`
  (`pdm`). `uv.lock` is already covered by the uv workspaces above.
- PEP 695: `pydantic`, `mitmproxy`.

Rejected candidates: `logfire` `v5.1.1` has no `include-group` at the tag;
`pip` keeps its `pylock.toml` under `build-project/`, and chokkin only reads a
lockfile at the project root.

### Before/after R-01..R-07

The baseline is commit `573ff37` (before #327–#333; there is no `v0.4.1`
tag), built in a `git worktree` and compared with `main` on the same corpus:

```sh
git worktree add ../chokkin-573ff37 573ff37
cargo build --release --manifest-path ../chokkin-573ff37/Cargo.toml
make oss-clones
scripts/oss-metrics.py -b ../chokkin-573ff37/target/release/chokkin -o target/oss-metrics-573ff37
make oss-metrics
```

`.github/workflows/oss-metrics.yml` runs the same steps on GitHub Actions
(`workflow_dispatch`, or a push that touches the corpus / labels / scripts),
prints the table below and the unclassified HEAD findings to the job summary,
and fails unless the §17 Phase 4 gate passes and the CHK003 total, without
`tp`-labelled findings and recall fixtures, does not grow over `573ff37`.

Measured on GitHub Actions run
[36306495456](https://github.com/watany-dev/chokkin/actions/runs/36306495456)
(PR #367 head `02ea86c`). "Recall fixtures" are the in-repo sentinels from
`scripts/oss-recall.manifest`.

| Corpus | CHK002 `573ff37` | CHK002 HEAD | CHK003 `573ff37` | CHK003 HEAD |
|---|---:|---:|---:|---:|
| 20-project set | 0 | 0 | 116 | 116 |
| R-01..R-07 corpus (9 projects) | 40 | 18 | 172 | 185 |
| Recall fixtures (10) | 7 | 9 | 4 | 1 |

R-01..R-07 corpus per project:

| Project | CHK002 `573ff37` | CHK002 HEAD | CHK003 `573ff37` | CHK003 HEAD |
|---|---:|---:|---:|---:|
| airflow | 2 | 14 | 0 | 4 |
| fastmcp | 0 | 1 | 2 | 8 |
| mcp-python-sdk | 0 | 0 | 4 | 0 |
| mitmproxy | 9 | 2 | 1 | 1 |
| mlflow | 9 | 0 | 158 | 169 |
| pdm | 11 | 1 | 4 | 0 |
| poetry | 9 | 0 | 2 | 2 |
| pydantic | 0 | 0 | 1 | 1 |
| virtualenv | 0 | 0 | 0 | 0 |

- CHK002 on HEAD: 27 findings, 0 unclassified, 1 fp
  (`fastmcp` `script:examples/screenshot.py:pillow`: declared in the PEP 723
  block, never imported by the script).
- The CHK003 growth is new true positives: PEP 723 scripts are now checked
  against their own block (airflow, fastmcp), and mlflow imports `langgraph`
  submodules and `grpc` that it does not declare. Excluding `tp`-labelled
  findings and recall fixtures, CHK003 goes from 288 to 283.
- Unclassified CHK003 on HEAD: 241 (not gated).

Gate result:

| Criterion | Target | Measured | Result |
|---|---|---|---|
| Unused-dep FP rate (CHK002) | < 5% | 3.7% (1 FP / 27 reported, 0 unclassified) | PASS |
| Recall (`tp` labels) | all detected | 48/48 detected | PASS |
| Crashes (exit 3) | 0 | 0 | PASS |
| Cold run, medium project | <= 2000 ms | all within budget | PASS |
| CHK003 excluding `tp` | <= `573ff37` | 283 <= 288 | PASS |

## #346 corpus measurements (#338–#342)

Measured on 2026-10-04 with chokkin `0.6.0` over the same 20 pinned clones.
These are measurements, not new release criteria. Details and reproduction
steps are in [`chk001-remove-and-test.md`](./chk001-remove-and-test.md)
(#338, #339) and
[`oss-differential-and-recall.md`](./oss-differential-and-recall.md) (#340,
#341, #342).

### Gates (#342)

| Gate | Harness | Measured | Result |
|---|---|---|---|
| Determinism: cold, warm and `--no-cache` byte-identical | `make oss-gate` | 30/30 (20 clones + 10 sentinels) | PASS |
| JSON schema and summary consistency | `make oss-gate` | 30/30 | PASS |
| Crash: no exit 3, no non-JSON output | `make oss-gate` | 0 | PASS |
| Performance: no benchmark > 10% slower with its 95% CI above 0, confirmed by a re-run | `make bench-gate` (A/A: this branch has no Rust change) | 0/27 regressed | PASS |

The performance gate was calibrated with an A/A run on a shared VM:
`make bench-save BASELINE=main` followed by `make bench-gate` on the same code.
A single comparison flagged 4 of 27 benchmarks (+10.9% to +14.6%, CIs above 0),
so sequential runs drift by more than the threshold. The gate now re-runs only
the flagged benchmarks (`--confirm 1`) and fails only if they regress again. In
the calibrated run, `parse_cache_warm/src/1000` went from +10.5% to -1.6% on
re-run, and the gate passed. Use a dedicated runner for a tighter threshold.

### Precision by removal (#338, #339)

Each finding is removed from a disposable copy and the project's own test
suite is run offline in a per-project venv. Precision = pass / (pass + break).

| Rule | Removal | Tested | pass | break | Precision |
|---|---|---:|---:|---:|---:|
| CHK001 | delete the file | 50 | 28 | 22 | 56.0% |
| CHK006 | delete or privatize the symbol | 243 | 226 | 17 | 93.0% |

All 22 CHK001 breaks are files the suite uses without importing them. Seven
are fastapi modules judged in app mode, seven are black test data, and eight
are werkzeug/uvicorn test modules loaded by path or import string. Django
(988 CHK001) is not run: about 6 minutes per baseline, four environmental
baseline failures and an expected yield dominated by string-loaded modules.

CHK006 is sampled to 15 findings per project, giving 243 tests over 17 projects.
Of the 17 breaks, 5 are chokkin false positives: names re-exported through
`from m import *` (httpx's private modules into `httpx/__init__.py`, and
`attrs.exceptions`). Another 5 are names referenced as strings (`dictConfig`,
`monkeypatch.setattr`, `getattr`), 3 are framework hooks called by name
(pytest plugin hooks and an IPython extension), and 4 are classes whose
runtime name is observable once they are renamed.

### Mutation recall (#341)

| Injection | Default | `--confidence maybe` |
|---|---:|---:|
| CHK001 orphan module | 8/20 | 20/20 |
| CHK002 unused dependency | 14/14 | 14/14 |
| CHK003 undeclared import | 18/20 | 18/20 |
| CHK010 unresolved import | 18/20 | 18/20 |
| CHK006 unreferenced function | 17/20 | 17/20 |
| CHK007 unused re-export | 18/20 | 18/20 |
| Trap: `importlib.import_module` target (lower is better) | 0/20 | 2/20 |
| Trap: `PyYAML`/`Pillow` dist ≠ import name (lower is better) | 1/14 | 1/14 |

The CHK001 default misses are the library-mode `maybe` cap. Every other miss
and trap trigger is in urllib3 or pluggy (no entry root, see follow-ups), plus
one anyio backend loaded through `importlib`.

### Differential (#340)

Agreement is low by design, so the useful output is the triage of each
disagreement:

| Rule | Tool | Jaccard | Main reason for disagreement |
|---|---|---:|---|
| CHK002 | deptry / fawltydeps | 0% (0 vs 74/81) | the others flag dev and docs extras, which chokkin treats as non-runtime |
| CHK003 | deptry / fawltydeps | 13.9% / 36.7% | `setup.py` with variable `install_requires` (requests); optional backends (django) |
| CHK006 | vulture / deadcode | 7.9% / 7.3% | different claims: the others report names unused anywhere, and CHK006 reports public module-level names that nothing outside their module uses; the other-only names are dynamically loaded or framework-used |
| CHK007 | ruff F401 / pyflakes | 6.1% / 18.9% | the `import X as X` and `__all__` re-export conventions |

### Follow-ups

1. **Library with `console_scripts` resolves to app** (fastapi, uvicorn), so
   public modules are judged by app reachability. Treat `[project.scripts]`
   together with an importable package as library in `auto` mode, or cap
   CHK001 inside the distributed package.
2. **Library entry roots.** In urllib3 and pluggy nothing is reachable,
   because the package is not an entry root and `test/` and `testing/` are
   not discovered as test roots.
3. **String module references** (`import_module(f"...{lang}.formats")`,
   gunicorn `-k uvicorn.workers.UvicornWorker`, Django settings dotted paths):
   the plugin route of spec §9.
4. **Star re-exports for CHK006.** A name pulled into a public module with
   `from m import *` (and listed in its `__all__`) should count as used.
5. **`setup.py` with a non-literal `install_requires`** is skipped, which
   produces requests' CHK003 false positives (labelled `deferred`).

## Phase 1.5 remediation summary

| Workstream | Change | Impact |
|---|---|---|
| 4.D | package-module-map aliases + self-extra guard | Map gaps + self-referential extras |
| 4.A | config/binary usage scanner | Dev-tool CLI usage via `[tool.*]`, tox, pre-commit |
| 4.B | dev context policy, PDM/Hatch read, requirements context | Dev groups, nested `-r` includes, `requirements.txt` when pyproject/setup declare runtime deps |
| 4.C | optional/platform-guarded import tracing | try/except + `sys.platform` imports mark distributions used |

Re-run: `make oss-clones && make oss-metrics ARGS=--gate`
