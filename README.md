# yokei

[日本語](./README.ja.md)

Find unused files, dependencies, and public symbols in Python projects.

`yokei` is a reachability analyzer for whole Python projects — a [Knip](https://knip.dev/)-like experience for Python. It builds a project-wide graph from your manifests, source code, and tool configs, then reports what nothing reaches: run `uvx yokei` with zero configuration, and tighten things up with precise settings and CI integration as you go.

> [!WARNING]
> **Status: pre-alpha.** Running `yokei` executes **probe mode**: pipeline steps 1–4 (discovery, config, manifest, sources) and prints a project summary. Step 5 (`src/plugins/` config/plugin extraction) is available as a library API. Full unused dependency/file analysis and issue reporting are not implemented yet. Phase 0 also adds `src/graph/` and `src/parser/` skeletons. The command output below shows the **target** UX once analysis is complete; see [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md).

## Why yokei?

Existing tools each cover one slice of the problem:

```text
Ruff     : per-file, syntax-level linting
Vulture  : Python AST-based dead code detection
deptry   : consistency between dependency manifests and imports
yokei    : unused files, dependencies, and public symbols from the whole project graph
```

`yokei` is not a style/lint tool. It answers a different question: starting from your entry points, what can actually be reached — and what is just sitting there? It reads `pyproject.toml`, requirements files, uv/Poetry lockfiles, and framework/tool configs (Django, FastAPI, pytest, tox, nox, pre-commit, GitHub Actions, …) to build that picture.

## Quick start

```bash
uvx yokei
```

No configuration needed. On first run, yokei discovers your manifests (`pyproject.toml`, `setup.cfg`, `setup.py`, `requirements*.txt`, `uv.lock`), infers your layout (src/flat, tests, scripts, docs), infers entry points, builds the import graph, and reconciles it against your declared dependencies:

```text
yokei 0.1.0

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
| `YOK001` | `unused_file`           | Python file not reachable from any entry point                            | warning                       |
| `YOK002` | `unused_dependency`     | declared in a manifest, but no import/config/binary usage found           | error                         |
| `YOK003` | `missing_dependency`    | imported, but not declared directly in any manifest                       | error                         |
| `YOK004` | `transitive_dependency` | imported directly, but only available via another dependency              | error                         |
| `YOK005` | `misplaced_dependency`  | runtime code uses a dev-group dependency, or a test-only dep is in main   | warning                       |
| `YOK006` | `unused_export`         | public symbol not referenced from outside its module                      | warning                       |
| `YOK007` | `unused_reexport`       | re-export (e.g. in `__init__.py`) not referenced internally               | library: info / app: warning  |
| `YOK008` | `unlisted_binary`       | CLI used by tox/nox/pre-commit/CI without a declared dependency           | warning                       |
| `YOK009` | `duplicate_dependency`  | declared in multiple of main/dev/optional                                 | warning                       |
| `YOK010` | `unresolved_import`     | import that resolves to neither first-party, third-party, nor stdlib      | warning                       |

Because any module top-level name is importable in Python, `unused_export` starts out as a preview rule (info-level in library mode) rather than a hard error.

## CLI

```bash
uvx yokei
uvx yokei --production
uvx yokei --strict
uvx yokei --fix
uvx yokei --fix --allow-remove-files
uvx yokei --include dependencies,files
uvx yokei --exclude exports
uvx yokei --reporter json
uvx yokei --reporter sarif   # v0.2
uvx yokei --no-exit-code
uvx yokei --explain YOK002:boto3
uvx yokei --trace src/acme/legacy.py
uvx yokei --init
```

Key flags:

- `--production` — drop dev/test/docs/lint/type contexts and judge reachability from runtime context only. Dev-only files and dependencies are no longer reported, and "unused in production" becomes strict.
- `--strict` — direct imports of transitive dependencies always error, workspace members must declare their own dependencies, unused environment-marker dependencies error, and `maybe`-confidence issues are shown.
- `--no-exit-code` — exit 0 even when issues are found (config/CLI errors still exit 2, internal errors 3). Useful during adoption and for GitHub Actions summaries.
- `--explain` / `--trace` — show why an issue was reported / why a file is considered reachable. These are the intended path for investigating and reporting false positives.

Exit codes are fixed for CI:

```text
0: no reportable issues
1: issues found
2: CLI/config error
3: internal error
```

## Configuration

Zero config is the default. When you need precision, configure `[tool.yokei]` in `pyproject.toml` (standalone `yokei.toml` / `.yokei.toml` are also accepted). `yokei --init` appends a starter `[tool.yokei]` reflecting what auto-discovery found.

```toml
[tool.yokei]
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

[tool.yokei.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution name -> import name(s), for cases the bundled map doesn't cover
[tool.yokei.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]

# CLI name -> distribution name, used by YOK008/YOK002 binary-usage checks
[tool.yokei.binary_map]
"sphinx-build" = "Sphinx"

[tool.yokei.plugins]
pytest = true
django = true
fastapi = true
```

### Modes

`mode = "auto"` picks one of:

- **app mode** — there's a clear entry (`console_scripts`, `manage.py`, `asgi.py`, `wsgi.py`, `app.py`). Unused files are reported aggressively.
- **library mode** — a `[project] name` with a package and no clear entry. Public modules may be imported by external users, so unused files/exports are reported at low confidence (or as info). For serious unused-file detection in a library, declare `entry` explicitly.
- **workspace mode** — multiple `pyproject.toml` files or `tool.uv.workspace.members`. Each member is analyzed separately (per-member `[tool.yokei.workspaces.<name>]` config is supported), sharing the workspace lockfile.

### Dependency contexts

Dependencies and files are both assigned contexts (runtime / dev / test / docs / lint / type / optional extras). That's what powers `YOK005`: `import pytest` in `tests/` with pytest in your dev group is fine; the same import in `src/` is a misplaced dependency. `TYPE_CHECKING`-only imports are type-context, and `try: import orjson / except ImportError` is treated as optional rather than missing.

## Plugins

Frameworks reference modules through strings and decorators, which pure import analysis can't see. Plugins close that gap by adding entry files, string/module references, and binary usage:

- **v0.1**: pytest, django, fastapi/uvicorn
- **v0.2+**: flask, celery, tox, nox, pre-commit, github-actions, sphinx, mkdocs, alembic

For example, the Django plugin treats `INSTALLED_APPS` / `MIDDLEWARE` / `ROOT_URLCONF` strings as module references and `migrations/**` as framework-used; the FastAPI plugin treats `@router.get`-decorated handlers as externally used.

## Suppressing issues

Inline and file-level ignores:

```python
from legacy import old_api  # yokei: ignore[YOK003]

# yokei: file-ignore[YOK006]   (at the top of a file)
```

Config ignores, keyed by rule code (globs over distribution names, paths, or `path:symbol`):

```toml
[tool.yokei.ignore]
YOK001 = ["src/acme/generated/**/*.py"]
YOK002 = ["boto3", "google-cloud-*"]
YOK006 = ["src/acme/public_api.py:*"]
```

For large existing projects, a baseline freezes current issues so CI only fails on new ones (v0.2):

```bash
uvx yokei --update-baseline
uvx yokei --baseline yokei-baseline.json
```

## Installation

`yokei` is a single Rust binary shipped inside a Python wheel (prebuilt for Linux/macOS/Windows), so all of these work without a Rust toolchain:

```bash
uvx yokei        # run without installing
pipx run yokei
pip install yokei
```

yokei never executes your project's code — analysis is fully static. It also doesn't require your project's virtualenv: if `.venv` exists it is read for metadata, otherwise manifests, lockfiles, and bundled maps are used.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). The full design specification (analysis engine, import resolution strategy, roadmap) is in [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) (Japanese).

## License

[MIT](./LICENSE)
