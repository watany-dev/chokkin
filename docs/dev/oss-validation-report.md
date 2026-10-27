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
[`v0.3-stocktake-coverage.md`](https://github.com/watany-dev/chokkin/blob/a199e81/docs/dev/v0.3-stocktake-coverage.md).

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

### Re-measurement at `8ebad05` (2026-10-09)

`make oss-metrics` on main `8ebad05` (35 clones + 10 recall sentinels,
`--gate` passes). Since the #656 sample, the default-filter hits dropped
sharply (CHK001 300, CHK006 523 hits with 2,230 more at info, CHK010 662 hits
with 222 at info), so 40 CHK001 / CHK006 labels went stale and were refilled.
CHK003, CHK005, CHK007, CHK008 and CHK009 were sampled for the first time with
the same script. Seven CHK010 `tp` labels on optional imports that #670 moved
to info are now `info-expected`, and the mlflow protobuf CHK006 label that
#669 removed was dropped.

| Rule | Hits | Sample | tp | fp | Precision | Main FP buckets |
|---|---:|---:|---:|---:|---:|---|
| CHK001 | 300 | 29 | 2 | 27 | 7% | script-entry 14, other 5, framework-loaded 3, dynamic-import 3, library-public 2 |
| CHK002 | 27 | 27 | 26 | 1 | 96% | — |
| CHK003 | 170 | 20 (fresh) | 17 | 3 | 85% | map-gap 3 (`pydantic_ai` from pydantic-ai-slim). The earlier labels bring it to 42 / 45 (93%) |
| CHK004 | 1,926 | 54 | 54 | 0 | 100% | — |
| CHK005 | 44 | 24 | 8 | 16 | 33% | optional-import 13 (lazy or feature-gated imports), test-only-path 2 |
| CHK006 | 523 | 30 | 13 | 17 | 43% | library-public-api 7, name-convention 4, external-reference 3, dynamic-import 3 |
| CHK007 | 53 | 14 | 0 | 14 | 0% | library-public-api 11 (fastmcp / mcp / llama_index_instrumentation run in app mode, so `__all__` is not treated as API), external-reference 3 |
| CHK008 | 113 | 50 | 7 | 43 | 14% | declared-elsewhere 21 (tox `deps` / `-r`, nox `session.install`, workspace members), isolated-env 11 (pre-commit hook repos), not-invoked 6 (config section only), ci-provided 3, transitive-provider 2 |
| CHK009 | 12 | 11 | 0 | 11 | 0% | intentional-duplicate 11 (extras built by joining lists, generated extras, a lint group installed alone) |
| CHK010 | 662 | 50 | 19 | 31 | 38% | declared-third-party 18, first-party-missed 8 (`sys.path` inserts, example packages), stdlib 2, other 2, generated 1 |

Samples are capped at 6 per project, so CHK006 (5 projects with hits) and
CHK007 (3 projects) have fewer than 50 labels, and a 30-label sample has a 95%
interval of about ±18 points. Five CHK005 tp are imports that work only
through another runtime dependency (`openai` via llama-index-llms-openai,
`anyio` via starlette). Counting them as fp gives 3 / 24 (13%).

### Precision top-up at `883e306` (#699, 2026-10-10)

CHK004 / CHK006 / CHK010 were topped up with the same script until each covers
at least 20% of its hits (`--total 370 --per-project 80` for CHK004,
`--total 225 --per-project 70` for the other two), so these three samples are
no longer capped at 6 per project and mostly come from airflow, llama_index,
langchain, mlflow and fastmcp. 19 stale CHK010 labels were dropped. The 95%
interval shrinks to about ±7 points for CHK006 / CHK010; the other rules keep
the 2026-10-09 sample.

| Rule | Hits | Sample | tp | fp | Coverage | Precision | Main FP buckets |
|---|---:|---:|---:|---:|---:|---:|---|
| CHK004 | 1,888 | 403 | 401 | 2 | 21% | 99.5% | map-gap 2 (`google.cloud.firestore` from google-cloud-firestore, missing from the bundled map) |
| CHK006 | 524 | 230 | 127 | 103 | 44% | 55% | library-public-api 35 (`__all__` / documented API in app mode), dynamic-import 28 (`import_module` + `getattr`, airflow `lazy_load_command`), external-reference 24 (module-attribute access, Sphinx `conf.py`), name-convention 16 (pluggy / pytest hooks, plugin `inspect.getmembers`) |
| CHK010 | 350 | 225 | 114 | 111 | 64% | 51% | declared-third-party 55 (import≠dist names such as `weaviate-client`, `pymupdf`, `gremlinpython`; `databricks.agents` from databricks-agents, which the bundled map lacks), first-party-missed 46 (`sys.path` inserts, test-only packages, task-sdk tests), generated 4, stdlib 3, other 3 |

CHK004 `tp` rows that a member manifest omits but the root lockfile provides
are noted `lock-only edge`; they are the population #649 will use to pick the
transitive-dependency threshold.

### Re-measurement, stale cleanup and CHK001 / CHK004 top-up at `3406814` (#785, 2026-10-11)

`scripts/oss-metrics.py -r 1 --with-strict --gate` at main `3406814` (after
#788–#792) passes every gate: CHK002 FP 3.7% (1 / 27), recall 843 / 843, no
exit 3 / exit 2, every expectation within range, speed within budget.

Before topping up, 228 labels whose finding no longer appears were deleted.
They are fixed false positives, recorded here so the deleted rows stay
accounted for:

| Rule | Rows | Verdict | When the finding disappeared |
|---|---:|---|---|
| CHK001 | 6 | fp | mlflow: 5 at v0.7.4 (#776 / #792; dynamic-import 2, other 2, script-entry 1), 1 before |
| CHK002 | 93 | fp | before v0.7.4 |
| CHK003 | 3 | fp | before v0.7.4 (airflow `pydantic_ai`, map-gap) |
| CHK005 | 2 | fp | before v0.7.4 (llama_index `IPython`, mitmproxy `requests`) |
| CHK006 | 67 | 64 fp, 3 info-expected | 45 before v0.7.4 (poetry 19 #750 / #728, fastmcp 11 + mcp 1 #742, airflow provider_conf 12 #729 + 1, mlflow 1); 22 at v0.7.4 (mlflow library-public-api 15, name-convention 4, info-expected 3; #776 / #792) |
| CHK008 | 33 | fp | before v0.7.4 (#692 / #745) |
| CHK009 | 1 | fp | before v0.7.4 (poetry `dulwich`) |
| CHK010 | 23 | fp | before v0.7.4 (first-party-missed, `sys.path` inserts #754, ...) |

All 93 CHK002 targets are still declared in the pinned clones (including the
six PEP 723 scripts), and the clone manifest has not changed since #690, so
they vanished because of analysis fixes, not pin bumps. The 74 CHK002 labels
that report.md still lists as stale are dev-group dependencies: they match
only the `--strict` run and are kept.

CHK001 and CHK004 were then topped up to at least 30% coverage
(`--total 21 --per-project 12`, `--total 565 --per-project 140`). The 14 new
CHK001 labels are all fp; the 196 new CHK004 labels are all tp, checked
against the member / root manifests and the nearest `uv.lock` (14 are
`lock-only edge`, 10 reach the import only through the dev closure). The
expectations for sqlalchemy (CHK010 14 → 6), transformers (CHK010 234 → 203)
and langchain (CHK001 3 → 0) follow the fixes in the same range.

| Rule | Hits | Sample | tp | fp | Coverage | Precision | Main FP buckets |
|---|---:|---:|---:|---:|---:|---:|---|
| CHK001 | 63 | 21 | 2 | 19 | 33% | 10% | script-entry 8 (workflow / pre-commit / SKILL.md `python path.py` runs, standalone examples), framework-loaded 5 (Sphinx extensions #779, DAG folders), dynamic-import 3, library-public 2 (#778, #782), other 1 |
| CHK002 | 27 | 27 | 26 | 1 | 100% | 96% | — |
| CHK003 | 115 | 48 (+42 deferred) | 48 | 0 | 42% | 100% | — |
| CHK004 | 1,877 | 610 | 610 | 0 | 32% | 100% | — |
| CHK005 | 36 | 17 | 8 | 9 | 47% | 47% | optional-import 8 (#781), other 1 |
| CHK006 | 270 | 104 | 83 | 21 | 39% | 80% | external-reference 10 (airflow `security/permissions.py`, a deprecation shim; #778), name-convention 6 (#777), dynamic-import 5 (#493) |
| CHK008 | 18 | 8 | 7 | 1 | 44% | 88% | not-invoked 1 |
| CHK009 | 0 | 0 | 0 | 0 | n/a | n/a | — |
| CHK010 | 214 | 126 | 110 | 16 | 59% | 87% | declared-third-party 10 (#749 / #780), first-party-missed 5, generated 1 |

CHK006 precision went up from 55% at `883e306` to 80%, but less than
the 85.5% estimated in #787: #792 relabelled 41 mlflow tp as info-expected, so
tp fell from 124 to 83. CHK010 has 16 fp rather than the estimated 14 (#790
removed 18 fp, not 20; #789 moved 3 tp to CHK003).

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
(`workflow_dispatch`, weekly, and on pull requests and main pushes that touch
`src/**`, `Cargo.lock` or the corpus / labels / scripts; #835),
prints the table below and the unclassified HEAD findings to the job summary,
and fails unless the §17 Phase 4 gate passes and the CHK003 total, without
info-severity findings (#773), `tp`-labelled findings and recall fixtures,
does not grow over `573ff37`.

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

## Orchestration plugin corpus (#1057)

The Dagster, Prefect and Scrapy plugins had only been measured on fixtures and
on the framework repositories themselves, not on code written for them. One
user-side project per plugin was added under `# --- orchestration plugin
corpus`; each holds the plugin's config at its root. None tags releases, so
each is pinned by its default branch's commit.

| Slug | Commit (date) | Plugin config | Mode |
|---|---|---|---|
| `dagster-open-platform` | `c29d915` (2026-10-02) | `[tool.dg]` project, `registry_modules`, `defs/` with `component.yaml` | library |
| `prefect-collection-registry` | `6be8519` (2026-06-16) | `prefect.yaml` with one deployment entrypoint | library |
| `open-gov-crawlers` | `87bdeaa` (2025-08-08) | `scrapy.cfg`, `public_law/settings.py`, 30 spiders | library |

Rejected candidates:

- `alltheplaces`: the layout guess picks `ci` as the source package, and the
  thousands of spiders the Scrapy plugin then adds under `locations/` resolve
  to nothing (5809 CHK010). A layout bug, not the plugin's, so it would
  measure the wrong thing.
- `city-scrapers`: dependencies are only in a `Pipfile`, which chokkin does
  not read, so every import is a certain CHK003.
- `splash_flows_globus`: its `prefect.yaml` is in a subdirectory; the Prefect
  plugin reads only the root one.

### Plugin on vs off

Each project was run with the PR build and with the same build with plugin
dispatch emptied (`src/plugins/extract.rs`), default and `--strict`. GONE =
reported only with plugins off; NEW = reported only with plugins on.

| Project | Reachable runtime files (on / off) | GONE | NEW | `--strict` CHK001 (on / off) |
|---|---|---|---|---|
| `dagster-open-platform` | 125 / 38 of 126 | 10 CHK006 | 2 CHK004 | 1 / 88 |
| `prefect-collection-registry` | 9 / 9 of 10 | — | — | 1 / 1 |
| `open-gov-crawlers` | 84 / 80 of 88 | 22 CHK006 | — | 4 / 8 |

- All 32 GONE are false positives the plugin removes: Dagster `defs` objects
  and the helpers only `defs/` modules use (the plugin roots `defs/` and the
  `definitions.py` module), and Scrapy settings constants the framework reads
  by name plus two helpers only spiders use.
- The 2 NEW are true positives: `google.oauth2` in two `defs/` resources, which
  only become reachable with the plugin; google-auth comes only through
  google-api-python-client (labelled `tp`).
- `prefect-collection-registry` does not change: it is library mode, so the
  `src/` package is already public and the deployment entrypoint adds nothing
  reachable.
- Gaps: none of the three plugins left a value unread (0 opaque command
  targets, 0 unread settings). `dagster_cloud.yaml`'s `module_name` is not
  read, but `[tool.dg]` already roots the same module.

Every CHK002 and CHK004 hit on the three projects is labelled in
`scripts/oss-fixtures.labels.tsv`. Their CHK006 are labelled too: all three
projects have no app signal and run in library mode, so the symbols are really
unused but reported at info, labelled `info-expected`. There is no CHK001 hit in the default run, and the six `--strict` `maybe`
CHK001 are in `scripts/oss-maybe.labels.tsv` (all `tp`). The CHK002 hits are
mostly security or transitive pins that are never imported (`tp`); the two
`fp` are implicit uses chokkin cannot see: `dagster-webserver`, which `dg dev`
serves, and `dbt-snowflake`, the adapter `profiles.yml` names. With them the
gated CHK002 FP rate would be 5.4% (3 / 56), so this section's CHK002 is
recorded only, like the app-mode corpus (#1065): the gate would measure forms
chokkin does not read yet. The gated run then passes every criterion
(CHK002 FP 2.6%, 1 / 39; recall 1007 / 1007).

Four light fixes came out of the measurement: `progressbar2` was missing from
the package map (CHK010 and a CHK002 on `open-gov-crawlers`), and the closed binary map did not
know `dagster`, `dg`, `dbt`, `prefect` or `scrapy`, so `uv run dg dev` and
`uv run dbt deps` in a Makefile left `dagster-dg-cli` and `dbt-core` unused.
The same map left the dev-group `sqlfluff` unused (`--strict`) although the
Makefile runs `sqlfluff lint`. Review then found that a project declaring only
a dbt adapter or the old `dagster-dg` distribution would get CHK008 for `dbt`
or `dg`, so those now provide the CLI as wrappers do.

## Diversity corpus

The 50 projects above were picked one feature at a time, and the picks
cluster: 11 are web/HTTP libraries or frameworks (pallets, encode, Django,
FastAPI), 7 are LLM tooling, 6 are Django apps, and every one of them is pure
Python with a `pyproject.toml`-first build. Nothing covered scientific code,
compiled extensions, a `setup.py`-only package or a single-module package, and
only 6 projects were `small`. Thirteen release-pinned projects were added
under `# --- diversity corpus`, one or two per missing axis:

| Slug | Tag | Category / size | Axis it adds |
|---|---|---|---|
| `pandas` | `v3.1.0` | library / large | data science; meson-python, Cython |
| `scikit-learn` | `1.9.1` | library / large | machine learning; meson-python, Cython; optional plotting deps |
| `pytest` | `9.1.1` | cli / medium | test runner; `pytest11` plugins; setuptools-scm |
| `rich` | `v15.0.0` | library / medium | terminal UI; poetry-core backend |
| `sphinx` | `v9.2.0` | cli / large | docs tool; flit backend; extensions loaded by name |
| `ansible` | `v2.21.5` | cli / large | infrastructure automation; plugins loaded by directory |
| `aiohttp` | `v3.14.5` | library / medium | async networking; C extensions; `setup.cfg` |
| `pillow` | `13.0.0` | library / medium | imaging; C extensions; in-tree build backend |
| `boto3` | `1.43.119` | library / small | cloud SDK; `setup.py` / `setup.cfg` only |
| `six` | `1.17.0` | library / small | single module; `setup.py` only |
| `more-itertools` | `v11.2.1` | library / small | small library; flit backend |
| `httpie` | `3.2.4` | cli / medium | CLI app; `setup.py` / `setup.cfg` only |
| `mypy` | `v2.4.0` | cli / large | type checker; mypyc-compiled |

Measured on 2026-10-27 with chokkin `0.7.11` plus the pyrefly fix below
(3 cold runs, median):

| Slug | Exit | Median ms | Findings | Main findings |
|---|---:|---:|---:|---|
| `pandas` | 0 | 253 | 76 | 65 CHK006 |
| `scikit-learn` | 0 | 168 | 74 | 45 CHK003 (`matplotlib` / `pandas` imported in functions) |
| `pytest` | 0 | 54 | 48 | 34 CHK006 |
| `rich` | 0 | 35 | 24 | 19 CHK006, 3 CHK004 (`IPython`) |
| `sphinx` | 0 | 94 | 88 | 66 CHK006, 7 CHK002 |
| `ansible` | 0 | 224 | 707 | 467 CHK001, 174 CHK006 |
| `aiohttp` | 0 | 56 | 108 | 103 CHK006 |
| `pillow` | 0 | 64 | 74 | 61 CHK006 |
| `boto3` | 0 | 26 | 11 | 8 CHK006 |
| `six` | 0 | 10 | 5 | 2 CHK008 |
| `more-itertools` | 0 | 31 | 1 | 1 CHK008 |
| `httpie` | 0 | 26 | 12 | 4 CHK003 (`urllib3` via `requests`) |
| `mypy` | 0 | 106 | 40 | 33 CHK006 |

No project crashes or exits 2, and the slowest one (pandas, 1544 `.py`
files) takes a quarter of a second, so speed does not depend on the domains
the old corpus favoured. v0.4.1 (`573ff37`, the CI baseline) exits 2 on
`sphinx` and reports 115 CHK003 on these projects against HEAD's 75, so the
CHK003 growth criterion does not move; timed in turn with v0.7.10, the
large projects summed 803 ms -> 817 ms (1.02x).

Every CHK002 hit is labelled. All 7 are `fp` on `sphinx`: `alabaster` and the
six `sphinxcontrib-*` distributions are runtime dependencies that Sphinx loads
by name from `builtin_extensions` in `sphinx/application.py`. chokkin already
notices those strings (`names sphinx.builders.changes and 53 more modules in
strings`) but does not map them to distributions. As with the app-mode and
orchestration corpora, this section's CHK002 is recorded only: the gate would
measure a form chokkin does not read yet.

### Precision on the diversity corpus

Every hit of the diversity corpus was then sampled per project and rule (at
most 6 each, first in `sample-precision-labels.py` hash order, 95 labels) and
labelled by reading the clones. `info-expected` counts as `fp`, as in the
gate. The existing corpus column is the same full run without the diversity
section:

| Rule | Diversity hits | Labelled | tp / fp | Diversity precision | Existing corpus | Floor |
|---|---:|---:|---:|---:|---:|---:|
| CHK001 | 471 | 12 | 2 / 10 | 16.7% | 88.5% (23/26) | 85 |
| CHK002 | 7 | 7 | 0 / 7 | 0.0% | (FP-rate gate) | — |
| CHK003 | 34 | 21 | 4 / 17 | 19.0% | 100.0% (146/146) | 90 |
| CHK004 | 4 | 4 | 1 / 3 | 25.0% | 100.0% (611/611) | 95 |
| CHK005 | 1 | 1 | 0 / 1 | 0.0% | 89.5% (17/19) | 85 |
| CHK006 | 207 | 12 | 3 / 9 | 25.0% | 96.2% (101/105) | 90 |
| CHK008 | 14 | 14 | 7 / 7 | 50.0% | 85.7% (12/14) | 80 |
| CHK010 | 57 | 35 | 26 / 9 | 74.3% | 89.8% (106/118) | 89 |

The floors were measured on the corpus that the rules were tuned on, and the
diversity corpus is far below them. Pooled, five rules would fail their floor
(CHK001 65.8%, CHK003 89.8%, CHK006 88.9%, CHK008 67.9%, CHK010 86.3%), so
the floors leave the diversity corpus out and `report.md` records its
precision per rule in the same row. The false positives fall into a few
patterns:

- Optional imports chokkin cannot see as optional (CHK003 / CHK004 / CHK005,
  21 of 32 non-CHK001/006 `fp`): imports after an availability helper
  (#1269), inside the only module a host framework loads (aiohttp's Gunicorn
  worker), behind `"IPython" in sys.modules`, or in a `try` whose
  `ModuleNotFoundError` handler the CHK004 path does not honour (rich).
- Files and symbols a loader finds by name or directory (CHK001 / CHK006):
  ansible-test targets and plugin directories (#1267, #1268), doc fragments
  named in module docs, platform subclasses chosen by a `platform`
  attribute, dmypy handlers registered by an `@action(parser)` decorator, a
  mypy plugin named in a `--config-file` ini, and mypyc test fixtures copied
  by path.
- Tools installed into an isolated environment (CHK008 / CHK010): tox
  `deps`, ansible-test's per-test requirement files, and pre-commit hooks.

One light fix came out of the measurement: `pyrefly.toml` and
`[tool.pyrefly]` now mark `pyrefly` used, like `ty.toml` and
`pyrightconfig.json` do for their checkers (`sphinx`'s `types` group).

Gaps found, left for follow-up issues:

- `sphinx`: module-name strings passed to an extension loader do not use the
  distribution that provides the module (7 CHK002 `fp`, #1266).
- `ansible`: 467 CHK001 in app mode. 251 are `test/integration/targets`
  files that `ansible-test` runs, and 121 are modules and plugins under
  `lib/ansible/{modules,plugins}` that the plugin loader finds by directory
  (#1268, #1267).
- `scikit-learn`: 20 `certain` CHK003 for `matplotlib` and `pandas` (and 2
  for `pytest`). They are imported inside functions after
  `check_matplotlib_support()` / `check_pandas_support()`, helpers that
  try-import the package and raise a helpful error, so the import is optional
  although it is not under a `try` itself. The 1.9 `pyproject.toml` declares
  no extras; the optional versions live in `sklearn/_min_dependencies.py`
  (#1269).

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
