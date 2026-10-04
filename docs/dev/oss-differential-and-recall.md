# Differential Oracle and Mutation Recall (#340, #341)

Date: 2026-10-04  
chokkin: `0.6.0`, over the pinned 20-project corpus (`scripts/oss-clones.manifest`;
the SHAs are listed in `docs/dev/chk001-remove-and-test.md`)  
Scripts: `scripts/oss-differential.py` (`make oss-diff`) and
`scripts/oss-mutation-recall.py` (`make oss-mutation`)  
Parent: #346; the summary is in `docs/dev/oss-validation-report.md`

Neither script executes anything from the analyzed projects. They only read
the clones (mutation works on copies), so both are as safe as `make oss-metrics`.

## Differential oracle (#340)

Every checker runs over the same clones, its findings are normalized to one
key per chokkin rule, and each (rule, tool) pair is split into agree,
chokkin-only and other-only. The tools are pinned and run via `uvx`:
vulture 2.14, deadcode 2.4.1, deptry 0.23.0, fawltydeps 0.20.0, ruff 0.12.0
(`F401 --isolated`), pyflakes 3.4.0. chokkin runs at its default confidence.

The other tools scan everything under the root. Their findings in `tests/`,
`docs/`, `docs_src/`, `examples/`, `scripts/`, `build/` and similar
non-shipped paths are dropped first, because that is a difference of scope, not
a miss (`in_scope` in the script).

| Rule | Tool | chokkin | other | agree | chokkin-only | other-only | Jaccard |
|---|---|---:|---:|---:|---:|---:|---:|
| CHK002 unused dep | deptry DEP002 | 0 | 74 | 0 | 0 | 74 | 0.0% |
| CHK002 unused dep | fawltydeps | 0 | 81 | 0 | 0 | 81 | 0.0% |
| CHK003 missing dep | deptry DEP001 | 28 | 62 | 11 | 17 | 51 | 13.9% |
| CHK003 missing dep | fawltydeps | 28 | 54 | 22 | 6 | 32 | 36.7% |
| CHK004 transitive | deptry DEP003 | 0 | 0 | 0 | 0 | 0 | n/a |
| CHK005 misplaced | deptry DEP004 | 12 | 0 | 0 | 12 | 0 | 0.0% |
| CHK006 unused export | vulture | 3025 | 1373 | 323 | 2702 | 1050 | 7.9% |
| CHK006 unused export | deadcode | 3025 | 1344 | 296 | 2729 | 1048 | 7.3% |
| CHK007 unused re-export | ruff F401 | 173 | 35 | 12 | 161 | 23 | 6.1% |
| CHK007 unused re-export | pyflakes | 173 | 525 | 111 | 62 | 414 | 18.9% |

Low Jaccard values are expected where the tools ask a different question. The
triage below sorts each disagreement into "different claim", "different
scope" or "chokkin gap".

### Dependencies (CHK002 to CHK005)

- **CHK002 other-only (74 / 81): different dependency contexts.** chokkin has
  no CHK002 findings left on the corpus, which matches the §17 scorecard. deptry
  and fawltydeps flag the `test`/`docs`/`dev` extras (`coverage`,
  `hypothesis`, `sphinx*`, `pytest-mock`, `furo`, …) because those are not
  imported from the package. chokkin assigns such extras to dev contexts and
  only checks them against code in those contexts (README "Contexts"). deptry
  also lists anyio's own `anyio` self-dependency. None of the 74 is a runtime
  dependency that the package never imports.
- **CHK003 chokkin-only:**
  - **requests** (`certifi`, `charset_normalizer`, `idna`, `urllib3`):
    requests declares its dependencies as `install_requires=requires`, a
    variable, in `setup.py`. chokkin parses `setup.py` statically and skips a
    non-literal argument (spec §8), so it sees no dependencies. These are
    already labeled `deferred` in `scripts/oss-fixtures.labels.tsv`.
    fawltydeps matches 3 of the 4.
  - **django** (`jinja2`, `numpy`, `PIL`, `selenium`, `psycopg*`, …): optional
    backends imported by runtime modules. deptry's defaults ignore them, while
    fawltydeps agrees on most.
- **CHK003 other-only:**
  - First-party and vendored names the other tools cannot map, such as black's
    `blib2to3` and `_black_version`, and attrs' `attr`.
  - `TYPE_CHECKING` / `_typeshed` imports, which chokkin puts in the type
    context.
  - Optional imports guarded by `try/except ImportError`, which chokkin reports
    at `info`, below the default threshold.
