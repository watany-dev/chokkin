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

Every `tp` label must appear in the run's findings or the recall gate fails
(`pass_recall=0`, exit 1). The CHK002 sentinels also keep the FP-rate denominator
non-zero (0 FP / 2 reported on the 20-project set), so the precision figure
reflects real true-positive detection rather than an empty set.

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

## Phase 1.5 remediation summary

| Workstream | Change | Impact |
|---|---|---|
| 4.D | package-module-map aliases + self-extra guard | Map gaps + self-referential extras |
| 4.A | config/binary usage scanner | Dev-tool CLI usage via `[tool.*]`, tox, pre-commit |
| 4.B | dev context policy, PDM/Hatch read, requirements context | Dev groups, nested `-r` includes, `requirements.txt` when pyproject/setup declare runtime deps |
| 4.C | optional/platform-guarded import tracing | try/except + `sys.platform` imports mark distributions used |

Re-run: `make oss-clones && make oss-metrics ARGS=--gate`
