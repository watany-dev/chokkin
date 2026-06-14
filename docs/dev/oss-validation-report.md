# OSS validation report — Phase 1 §17 release gate

Run before the v0.1 release to measure the §17 exit criteria over a fixed set
of 20 real OSS Python projects.

- yokei version: `0.1.0`
- date: 2026-06-14
- harness: `scripts/clone-oss-fixtures.sh` + `scripts/oss-metrics.sh`
  (`make oss-metrics`)
- validation set: `scripts/oss-clones.manifest` (pinned tags; resolved SHAs in
  `target/oss-clones/clones.lock.tsv`)
- ground truth: `scripts/oss-fixtures.labels.tsv`

## Verdict: ✅ release gate met

| §17 criterion | Target | Measured | Result |
|---|---|---|---|
| Unused-dependency FP rate (YOK002) | < 5% | **0%** (0 FP / 0 reported) | ✅ PASS |
| Crashes (yokei internal error, exit 3) | 0 | 0 | ✅ PASS |
| Cold run, medium project | ≤ 2000 ms | all within budget | ✅ PASS |

Phase 1.5 workstreams 4.A–4.D cleared the YOK002 false-positive backlog
(155/155 before remediation → 0/0 after). Crash-free and performance criteria
were already passing.

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