- **CHK005 chokkin-only (12):**
  - djangorestframework, uvicorn, httpx and python-dotenv: runtime modules
    import packages that are declared only in development requirement files or
    groups (`coreapi`, `markdown`, `pygments`, `wsproto`, `trio`, `ipython`,
    …). chokkin calls that misplaced. deptry reads `requirements*.txt` as
    runtime dependencies, so its DEP004 has nothing to compare against.
  - fastapi: imports `anyio` at runtime but declares it only in
    `requirements-tests.txt`. At runtime it arrives transitively through
    starlette.
- **CHK004 (0 / 0):** no tool reports transitive-only imports on this corpus.
  chokkin's CHK004 is covered by the `lock_unused_*` sentinels of
  `scripts/oss-recall.manifest` (R-03), see [Gates](#gates).

### Symbols (CHK006, CHK007)

- **CHK006 chokkin-only (~2700): a different claim.** vulture and deadcode
  report names that are unused *anywhere*. CHK006 reports public module-level
  names that nothing *outside their module* uses, such as anyio's `BACKENDS`
  and `T_Retval`. The first set is roughly a subset of the second, so a large
  chokkin-only count is expected. django (1446), black (267) and jinja (216)
  account for most of it.
- **CHK006 other-only (~1050): mostly dynamic or framework use.**
  - django (1149 of 1373 vulture findings): mostly constants in
    `django/conf/locale/*/formats.py`. Those modules are only reached through
    `import_module(f"...{lang}.formats")`, so chokkin reports the whole file as
    CHK001 (84 of django's 246 package-level CHK001 findings). Symbol rules do
    not run on unreachable files, so there is no CHK006 for them.
  - black `schema.get_schema` is a `validate_pyproject.tool_schema`
    entry point (`pyproject.toml`). chokkin counts entry-point targets as
    used, while vulture reports a false positive.
  - anyio `_backends/_trio.py` is loaded through `import_module`, and chokkin
    conservatively treats the exports of such modules as used.
  - click `P = ParamSpec("P")` is bound inside `if TYPE_CHECKING:` and is not
    a runtime module-level name.

  None of the sampled entries is ordinary dead code that chokkin misses.
  CHK006 recall on ordinary code is measured directly in
  [Mutation recall](#mutation-recall-341).
- **CHK007 vs ruff (chokkin-only 161, other-only 23): a different claim.**
  - Ruff's F401 honors the explicit re-export conventions (`import X as X`,
    `__all__`) and skips such names. CHK007 asks whether anything in the
    project consumes the re-export. anyio's `BrokenWorkerProcess as
    BrokenWorkerProcess` is a convention-marked public name that the project
    itself never uses: it is chokkin-only.
  - The ruff-only names are requests' `__init__` imports (`Request`,
    `Session`, `__author__`, …), which carry no `as X` / `__all__` marker. Ruff
    flags them as unused within `__init__`. chokkin does not report them at the
    default confidence, because they are consumed elsewhere in the project or
    reported only at `maybe`.
- **CHK007 vs pyflakes (other-only 414):** pyflakes does not honor
  `import X as X`, so it reports every convention-marked re-export, for
  example all of anyio's public `__init__`. chokkin, like ruff, treats them as
  the API.

## Mutation recall (#341)

For every project, a disposable copy gets one injection per rule, and the
script checks that chokkin reports the expected (rule, subject) key that is
**new** compared to the unmutated baseline. Each injection runs in its own
fresh copy. Recall = detected / applied. The run is repeated with
`--confidence maybe`.

| Mutation | Kind | Rule | Applied | Default | Recall | `maybe` | Recall |
|---|---|---|---:|---:|---:|---:|---:|
| unimported module (`<pkg>/chokkin_mut_orphan.py`) | injection | CHK001 | 20 | 8 | 40.0% | 20 | **100.0%** |
| unused dependency (`chokkin-mut-unused>=1`) | injection | CHK002 | 14 | 14 | **100.0%** | 14 | 100.0% |
| undeclared import (`import xmltodict` in `__init__`) | injection | CHK003 | 20 | 18 | 90.0% | 18 | 90.0% |
| unresolved import (`import chokkin_mut_missing`) | injection | CHK010 | 20 | 18 | 90.0% | 18 | 90.0% |
| unreferenced function in the largest module | injection | CHK006 | 20 | 17 | 85.0% | 17 | 85.0% |
| unused re-export (`from .chokkin_mut_src import …`) | injection | CHK007 | 20 | 18 | 90.0% | 18 | 90.0% |
| `importlib.import_module("<pkg>.chokkin_mut_plugin")` | trap | CHK001 | 20 | 0 | **0.0%** | 2 | 10.0% |
| declare `PyYAML`/`Pillow`, import `yaml`/`PIL` | trap | CHK002/CHK003 | 14 | 1 | 7.1% | 1 | 7.1% |

For a trap, the rate is how often chokkin *wrongly* fires, so lower is better.
CHK004 is not injected: it needs a lockfile, and the R-03 `lock_unused_*`
sentinels already pin it (see [Gates](#gates)).

The unused-dependency and dist-name traps were **not applied** to requests,
click, python-dotenv, tenacity, pluggy and djangorestframework. Those projects
have no static `[project].dependencies` array to edit, because their
dependencies are dynamic or live in `setup.py`/`setup.cfg`.

### Misses, by cause

1. **CHK001 in library mode is `maybe` by design (12 of 20 projects).** An
   orphan module in a library's package is only reported at `maybe`, because
   users may import it. At `--confidence maybe`, recall is 20/20. This is the
   intended trade-off of spec §10, not a defect.
2. **urllib3 and pluggy: no entry root, so nothing is evaluated (11 misses
   plus 2 trap triggers).**
   - Both are library-mode packages whose tests live in `test/` and `testing/`.
     Test discovery does not cover those directories (`--probe`: "contexts:
     runtime 35, test 0").
   - The package itself is not an entry root in library mode
     (`src/reachability/build.rs`), so nothing is reachable.
   - The trace for an injected file reads "not reachable from any entry root;
     entry roots analyzed: (none)".
   - Symbol and import rules run only on reachable modules, so every
     injection is missed. For the same reason, urllib3's dist-name trap fires
     CHK002 `pillow`/`pyyaml`: the imports exist, but nothing reachable uses
     them.
   - The `maybe`-level trap triggers (`chokkin_mut_plugin.py`) are the same
     effect.
   - **Follow-up:** make the distributed package an entry root in library
     mode (its public modules are the API), and/or discover `test/` and
     `testing/` as test roots.
3. **anyio CHK006:** the function was injected into
   `src/anyio/_backends/_asyncio.py`, a backend loaded through `importlib`.
   chokkin deliberately treats every export of a dynamically loaded module as
   used. This is a conservative miss, the same mechanism as the CHK006
   other-only entries above.

Apart from those two projects, recall is 100% for CHK002, CHK003, CHK010 and
CHK007, and 17/18 for CHK006. The dynamic-import trap never fires at the
default confidence, and the dist ≠ import-name trap (`PyYAML`→`yaml`,
`Pillow`→`PIL`) never fires in any project whose package is reachable.

## Gates

`scripts/oss-gate.py` (`make oss-gate`) runs chokkin three times on each of
the 20 clones and on the 10 in-repo recall sentinels: cold (cache removed),
warm (cache reused) and `--no-cache`. It then gates the 90 JSON outputs:

| Gate | Criterion | Measured | Result |
|---|---|---|---|
| determinism | cold / warm / `--no-cache` byte-identical | 30/30 identical | PASS |
| schema | valid against `docs/schema/chokkin-report.schema.json`; `summary.total`/`by_code` agree with `issues` | 30/30 valid | PASS |
| crash | no exit 3 and no non-JSON output | 0 crashed | PASS |

The recall sentinels include CHK004 (`lock_unused_pylock`, `lock_unused_poetry`,
`lock_unused_pdm`). `make oss-metrics` gates their expected findings and the
determinism gate gates their stability.

`scripts/bench-gate.py` (`make bench-gate`) is the performance pass/fail. It
compares the current tree with a criterion baseline saved on the base commit
(`make bench-save BASELINE=main`), and fails when a benchmark's mean is more
than 10% slower and its 95% confidence interval lies entirely above zero.
Result for this branch: see [`oss-validation-report.md`](./oss-validation-report.md#346-corpus-measurements-338342).

## Reproducing

```bash
make oss-clones
make oss-diff        # -> target/oss-diff/{report.md,summary.json,findings.tsv,raw/}
make oss-mutation    # -> target/oss-mutation/{report.md,summary.json,results.tsv}
make oss-gate        # -> target/oss-gate/
```

`make oss-diff` needs `uv` (for `uvx`) and network access the first time each
pinned tool is fetched. When `target/oss-envs/<slug>` exists, fawltydeps maps
imports through that venv. Other tools' raw output is kept under
`target/oss-diff/raw/<slug>/` so that individual disagreements can be checked.
