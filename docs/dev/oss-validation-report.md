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

## Verdict: ❌ release gate NOT met

| §17 criterion | Target | Measured | Result |
|---|---|---|---|
| Unused-dependency FP rate (YOK002) | < 5% | **100.0%** (155 FP / 155 reported) | ❌ FAIL |
| Crashes (yokei internal error, exit 3) | 0 | 0 | ✅ PASS |
| Cold run, medium project | ≤ 2000 ms | ≤ 111 ms (max: werkzeug) | ✅ PASS |

The crash-free and performance criteria pass comfortably. The unused-dependency
false-positive rate fails by a wide margin: **every** YOK002 finding across the
set is a false positive. Per §17 ("リリース判断は期日ではなく exit criteria で
行う … 未達のまま v0.1 を出さない") v0.1 must not ship in this state.

## Validation set (20 projects)

Mix per §17 (library / app / server / framework / Django / FastAPI):

requests, urllib3, click, jinja, werkzeug, flask, httpx, starlette, uvicorn,
attrs, anyio, python-dotenv, tenacity, structlog, pluggy, typer, black,
fastapi, django-rest-framework, django.

All are well-maintained projects whose declared dependencies are, in practice,
genuinely used — so any YOK002 ("unused dependency") finding is a false
positive by construction, which is what makes this set a precision probe.

## Speed

Cold-run median of 3 timed runs per project (yokei has no cache in v0.1, so
every run is cold):

| Class | Slowest | Budget | Result |
|---|---|---|---|
| medium | werkzeug 111 ms | ≤ 2000 ms | ✅ 18× headroom |
| large (info only) | django 1840 ms | — | within 2 s anyway |

No project — including the large, ungated ones — exceeds 2 s. Performance is
not a release risk.

## False-positive root causes (155 findings)

Each finding was classified against ground truth in
`scripts/oss-fixtures.labels.tsv`. All 155 fall into four buckets:

| Root cause | Count | Example |
|---|---|---|
| Dev/test/docs tool used via CLI/CI, not imported (binary + config usage not detected) | 110 | `mypy`, `ruff`, `pytest-cov`, `sphinx`, `mkdocs-material`, `twine`, `coverage` |
| Optional/conditional/platform-guarded runtime import not traced, or PDM/Hatch dependency group unsupported in v0.1 | 34 | `django:tzdata`, `urllib3:brotli`, `jinja:babel`, `uvicorn:colorama` |
| Import name ≠ distribution name (package-module-map gap) | 8 | `python-multipart`→`multipart`, `pyopenssl`→`OpenSSL`, `pysocks`→`socks` |
| Self-referential extra (project depends on itself with extras) | 3 | `attrs[benchmark]`, `structlog`, `anyio` |

`yokei --explain YOK002:<dist>` confirms the mechanism: each finding reports
"no import, plugin module ref, or **binary usage** resolved to this
distribution", and several emit `manifest: PDM/Hatch sections detected
(unsupported in v0.1)`.

### Why these are false positives

- **Dev tooling (110)** — tools configured in `[tool.*]` / invoked by
  tox / pre-commit / CI are genuinely used; flagging them as removable is the
  exact "trust loss" §20 warns against. YOK002's own definition (§3) excludes
  config/binary usage, so detecting it is required, not optional.
- **Optional runtime deps (34)** — `tzdata`, `brotli`, `argon2-cffi`, etc. are
  imported inside `try:`/platform/extra guards yokei does not trace, and
  PDM/Hatch dependency groups are read as plain deps.
- **Map gaps (8)** — the bundled package-module-map is missing common
  import-name ≠ dist-name pairs.
- **Self-extras (3)** — a project listing itself with an extra is always
  "used"; this should never be a YOK002 candidate.

## YOK003 (missing dependency) — informational

Not a §17 gate, but measured for signal: **1747** YOK003 findings, heavily
concentrated in framework/monorepo projects (django 585, fastapi 553,
typer 217). These are dominated by the same root causes (stdlib/first-party
resolution gaps, test-only imports, namespace packages) and indicate the
import resolver / package-module-map needs the same hardening before YOK003 is
trustworthy.

## Recommendations (release blockers)

Priority order to clear the YOK002 gate:

1. **Binary + config usage detection.** Resolve dev tools used via CLI/CI as
   "used": read `[tool.<name>]` tables, `.pre-commit-config.yaml`, tox/nox
   envs, and `[project.scripts]`/entry points (the §3 YOK008 + binary-map
   path). This alone removes ~110/155 (71%).
2. **Dependency-group / extras awareness.** Parse PEP 735 `[dependency-groups]`,
   PDM/Hatch groups, and `requirements-*.txt` dev files as **dev** context, and
   apply a softer policy (or exclude from YOK002 unless `--strict`).
3. **Optional/conditional import tracing.** Treat imports under
   `try/except ImportError`, `sys.platform`, and extras as satisfying their
   declaring extra.
4. **Package-module-map expansion + self-extra guard.** Add the missing
   import-name ↔ dist-name pairs and never flag a project's own distribution.

After landing these, re-run `make oss-metrics ARGS=--gate` and re-classify any
remaining findings in `scripts/oss-fixtures.labels.tsv`; the gate passes when
the YOK002 FP rate drops below 5% with zero unclassified findings.

## Reproduce

```bash
make oss-clones    # clone the 20 pinned projects into target/oss-clones/
make oss-metrics   # build, measure, write target/oss-metrics/report.md
# gate (non-zero exit on any §17 miss):
scripts/oss-metrics.sh --build --clone --gate
```

Raw artifacts (gitignored): `target/oss-metrics/{summary,findings}.tsv`,
`target/oss-metrics/<slug>.json`, `target/oss-metrics/report.md`.
