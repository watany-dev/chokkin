# chokkin

[日本語](./README.ja.md)

Find unused files, dependencies, and public symbols in Python projects.

`chokkin` is a reachability analyzer for whole Python projects — a [Knip](https://knip.dev/)-like experience for Python. It builds a project-wide graph from your manifests, source code, and tool configs, then reports what nothing reaches: run `uvx chokkin` with zero configuration, and tighten things up with precise settings and CI integration as you go.

> [!NOTE]
> **Status: v0.2 development.** `chokkin` runs the **full analysis pipeline** (steps 1–13) by default: unused files, dependencies, and symbols with built-in reporters (`default`, `compact`, `json`, `markdown`, `github`, `sarif`), plus `--explain`, `--trace`, `--fix`, and baseline filtering. Use `--probe` for steps 1–4 summary only; it now reports resolved workspace member counts, and resolver tags member-owned imports while treating cross-member imports as first-party. Strict mode enforces member-local dependency declarations, and reporters expose member ids on workspace findings. The §17 **CHK002 false-positive gate passed** after Phase 1.5 (`make oss-metrics ARGS=--gate`), and **v0.1.0 has been released**.

## Why chokkin?

Existing tools each cover one slice of the problem:

```text
Ruff     : per-file, syntax-level linting
Vulture  : Python AST-based dead code detection
deptry   : consistency between dependency manifests and imports
chokkin    : unused files, dependencies, and public symbols from the whole project graph
```

`chokkin` is not a style/lint tool. It answers a different question: starting from your entry points, what can actually be reached — and what is just sitting there? It reads `pyproject.toml`, requirements files, uv/Poetry lockfiles, and framework/tool configs (Django, FastAPI, pytest, tox, nox, pre-commit, GitHub Actions, …) to build that picture.

## Quick start

```bash
uvx chokkin
```

No configuration needed. On first run, chokkin discovers your manifests (`pyproject.toml`, `setup.cfg`, `setup.py`, `requirements*.txt`, `uv.lock`), infers your layout (src/flat, tests, scripts, docs), infers entry points, builds the import graph, and reconciles it against your declared dependencies:

```text
chokkin 0.1.0

Project: acme-api
Config : pyproject.toml
Mode   : auto, production=false

Unused dependencies  3
  boto3          pyproject.toml:18  declared in [project.dependencies], no reachable import found
  rich           pyproject.toml:25  only used by scripts/dev.py; move to dependency-groups.dev
  python-dotenv  pyproject.toml:29  no import/config/binary usage found

Missing dependencies  1
  yaml -> PyYAML  src/acme/config.py:3  imported but not declared

Unused files  2
  src/acme/legacy.py        no path from any entry point
  src/acme/old_handlers.py  no path from any entry point

Unused exports  4
  src/acme/utils.py:12  function legacy_slugify
  src/acme/auth.py:44   class OldTokenBackend

Summary: 10 issues
```

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
| `CHK009` | `duplicate_dependency`  | declared in multiple of main/dev/optional                                 | warning                       |
| `CHK010` | `unresolved_import`     | import that resolves to neither first-party, third-party, nor stdlib      | warning                       |

Because any module top-level name is importable in Python, `unused_export` starts out as a preview rule (info-level in library mode) rather than a hard error.

## CLI

```bash
uvx chokkin
uvx chokkin --production
uvx chokkin --strict
uvx chokkin --no-exit-code
uvx chokkin --include CHK002,CHK003
uvx chokkin --exclude CHK006
uvx chokkin --reporter json
uvx chokkin --reporter markdown
uvx chokkin --reporter github
uvx chokkin --reporter sarif
uvx chokkin --confidence likely
uvx chokkin --fix
uvx chokkin --fix --dry-run
uvx chokkin --baseline chokkin-baseline.json
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --no-cache
uvx chokkin --explain CHK002:boto3
uvx chokkin --trace src/acme/legacy.py
uvx chokkin --probe              # steps 1–4 summary only
uvx chokkin --init                # v0.2
```

Key flags:

- `--production` — drop dev/test/docs/lint/type contexts and judge reachability from runtime context only. Dev-only files and dependencies are no longer reported, and "unused in production" becomes strict.
- `--strict` — direct imports of transitive dependencies always error, workspace members must declare their own dependencies, unused environment-marker dependencies error, and `maybe`-confidence issues are shown.
- `--no-exit-code` — exit 0 even when issues are found (config/CLI errors still exit 2, internal errors 3). Useful during adoption and for GitHub Actions summaries.
- `--baseline PATH` / `--update-baseline` — freeze current issues in a baseline file and suppress matching issues on later runs so CI fails only on new findings.
- `--no-cache` — disable Phase 2 cache reads/writes. Cache policy plumbing is present; parse/manifest cache units are still draft.
- `--reporter github` / `--reporter sarif` — emit GitHub Actions annotations or a SARIF 2.1.0 subset for code scanning.
- `--probe` — include resolved and inventoried workspace member counts when uv or chokkin workspaces are detected.
- `--explain` / `--trace` — show why an issue was reported / why a file is considered reachable. These are the intended path for investigating and reporting false positives.

Exit codes are fixed for CI:

```text
0: no reportable issues
1: issues found
2: CLI/config error
3: internal error
```

## Configuration

Zero config is the default. When you need precision, configure `[tool.chokkin]` in `pyproject.toml` (standalone `chokkin.toml` / `.chokkin.toml` are also accepted). `chokkin --init` appends a starter `[tool.chokkin]` reflecting what auto-discovery found.

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

[tool.chokkin.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution name -> import name(s), for cases the bundled map doesn't cover
[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]

# CLI name -> distribution name, used by CHK008/CHK002 binary-usage checks
[tool.chokkin.binary_map]
"sphinx-build" = "Sphinx"

[tool.chokkin.plugins]
pytest = true
django = true
fastapi = true
```

### Modes

`mode = "auto"` picks one of:

- **app mode** — there's a clear entry (`console_scripts`, `manage.py`, `asgi.py`, `wsgi.py`, `app.py`). Unused files are reported aggressively.
- **library mode** — a `[project] name` with a package and no clear entry. Public modules may be imported by external users, so unused files/exports are reported at low confidence (or as info). For serious unused-file detection in a library, declare `entry` explicitly.
- **workspace mode** — multiple `pyproject.toml` files or `tool.uv.workspace.members`. Each member is analyzed separately (per-member `[tool.chokkin.workspaces.<name>]` config is supported), sharing the workspace lockfile.

### Dependency contexts

Dependencies and files are both assigned contexts (runtime / dev / test / docs / lint / type / optional extras). That's what powers `CHK005`: `import pytest` in `tests/` with pytest in your dev group is fine; the same import in `src/` is a misplaced dependency. `TYPE_CHECKING`-only imports are type-context, and `try: import orjson / except ImportError` is treated as optional rather than missing.

## Plugins

Frameworks reference modules through strings and decorators, which pure import analysis can't see. Plugins close that gap by adding entry files, string/module references, and binary usage:

- **v0.1**: pytest, django, fastapi/uvicorn
- **v0.2+**: tox/nox/pre-commit binary usage detection is in progress; flask, celery, github-actions, sphinx, mkdocs, and alembic remain planned.

For example, the Django plugin treats `INSTALLED_APPS` / `MIDDLEWARE` / `ROOT_URLCONF` strings as module references and `migrations/**` as framework-used; the FastAPI plugin treats `@router.get`-decorated handlers as externally used.

## Suppressing issues

Inline and file-level ignores:

```python
from legacy import old_api  # chokkin: ignore[CHK003]

# chokkin: file-ignore[CHK006]   (at the top of a file)
```

Config ignores, keyed by rule code (globs over distribution names, paths, or `path:symbol`):

```toml
[tool.chokkin.ignore]
CHK001 = ["src/acme/generated/**/*.py"]
CHK002 = ["boto3", "google-cloud-*"]
CHK006 = ["src/acme/public_api.py:*"]
```

For large existing projects, a baseline freezes current issues so CI only fails on new ones (v0.2):

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --baseline chokkin-baseline.json
```

## Installation

`chokkin` is a single Rust binary shipped inside a Python wheel (prebuilt for Linux/macOS/Windows), so all of these work without a Rust toolchain:

```bash
uvx chokkin        # run without installing
pipx run chokkin
pip install chokkin
```

chokkin never executes your project's code — analysis is fully static. It also doesn't require your project's virtualenv: if `.venv` exists it is read for dist-info metadata (`METADATA`, `top_level.txt`, `RECORD`, `entry_points.txt`), otherwise manifests, lockfiles, and bundled maps are used.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). The full design specification (analysis engine, import resolution strategy, roadmap) is in [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) (Japanese).

## License

[MIT](./LICENSE)
