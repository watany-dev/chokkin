# OSS validation report — Phase 1 §17 release gate

Measured for the v0.3 release to verify the §17 exit criteria over a fixed set
of 20 real OSS Python projects.

- chokkin version: `0.3.0`
- date: 2026-07-01 (re-measured for the v0.3 release)
- harness: `scripts/clone-oss-fixtures.sh` + `scripts/oss-metrics.sh`
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

### Before/after R-01..R-07 (pending)

The baseline is commit `573ff37` (before #327–#333; there is no `v0.4.1`
tag), built in a `git worktree` and compared with `main` on the same corpus:

```sh
git worktree add ../chokkin-573ff37 573ff37
cargo build --release --manifest-path ../chokkin-573ff37/Cargo.toml
make oss-clones
scripts/oss-metrics.sh -b ../chokkin-573ff37/target/release/chokkin -o target/oss-metrics-573ff37
make oss-metrics
```

| Corpus | Build | CHK002 | CHK003 | Unclassified CHK002 | Unclassified CHK003 |
|---|---|---|---|---|---|
| 20-project set | `573ff37` | pending | pending | pending | pending |
| 20-project set | `main` | pending | pending | pending | pending |
| R-01..R-07 corpus | `573ff37` | pending | pending | pending | pending |
| R-01..R-07 corpus | `main` | pending | pending | pending | pending |

Not measured yet: neither build could be compiled in the environment
this change was written in (crate downloads were blocked). Until the corpus
findings are labelled in `scripts/oss-fixtures.labels.tsv`, its CHK002 findings
count as unclassified and `make oss-metrics ARGS=--gate` fails the FP gate.

## Phase 1.5 remediation summary

| Workstream | Change | Impact |
|---|---|---|
| 4.D | package-module-map aliases + self-extra guard | Map gaps + self-referential extras |
| 4.A | config/binary usage scanner | Dev-tool CLI usage via `[tool.*]`, tox, pre-commit |
| 4.B | dev context policy, PDM/Hatch read, requirements context | Dev groups, nested `-r` includes, `requirements.txt` when pyproject/setup declare runtime deps |
| 4.C | optional/platform-guarded import tracing | try/except + `sys.platform` imports mark distributions used |

Re-run: `make oss-clones && make oss-metrics ARGS=--gate`
