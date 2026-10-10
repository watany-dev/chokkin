# chokkin

[日本語](./README.ja.md)

[![PyPI](https://img.shields.io/pypi/v/chokkin)](https://pypi.org/project/chokkin/)
[![CI](https://github.com/watany-dev/chokkin/actions/workflows/ci.yml/badge.svg)](https://github.com/watany-dev/chokkin/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](./LICENSE)

**Find unused files, dependencies, and public symbols in Python projects.**

```bash
uvx chokkin
```

That is the whole setup. chokkin reads your `pyproject.toml`, requirements
files, lockfile, and tool configs, builds a reachability graph of the entire
project from its entry points, and reports what nothing reaches. It is
[Knip](https://knip.dev/) for Python: one command, zero configuration, and a
clear path to a CI gate when you want one.

- **Zero config.** Layout, entry points, dependency groups, and frameworks are
  detected automatically. Add `[tool.chokkin]` only when you need precision.
- **Whole-project view.** Not "is this import used in this file" but "can this
  file, package, or symbol be reached from anything that runs".
- **Never runs your code.** Analysis is fully static. Django settings,
  `setup.py`, and notebooks are parsed, not imported.
- **Fast and self-contained.** A single Rust binary shipped as a Python wheel
  for Linux, macOS, and Windows. No Rust toolchain, no project virtualenv
  required.
- **Built for CI.** Baselines, GitHub annotations, SARIF, fixed exit codes, and
  a bundled GitHub Action.

## What you get

```text
chokkin 0.7.3

Project: acme-api
Config : pyproject.toml
Mode   : app, production=false

Unused files  2
  src/acme/legacy.py       src/acme/legacy.py     file `src/acme/legacy.py` is not reachable from any entry root
  src/acme/old_handlers.py src/acme/old_handlers.py file `src/acme/old_handlers.py` is not reachable from any entry root

Unused dependencies  3
  boto3                    pyproject.toml         declared in project.dependencies[1], no reachable import, config, or binary usage found
  python-dotenv            pyproject.toml         declared in project.dependencies[3], no reachable import, config, or binary usage found
  rich                     pyproject.toml         declared in project.dependencies[2], no reachable import, config, or binary usage found

Missing dependencies  1
  src/acme/config.py:3 yaml src/acme/config.py:3   imported pyyaml in src/acme/config.py:3 (no lockfile — transitive check skipped) but not declared in matching dependency context

Unused exports  2
  acme.auth:OldTokenBackend src/acme/auth.py:5     public class `OldTokenBackend` in `acme.auth` is not referenced from outside the module
  acme.utils:legacy_slugify src/acme/utils.py:5    public function `legacy_slugify` in `acme.utils` is not referenced from outside the module

Summary: 8 issues
```

Every finding has a rule code, a location, and a reason. When you disagree
with one, `--explain` and `--trace` show the evidence behind it.

## Why not Ruff, Vulture, or deptry?

They are good tools that answer different questions:

| Tool    | Scope                                                          |
|---------|----------------------------------------------------------------|
| Ruff    | per-file, syntax-level linting (unused imports, unused locals) |
| Vulture | dead code inside Python files, from the AST                    |
| deptry  | declared dependencies vs. imports                              |
| chokkin | unused files, dependencies, and public symbols from the whole project graph |

chokkin starts from your entry points (`console_scripts`, `manage.py`,
`asgi.py`, test suites, notebooks, CI commands, …) and asks what is reachable.
A module that nothing imports, a package that nothing uses, a class that no
other module references: those are the findings. Because it also reads
framework and tool configuration, string references like Django's
`INSTALLED_APPS` or a `pre-commit` hook running `mypy` count as usage instead
of showing up as false positives.

## Install

```bash
uvx chokkin          # run without installing
pipx run chokkin
pip install chokkin
```

Python 3.10+ is required to install the wheel. The analyzed project can target
any Python version (`target_version` in the config).

chokkin does not need your project's virtualenv. If `.venv` exists it reads
dist-info metadata from it; otherwise it works from manifests, lockfiles, and a
bundled distribution-to-module map.

## What it checks

| Code     | Issue                   | Description                                                              | Default severity              |
|----------|-------------------------|--------------------------------------------------------------------------|-------------------------------|
| `CHK001` | `unused_file`           | Python file not reachable from any entry point                            | warning                       |
| `CHK002` | `unused_dependency`     | declared in a manifest, but no import/config/binary usage found           | error                         |
| `CHK003` | `missing_dependency`    | imported, but not declared directly in any manifest                       | error                         |
| `CHK004` | `transitive_dependency` | imported directly, but only available via another dependency              | error                         |
| `CHK005` | `misplaced_dependency`  | runtime code uses a dev-group dependency, or a test-only dep is in main   | warning                       |
| `CHK006` | `unused_export`         | public symbol not referenced from outside its module                      | warning                       |
| `CHK007` | `unused_reexport`       | re-export (e.g. in `__init__.py`) not referenced internally               | library: info / app: warning  |
| `CHK008` | `unlisted_binary`       | CLI used by tox/nox/pre-commit/CI without a declared dependency           | warning                       |
| `CHK009` | `duplicate_dependency`  | declared twice in one context, or in runtime and a group/extra            | one context: warning / runtime and group/extra: info |
| `CHK010` | `unresolved_import`     | import that resolves to neither first-party, third-party, nor stdlib      | `TYPE_CHECKING` or optional (`try` / `suppress(ImportError)` / `find_spec` / `is_*_available()`): info / else: warning |

Each finding also carries a confidence (`certain`, `likely`, `maybe`). By
default `maybe` findings are hidden; `--confidence` and `--strict` change that.

Python makes every module top-level name importable, so `unused_export` is
deliberately cautious: in library mode it is info-level, and names the defining
module itself reads (a TypeVar, a type alias, a helper) are not reported.

## What it understands

**Manifests and lockfiles:** `pyproject.toml` (PEP 621, Poetry, PDM, Hatch,
`[tool.uv]` sources and constraints), PEP 735 dependency groups including
`include-group`, PEP 723 inline script metadata, `setup.cfg`, static
`setup.py`, `requirements*.txt` / `*.in`, and `uv.lock`, `pylock.toml`,
`poetry.lock`, or `pdm.lock` for the transitive check.

**Layouts:** src and flat layouts, tests, scripts, docs, examples, Jupyter
notebooks, build-backend package directories, and uv workspaces or
auto-detected monorepo members, each analyzed with its own manifest.

**Frameworks and tools:** pytest, Django, FastAPI / uvicorn, Flask, Celery,
Sphinx, MkDocs, Alembic, tox, nox, pre-commit, and GitHub Actions. Plugins
add entry files, string module references, and binary usage that plain import
analysis cannot see, and they switch on automatically when the framework is a
declared dependency. The Django plugin, for example, treats `INSTALLED_APPS`,
`MIDDLEWARE`, and `ROOT_URLCONF` strings as module references and
`migrations/**` as framework-used; FastAPI and Flask route handlers count as
externally used.

**Dependency contexts:** both dependencies and files are assigned a context
(runtime / dev / test / docs / lint / type / optional extras). That is what
powers `CHK005`: `import pytest` under `tests/` with pytest in your dev group
is fine, the same import under `src/` is a misplaced dependency.
`TYPE_CHECKING`-only imports are type context, and guarded imports
(`try: import orjson`, `find_spec(...)`, platform checks) stay informational.

## Everyday usage

```bash
uvx chokkin                          # analyze the current directory
uvx chokkin path/to/project
uvx chokkin --reporter compact       # one line per finding
uvx chokkin --include CHK002,CHK003  # only dependency findings
uvx chokkin --exclude CHK006
uvx chokkin --production             # runtime context only
uvx chokkin --strict                 # stricter policies, show `maybe` findings
```

**Investigate a finding** before you trust it:

```bash
uvx chokkin --explain CHK002:boto3        # why is boto3 unused? which imports were seen?
uvx chokkin --trace src/acme/legacy.py    # how (or why not) is this file reached?
```

`--trace` prints the import chain from an entry root for reachable files, and
the reason, entry roots, and incoming imports for unreachable ones. This is
the intended path for reporting false positives.

**Fix it** when you agree:

```bash
uvx chokkin --fix --dry-run               # preview
uvx chokkin --fix                         # remove certain unused dependencies from manifests
uvx chokkin --fix --add-missing           # also declare certain missing dependencies
uvx chokkin --fix --allow-remove-files    # also delete certain unreachable files
```

`--fix` is conservative by design: it only touches `certain` findings, refuses
to edit a manifest whose line numbers no longer match, preserves line endings,
and reports anything it skipped on stderr.

**Key flags:**

- `--production` drops dev/test/docs/lint/type contexts and judges
  reachability from runtime code only. Dev-only files and dependencies
  disappear, and "unused in production" becomes exact.
- `--strict` makes direct imports of transitive dependencies an error, requires
  workspace members to declare their own dependencies, reports undeclared
  type/test/docs/dev imports as `CHK003`, errors on unused
  environment-marker dependencies, and shows `maybe` findings.
- `--reporter default|compact|json|markdown|github|sarif` picks the output.
  `github` emits workflow annotations; `sarif` writes a SARIF 2.1.0 subset for
  code scanning.
- `--no-exit-code` returns 0 even with findings, for adoption periods and
  summaries.
- `--no-cache` disables the on-disk parse cache under `.chokkin/`. The cache is
  keyed on file stat and treats anything stale or corrupt as a miss.
- `--no-auto-workspace` stops nested `pyproject.toml` files from becoming
  workspace members or being skipped as separate projects.
- `--init` appends a starter `[tool.chokkin]` reflecting what auto-discovery
  found.

Exit codes are fixed for CI:

```text
0: no reportable issues
1: issues found
2: CLI/config error
3: internal error
```

## Adopting chokkin in an existing project

Large projects rarely start clean. Freeze what exists today in a baseline so
CI fails only on new findings:

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
git add chokkin-baseline.json
```

Then gate pull requests with the bundled GitHub Action. It emits annotations,
writes SARIF for code scanning, and fails only for findings not in the
baseline:

```yaml
name: chokkin

on:
  pull_request:

permissions:
  contents: read
  security-events: write

jobs:
  chokkin:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: watany-dev/chokkin@f6d097a5595fa4c1d6a7d3b67154d9b30e5c1d46 # v0.7.3
        with:
          baseline: chokkin-baseline.json
          sarif-file: chokkin.sarif
      - uses: github/codeql-action/upload-sarif@2892aa5e19bbd11bc0cff5427e3b750a04d9e3c2 # v4.38.2
        if: always()
        with:
          sarif_file: chokkin.sarif
```

Pin the action to a full commit SHA with the tag as a comment, as above: a tag
can be moved, a SHA cannot, and Dependabot still bumps both.

| Input | Default | Description |
| --- | --- | --- |
| `version` | release of the action ref | chokkin version from PyPI, or `latest` |
| `working-directory` | `.` | Project root to analyze |
| `baseline` | — | Baseline file; only new findings fail the job |
| `reporter` | `github` | Reporter for the gating run |
| `sarif-file` | — | Also write SARIF here (written even when the gating run fails) |
| `args` | — | Extra CLI arguments, e.g. `--production --confidence likely` |
| `cache` | `false` | `true` reads and writes the `.chokkin/` cache; off so cache files in a PR cannot change the result |

Without the action, pin the version with `uvx`:

```yaml
      - uses: astral-sh/setup-uv@c18668ad3cf93ea998bef934396af7bb5c839dc7 # v10.2.0
      - run: uvx chokkin@0.7.3 --baseline chokkin-baseline.json --reporter github
```

Baseline files and `--reporter json` output carry `schema_version: "1"` and
follow the published JSON Schemas under [`docs/schema/`](./docs/schema/).

## Suppressing issues

Inline and file-level comments:

```python
from legacy import old_api  # chokkin: ignore[CHK003]

# chokkin: file-ignore[CHK006]   (at the top of a file)
```

Config ignores, keyed by rule code (globs over distribution names, paths, or
`path:symbol`):

```toml
[tool.chokkin.ignore]
CHK001 = ["src/acme/generated/**/*.py"]
CHK002 = ["boto3", "google-cloud-*"]
CHK006 = ["src/acme/public_api.py:*"]
```

Per-rule severity, including turning a rule off:

```toml
[tool.chokkin.severity]
CHK001 = "off"
CHK006 = "info"
```

## Configuration

Zero config is the default. When you need precision, add `[tool.chokkin]` to
`pyproject.toml` (a standalone `chokkin.toml` or `.chokkin.toml` also works).
`chokkin --init` writes a starter section for you. Everything below is
optional:

```toml
[tool.chokkin]
entry = [
  "src/acme/__main__.py",
  "src/acme/asgi.py:application",
  "manage.py",
]
project = [
  "src/**/*.py",
  "tests/**/*.py",
  "scripts/**/*.py",
]
mode = "auto"             # auto | app | library
production = false
target_version = "py311"  # Python version of the analyzed project
respect_gitignore = true
confidence = "likely"     # certain | likely | maybe
exclude = [
  ".venv/**",
  "build/**",
  "dist/**",
  "**/__pycache__/**",
]
vendored = [             # analyzed and traced, but never reported
  "**/_vendor/**",
  "**/third_party/**",
]

[tool.chokkin.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution name -> import name(s), for cases the bundled map doesn't cover
[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]
"protobuf" = ["google.protobuf"]  # dotted names pick one distribution under a namespace package

# CLI name -> distribution name, used by CHK008/CHK002 binary-usage checks
[tool.chokkin.binary_map]
"sphinx-build" = "Sphinx"

[tool.chokkin.plugins]
pytest = true
django = true
fastapi = true

[tool.chokkin.severity]
CHK001 = "off"
CHK006 = "info"
```

The root `.chokkin/` directory holds analyzer data and is always excluded.

### Modes

`mode = "auto"` picks one of:

- **app** when there is a clear entry (`console_scripts`, `manage.py`,
  `asgi.py`, `wsgi.py`, `app.py`). Unused files are reported aggressively.
- **library** when there is a `[project] name` with a package and no clear
  entry. Public modules may be imported by users you cannot see, so unused
  files and exports are reported at low confidence or as info. Declare
  `entry` explicitly for serious unused-file detection in a library.
- **workspace** when there are multiple `pyproject.toml` files or
  `tool.uv.workspace.members`. Each member is analyzed separately against the
  shared lockfile, with per-member settings under
  `[tool.chokkin.workspaces.<name>]`. Repositories without a workspace
  declaration get nested `pyproject.toml` files with a `[project]` name (up to
  four directories deep) as members automatically; nested projects that are
  not members, such as an example app with its own `[project]`, are left out
  with a warning.

## Known limits

Python is dynamic, and chokkin is static. Things it cannot see:

- Modules loaded by name from runtime data (a plugin registry read from a
  database, `importlib.import_module(f"{pkg}.{name}")` with a non-literal
  name). Declare these as `entry` or ignore the rule for that path.
- Code reached only through a framework or tool chokkin has no plugin for.
  `--trace` shows exactly why a file was judged unreachable, which is usually
  enough to pick the right `entry` or `ignore`.
- Users of a library's public API. That is why library mode downgrades
  `CHK001` / `CHK006` rather than erroring.

If a finding looks wrong and `--explain` / `--trace` do not settle it, please
[open an issue](https://github.com/watany-dev/chokkin/issues) with their
output.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). The full design specification
(analysis engine, import resolution strategy, roadmap) is in
[`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) (Japanese), and design
decisions are recorded under [`docs/adr/`](./docs/adr/).

## License

[MIT](./LICENSE)
