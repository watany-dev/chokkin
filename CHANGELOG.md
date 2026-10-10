# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- The bundled map gains `griffelib` (`griffe`), `llama-index-workflows`
  (`workflows`), `mkdocs-material` (`material`), `pytest-xprocess`
  (`xprocess`), `pywin32` (`win32api`, `win32com`, `pythoncom`,
  `pywintypes` and the other `win32*` modules), `django-guardian`
  (`guardian`), `jaraco-classes` (`jaraco.classes`) and the
  `gcloud-aio-*` packages (`gcloud.aio.*`) (#679).

### Changed
- CHK009 for a dependency group, extra or build requirement that repeats the
  runtime declaration is info / likely instead of warning / certain, and
  `--fix` no longer removes it: a group may be installed on its own
  (`uv sync --only-group lint`) and generated extras may mirror another
  package's. The same requirement twice in one context stays a warning
  (#696).
- Imports that only run once a package is known to be installed are
  optional (CHK010 / CHK003 info): the body of an `if` whose condition calls
  `importlib.util.find_spec(...)` or an `is_<name>_available()` helper, the
  code after `if not is_<name>_available(): raise ImportError(...)`, and the
  code of a function after it calls either. A `self.` / `cls.` method named
  `is_<name>_available` and a `find_spec` other than `importlib.util`'s do
  not count (#695, #713).
- Every file under the root's or a workspace member's `examples/` is now
  dev context, not only notebooks: its imports still make files reachable
  (CHK001) but no longer raise CHK003 / CHK004 / CHK005, and `--production`
  leaves it out. CHK010 in docs and dev files (`docs/`, `examples/`,
  `scripts/`) is info instead of warning. A nested `pyproject.toml` with
  `[project]` that is not a workspace member, contains none and is not a
  package is an independent project: its files are left out of the
  analysis with a `workspace: skipped N nested projects` warning
  (`--no-auto-workspace` keeps them) (#694).
- CHK005 no longer counts these imports as runtime use: an import inside
  `if "x" in sys.modules:` or `if sniffio.current_async_library() == "x":`
  whose top-level module matches the condition (now info, like other
  optional imports), an import in the `if __name__ == "__main__":` block of
  a runtime file (now dev context; `__main__.py` and the main block of a
  notebook keep their context), and notebooks under the root's or a
  workspace member's `examples/` (now dev context) (#681).
- CHK008 counts more places as declaring a tool: tox `deps`, literal
  `session.install("...")` arguments in `noxfile.py` (parsed, never run),
  workspace members' dependencies, requirements files outside the root names,
  the direct lockfile dependencies of all of these, and wrappers that ship a
  tool (`mkdocs-material` → `mkdocs`, `pre-commit-uv` → `pre-commit`). A
  remote pre-commit hook covers its tool's config section (`[tool.mypy]`) but
  not commands in a Makefile or CI. Binaries of one distribution are reported
  once (#680).

### Fixed
- CHK006 no longer reports the classes of modules a command loader fetches by
  a computed name: when a function binds
  `module = import_module("pkg.commands." + name)` (or
  `f"pkg.commands.{name}"`) and calls `getattr(module, <non-literal>)`, every
  class defined under `pkg.commands` counts as used. Functions, constants, a
  literal `getattr` name and a walk over `getattr(module, "__all__")` are
  judged as before (poetry, #728).
- PEP 723 inline scripts inside a skipped nested project are analyzed
  again: they declare their own dependencies, so `script:` CHK002 / CHK003
  still reports them (#714).
- `__import__(name, globals, locals, fromlist, level)` no longer reads its
  second argument as `import_module`'s `package`. A `level` of 1 or more
  resolves `name` against the calling module's package when `globals` is
  `globals()`; any other `globals` or a non-literal `level` makes the call
  opaque, and `level` without `globals` imports nothing (#566).
- CHK007 no longer reports `__all__` re-exports in an `__init__.py` that a
  wheel ships when the project or member is in app mode (a workspace root
  with two or more members, or a member whose console script targets its
  own package; mcp-python-sdk, fastmcp, llama_index). A file inside a
  member's tree is judged by that member's wheel, else the root's. The mode
  itself and CHK001 / CHK006 are unchanged (#678).
- Imports declared only in requirements files outside the fixed root names
  (`requirements/*.txt` / `*.in`, root `*requirements*.txt`,
  `requirements*.in`) resolve as third-party instead of CHK010. These files
  are used only for import resolution, so CHK001–CHK005 are unchanged, and
  an unreadable one is skipped (#679).
- Poetry dependency tables keep their version constraint (string value or
  table `version`), so a group entry such as `dulwich = ">=1.2.1"` is no
  longer CHK009 against the runtime one. Constraints that parse as PEP 440
  are compared in the same form as PEP 508 specifiers; `^` / `~` are
  compared as written, and `*` and path / git entries count as bare. A
  CHK005 `--fix` move from a Poetry table keeps a valid PEP 440 constraint
  and drops only Poetry-only syntax (#682).
- Test and package detection follows Python and pytest: a singular
  `test/` directly under a workspace member is test context like the
  root's (#571), the `test_` prefix and `_test` suffix are case-sensitive as
  in pytest (#570), and a directory whose name contains `.` (`v1.0/`) is
  no longer a package candidate (#569).
- `.py` files under the root `.github/` directory, which workflows run as
  `python .github/scripts/check.py`, are entry roots like `scripts/`, so
  they are no longer CHK001 (#734).
- hatchling's custom builder and build / metadata hooks
  (`[tool.hatch.build.targets.custom]`, `[tool.hatch.build.hooks.custom]`,
  `[tool.hatch.build.targets.<target>.hooks.custom]` and
  `[tool.hatch.metadata.hooks.custom]`, `path` defaulting to
  `hatch_build.py`) in the root or a workspace member are dev entry roots,
  so they are no longer CHK001 and `--production` drops them. Their imports
  resolve through `[build-system].requires` and need no other declaration
  (#735).

## [0.7.3] - 2026-10-09

v0.7.3 is a fix release on top of v0.7.2 that targets false positives on
real OSS projects. The JSON / baseline `schema_version` stays `"1"`, and
fingerprints are unchanged, so baselines keep matching. Some CHK006 and
CHK010 findings change severity or confidence, and some CHK001 / CHK004 /
CHK006 findings go away.

### Added
- The alembic plugin reads `[alembic] script_location` / `version_locations`
  from `alembic.ini` (root, each workspace member, and package directories)
  and `[tool.alembic]` in `pyproject.toml`, and roots `<script_location>/env.py`
  and the revision files under `versions/` as entries. Revisions in
  subdirectories count only with `recursive_version_locations`. Those entry
  files are left out of CHK006 / CHK007 and only count as references (#667).
- CHK006 skips names that tools read by convention rather than by import:
  protoc output (`*_pb2.py`, `*_pb2_grpc.py`) and a `modular_x.py` next to a
  `modeling_x.py` (transformers codegen) are left out of CHK006 / CHK007, and
  with the alembic plugin, `revision` / `down_revision` / `branch_labels` /
  `depends_on` / `upgrade` / `downgrade` in a revision module count as used.
  None of these affect CHK001 (#655).

### Changed
- `mode = "auto"` no longer treats a console script that targets the
  project's own package (`django-admin`, `httpx`) as an app signal, nor a
  `manage.py` / `asgi.py` / `wsgi.py` / `app.py` inside the package
  (werkzeug `src/werkzeug/wsgi.py`) or in test / docs / dev context. Such
  libraries are now analyzed in library mode, so their public API is no
  longer CHK006 certain. GUI scripts stay an app signal, and a workspace
  member's own console script still marks it as an app (#652).
- CHK010 for an optional import (in a `try:` body or `else:`, or under
  `with suppress(ImportError):`) is info instead of warning, like
  `TYPE_CHECKING` imports. A fallback import in an `except ImportError:`
  clause stays a warning (#654).

### Fixed
- `.py` files under `examples/` and `docs/` at the root and at each
  workspace member root are entries (`auto:examples/**`, `auto:docs/**`), so
  standalone example and docs scripts are no longer CHK001. Files inside a
  package named `examples` are not affected (#666).
- CHK004 uses a workspace member's own lockfile (`libs/core/uv.lock` in
  langchain) for imports from that member, starting the transitive walk from
  the member's declarations. The root lockfile is still checked from the
  root's declarations, and the stronger evidence wins (#653).

## [0.7.2] - 2026-10-08

v0.7.2 is a fix release on top of v0.7.1. The JSON / baseline
`schema_version` stays `"1"`, and fingerprints are unchanged, so baselines
keep matching.

### Fixed
- `[tool.uv.workspace] exclude` is honored: a directory it matches is no
  longer read as a workspace member even when `members` matches it, as in
  uv (#568).

## [0.7.1] - 2026-10-08

v0.7.1 is a fix release on top of v0.7.0. Over the 45-project corpus in
`docs/dev/v0.7.1-release-validation.md` (measured at `4836b55`), the issue
count falls from 12,328 to 10,023, mostly CHK001 and CHK010. The JSON / baseline `schema_version` stays
`"1"`, and CHK010 fingerprints are unchanged, so baselines keep matching.

### Added
- When `[project].dynamic` lists `dependencies` or `optional-dependencies`,
  the arrays of `[tool.hatch.metadata.hooks.uv-dynamic-versioning]` are read
  as runtime dependencies and extras. A `{{ version }}` template in the
  version clears the specifier; one in the name, extras, URL or marker is
  reported as an invalid requirement. `--fix` removes unused entries from
  those arrays. The bundled map gains `py-key-value-aio` (`key_value`) and
  `google-genai` (`google.genai`) (#590).
- pytest's native `[tool.pytest]` table (pytest ≥ 9) is read like
  `[tool.pytest.ini_options]` for `testpaths`, `python_files`, `pythonpath`
  and `addopts`; it wins when it holds keys other than `ini_options`. When no
  `testpaths` entry exists, test files are collected from the rootdir, as
  pytest does. `python_files` also applies to tests inside workspace members
  regardless of the root `testpaths` (airflow
  `providers/*/tests/**/example_*.py`) (#603).
- Each workspace member's `docs/conf.py` is a Sphinx entry like the root one:
  its `extensions` count as module references, it and the shared modules it
  imports stay reachable, the member's `docs/` is docs context, and
  `--production` leaves it out (#612).
- Under the test tree (`tests/`, root `test/`), files below a `data`,
  `fixtures`, `testdata` or `test_data` directory are treated as test input
  (black `tests/data/cases/*.py`) and are no longer CHK001 when unreachable.
  Test data that tests import stays reachable as before, and `--strict` still
  reports it (#593).
- The bundled map gains `google-api-python-client` (`googleapiclient`), the
  `microsoft-kiota-*` packages (`kiota_abstractions`, `kiota_http`,
  `kiota_serialization_json`, `kiota_serialization_text`,
  `kiota_authentication_azure`), `opensearch-py` (`opensearchpy`), and
  `snowflake-connector-python` / `snowflake-snowpark-python` /
  `snowflake-sqlalchemy` (`snowflake.*`) (#591).

### Changed
- The GitHub Action passes `--no-cache` unless the new `cache: "true"`
  input is set, so `.chokkin/` cache files committed in a pull request
  cannot change the result. The `version` input defaults to this release.
- Members of a declared workspace (`[tool.uv.workspace]` /
  `[tool.chokkin.workspaces]`) that name a distribution and have no app
  entry are scored like libraries, as auto-detected members already were:
  their unreachable files are `maybe` CHK001 warnings, including under
  `--production`. Files outside the member's own wheel targets (R-05), such
  as `docs/conf.py`, keep app scoring. airflow's CHK001 count drops from
  1,008 to 389 (1,867 to 509 with `--production`) (#587).
- With `--production`, a workspace member that only dependency groups reference
  (airflow `devel-common`) is dropped like tests. Its files and issues are not
  reported, and `--probe` does not count it. A member stays when the root or a
  shipped member depends on it at runtime, including through
  `include-group`. It also stays when it is nested under a shipped member, or
  when its `classifiers` (given or `dynamic`) mark it as published without
  `Private :: Do Not Upload` (airflow `airflow-ctl`) (#613, #621).
- CHK010 reports an unresolved module once per file instead of once per
  import line. The issue points at the first import that runs (the first
  `TYPE_CHECKING` one when none does), and `explain` lists the other lines
  ("also imported at lines …"). It is a warning when any of those imports
  runs, and info only when all sit under `TYPE_CHECKING`. An inline
  `# chokkin: ignore[CHK010]` silences it only when every one of those lines
  carries the directive. The fingerprint (`CHK010:<file>:<module>`) is
  unchanged, so baselines keep matching (#584, #585).
- An optional (`try:`) or platform-guarded import that CHK004 reports through a
  lock edge is now a `warning` instead of an `error`, also with `--strict`.
  The confidence stays `certain` (#582).
- CHK005 now looks at every runtime import of a distribution, including
  `importlib.import_module` / `__import__` calls, before it sets confidence.
  Previously it used only the first import and was always warning/`certain`.
  A top-level import keeps it warning/`certain`. If the distribution is only
  imported inside functions or lambdas, CHK005 is warning/`likely`. If it is
  only imported optionally or behind a platform guard, CHK005 is
  info/`likely`. `--fix` moves only `certain` CHK005 dependencies to runtime
  (#583, #599).
- CHK005 treats a top-level import in a module that is only loaded from inside
  functions or `TYPE_CHECKING` blocks like a function-local import
  (warning/`likely`). `--fix` therefore no longer moves dev dependencies that
  only lazily loaded modules use (#610).
- CHK005 is no longer `certain` for an import reached only through an optional
  import or from test, docs or dev files. Imports under
  `with suppress(ImportError):` (also `contextlib.suppress`,
  `ModuleNotFoundError`, `Exception`) are now optional, like `try:` imports
  (#614).
- `--fix --dry-run` labels its previews `planned` instead of `applied`, and
  skipped fixes print their reason code (`file-removal-denied: …`) (#594).
- Auto workspace detection walks the tree in parallel, `uv.lock` files in
  uv's own layout are read without a TOML parser, and import resolution
  finds a file's member by directory lookup instead of scanning every
  member; llama_index (608 members) runs in ~1.45s instead of ~1.8s (#592).
- The Rust library is no longer a public API: chokkin ships as a CLI, every
  pipeline module is crate-private, and `cargo-semver-checks` is dropped from
  CI. ADR 0004 now excludes the library from the compatibility surface.

### Fixed
- Imports in a `try` statement's `else:` clause, and imports under a
  module-level `if has_x:` whose flag was set to `True` in the `try` body, are
  treated as optional. Before, they were CHK003 error/`certain`, and
  `--fix --add-missing` added e.g. `cryptography` to requests (#580).
- `--fix --add-missing` skips a `pyproject.toml` that has no `[project]` table
  and reports it as unsupported. Before, it created
  `[project].dependencies`, which broke setuptools builds that declare
  dependencies in `setup.py` / `setup.cfg` (#581).
- A `setup.py` whose `setup()` passes a `name=` that cannot be read statically
  (requests: `name=about["__title__"]`) counts as having a name. The project
  (or workspace member) is now analyzed as a library instead of an app, so
  `--production` no longer reports every file as CHK001 (#586).
- A module-level `pytest_plugins = [...]` in conftests and plugin modules is
  followed, including tuples, string values, annotated assignments and
  `+=`. Plugin modules it names are reachable instead of CHK001. A plugin name
  that does not resolve does not raise CHK010 (#606).
- Tests whose basedir is the root resolve root-level directories that the
  source globs skip (fastapi `docs_src/`, urllib3 `dummyserver/`). Scripts
  outside packages resolve modules next to them
  (`scripts/ci/prek/common_utils.py`). These imports are no longer CHK010
  (#589).
- `_typeshed` imported under `TYPE_CHECKING` resolves as stdlib, so it is no
  longer CHK010 (#584).
- Library analysis with `--production` (library mode or library workspace
  members) still uses the tests, and the public modules only they reach, as
  evidence of API use. Symbols in public modules that only tests import are
  no longer CHK006 (starlette `starlette.responses`) (#588).
- CHK009 no longer reports a group or extra declaration that adds its own
  constraint: a different specifier (`click!=8.3.0` against runtime
  `click>=7`, `pytest>=7` against `pytest>=8`), different extras, or a
  different marker. Specifier order is ignored (`<9,>=7.0` equals
  `>=7.0, <9`). `--fix` removes only a declaration involved in the duplicate
  and keeps the runtime one (#629).
- A declared `python-multipart` counts as used when `starlette` or `fastapi`
  is, since starlette imports it only inside `request.form()`, so it is no
  longer reported as CHK002.

## [0.7.0] - 2026-10-06

v0.7 targets accuracy on real OSS projects (#485). Over the 24 targets
measured in #485 (20 projects, langchain split into 5 libraries), the issue
count falls from 63,606 with v0.6.0 to 9,287. The JSON / baseline
`schema_version` stays `"1"`: the JSON changes below are additive.

### Added
- Monorepos without a workspace declaration now treat nested
  `pyproject.toml` files that declare `[project].name` (depth ≤ 4, honoring
  `exclude`) as workspace members, so dependencies declared in a member's
  manifest satisfy its imports and orphan files in library members are
  `maybe` CHK001 warnings instead of certain errors. `--no-auto-workspace`
  keeps the previous single-project analysis (#488).
- Inside workspace members, declared or auto-detected, `[project.scripts]`
  and entry points become entry roots, the member's own packages (`src/` or
  flat) and PEP 420 namespace packages (`llama_index.core`) resolve to its
  files, and its packages are first-party to every file, so a member
  importing itself no longer raises CHK003 and an application member's
  modules are no longer certain CHK001 (#488, #511).
- In-tree `[tool.uv.sources]` path sources whose `pyproject.toml` has
  `[project]` are read as workspace members, keyed by distribution name,
  including the marker-scoped array form (the first `path` entry wins) and
  `path = "lib/."`. Path sources outside the root and git / url sources are
  ignored. A member's runtime declaration also satisfies CHK005 (#499, #508).
- The package directory comes from build backend config before any guessing:
  setuptools `packages` / `package-dir` / `packages.find.where`, hatch, flit,
  pdm, maturin and poetry `packages`. Otherwise chokkin looks for a
  project-named package under `src/`, the root or `lib/`, then an in-tree uv
  path source named after the project; `examples/`, `benchmarks/` and
  `e2e*/` are never picked. A guess raises the new `GuessedPackageDir`
  warning, and `--probe` shows the result (`root: lib`). sqlalchemy is now
  analyzed from `lib/` (previously `examples/`) and streamlit from `lib/`
  (previously `e2e_playwright/`) (#487).
- `setup.py` is evaluated statically (never executed): variables, `+`
  concatenation, `*` / `**` unpacking, comprehensions, single-`return`
  helpers, if / try branch merges, `deps["x"]` tables, `.append` and `del`. A
  `.txt` / `.in` argument is read as a requirements file from the root or
  `requirements/` (transformers, celery, botocore). When runtime dependencies
  cannot be known (`setup(**kwargs)`, or `dynamic` with nothing found), a
  `RuntimeDependenciesUnknown` warning is raised and CHK003 / CHK004 drop to
  `info` / `maybe`, hidden by the default filter (#491).
- `.ipynb` notebooks at any depth are entry roots (`auto:**/*.ipynb`), so they
  are no longer CHK001 and modules imported only from notebooks are reachable
  (#514).
- In library mode, package `__init__.py` files are entry roots
  (`auto:library package`), and imports from public library orphans count as
  dependency usage, so a library with no other roots no longer reports every
  runtime dependency as CHK002 (#501).
- A new `vendored` setting (default `**/_vendor/**`, `**/vendored/**`,
  `**/externals/**`, `**/third_party/**`) marks vendored code: it is still
  traced and counts as a referencer, but its issues are not reported.
  `vendored = []` turns it off (#490).
- The root `test/` directory (sqlalchemy, CPython style) is scanned by
  default, is a test context, and imports as the local `test.*` package
  (#490).
- The root `conftest.py` is always discovered, and pytest `testpaths` from the
  root config and the package-root config (streamlit `lib/`) are scanned as
  test context, so symbols only tests use are no longer CHK006. Entries that
  overlap the package or come from an explicit `project` glob are skipped
  (#544).
- The JSON report gains a top-level `diagnostics: [{"message": ...}]` array
  holding non-fatal warnings, and `summary.files: {runtime,
  reachable_runtime}` with the number of runtime-context files and how many
  are reachable (#486, #495).

### Changed
- `tests/` directories at any depth (`pandas/tests/`) are test context, and
  test-context files, including those a pytest config roots as tests, are no
  longer reported by CHK006 / CHK007 — they only count as referencers.
  Without `testpaths`, pytest test files are collected from the whole
  project, as pytest does, so tests inside packages and monorepo members are
  roots (pandas CHK006 7946 → 469) (#490).
- CHK009 no longer reports a distribution listed under two optional extras,
  two dependency groups, or a group and an extra: extras and groups are
  installed independently, so a tool needed by both belongs in both. It
  reports a declaration repeated in one context (same marker), or a runtime
  dependency repeated in a group, an extra or the build requirements (#494).
- `pip` / `uv` / `pipx` and other environment managers invoked from `tox.ini`,
  shell scripts and pre-commit hooks are no longer CHK008 binary usages, as
  was already the case for GitHub Actions `run:` steps (#494).
- `--production` never reports dev- or type-only dependencies, even with
  `--strict` (#501).
- A `try:`-wrapped or platform-guarded import of a transitive dependency
  reports CHK004 when a lock edge proves it is transitive, instead of the
  informational CHK003 (#504).
- An internal graph-invariant failure exits 3 instead of 2 (#475).
- `uv.lock` is parsed into only the fields the dependency graph reads
  (`sdist` / `wheels` lines are skipped before TOML parsing), and
  auto-detected members' inputs are collected in parallel; a monorepo with
  ~600 member lockfiles (llama_index) probes in ~2s instead of ~9s (#488,
  #513).
- Breaking changes for the Rust library API (the CLI, config keys, and JSON /
  SARIF output stay compatible):
  - `ChokkinConfig` / `PartialConfig` gain `vendored`, and `SuppressReason`
    gains `Vendored` (#490).
  - `EntryPlan` gains `library_members`, `UnreachableFile` gains `mode`,
    `RuntimeOverrides` / `CliArgs` gain `no_auto_workspace`, `ProbeReport`
    gains `auto_workspace`, `ProbeWarning` gains `AutoWorkspace`,
    `LayoutInfo` gains `members` (`MemberLayout`), `emit_issues` drops its
    mode argument, and `apply_public_surface` takes the `EntryPlan` (#488).
  - `ParsedModule` gains `used_import_bindings` (#489) and `skipped`,
    `ProbeWarning` gains `SkippedSource`, and `RenderContext` gains
    `diagnostics` (#486).
  - `RenderContext` gains `files` (`FileCounts`, from
    `AnalysisReport::runtime_file_counts`) (#495).
  - `LayoutInfo` gains `package_root` (#487).
  - `AnalyzeError::Usage` is removed (#475).
  - `LockfileGraph` gains `extras` (#516).
  - `ManifestSources` gains `runtime_dependencies_unknown` (#491).
  - `ManifestWarning::InvalidRequirementLine.line` becomes `Option<u32>` and
    the variant gains `label` (#503).
  - `SymbolDef` gains `used_in_module` (#540).
  - `plugins::PytestImportSettings` gains `testpaths` (#544).

### Fixed
- A non-UTF-8 Python source no longer aborts the whole analysis with exit 2.
  A file with a PEP 263 `latin-1` / `cp1252` declaration (CPython aliases
  included) is decoded; any other file is skipped with a warning and is never
  reported as CHK001. When a reachable file is skipped, CHK001 / CHK002 drop
  to `likely`, so `--fix` leaves them alone (#486).
- Undecodable `setup.py`, `setup.cfg` and requirements files are skipped with
  a `FileUndecodable` warning instead of aborting the run (#552).
- `importlib.import_module("")`, `"a..b"`, `"pkg."` and a relative name
  without a package no longer crash with "graph invariant violated" (exit 3).
  Relative names resolve against `package=` or `__package__` (#500).
- Library mode no longer reports a library's declared public API as CHK006 /
  CHK007: names in `__all__`, `from ._x import Foo as Foo` re-exports, names
  reached through `from ._x import *` into a public module, and anything in a
  public package's `__init__` (#489).
- CHK007 skips an import its own `__init__` reads, and names a renamed
  re-export (`from .x import a as b`) by the name the package exposes (#489).
- Library mode no longer reports CHK006 for a symbol its own module reads
  (TypeVars, type aliases, classes reached only as an attribute's type,
  module-level helpers and loggers). openai-python CHK006 258 → 20,
  transformers 11700 → 1299 (#540).
- App mode no longer reports CHK006 for a symbol its own module reads unless
  `__all__` lists it: without `export`, such a name is a plain declaration,
  which knip's `exports` does not report either. litellm CHK006 4572 → 737,
  prefect 1774 → 778 (#564).
- A name read only inside a quoted annotation (`x: "list[T]"`,
  `-> "Message[R]"`, `list["Foo"]`) counts as read by its module, so such
  TypeVars and aliases are no longer CHK006. `Literal[...]` strings,
  `Annotated[...]` metadata, `cast("T", x)` and `TypeVar(bound="Foo")` are
  not parsed (#545).
- CHK006 / CHK007 severity for symbols in library workspace members is judged
  in library mode (prefect `src/integrations/*`) (#515).
- Files under a directory that is not a valid identifier
  (`src/integrations/prefect-aws/`) get no module name, so their relative
  imports are no longer CHK010 (#512).
- A declared or locked distribution whose name differs from its import only
  by a `python-` / `py-` / `py` prefix or `-python` / `-py` / `py` suffix
  (`pydocket` for `docket`, `discord-py` for `discord`) satisfies the import
  as a `maybe` match, and the bundled map covers `markdown-it-py`, `pytest`
  (`_pytest`), `odfpy`, `matplotlib` (`mpl_toolkits`), `billiard`,
  `huggingface-hub` and `torch`, so these no longer raise a CHK002 + CHK010
  pair. An exact declaration in a script or member wins over a root affixed
  name (#492, #542, #561).
- Python 3.14's standard library (`compression`, `annotationlib`, ...) and
  `__main__` are recognized as stdlib (#492).
- CHK002 name deduplication checks runtime declarations first, so a
  pyproject dev declaration no longer hides an unused `setup.py`
  `install_requires` entry (sentry-python `certifi` / `urllib3`) (#502).
- Metapackages (a declared wheel target that matches no files, or hatch
  `bypass-selection = true`) keep `project.dependencies` and extras out of
  CHK002; dependency groups are still checked. Hatch wheel `include` is read
  for the public surface (#529).
- An in-tree path source is no longer CHK002 when it is used only through a
  `[project.scripts]` target inside it (#509), or when its module name differs
  from its distribution name (`acme-core` providing `acme/core`) (#553).
- Distributions that a declared extra pulls in according to `uv.lock`
  (`psycopg[pool]` → `psycopg-pool`) count as declared in that context, so
  they are no longer CHK003 / CHK004 (#516).
- The requirements `-r` include context matches whole path words, so
  `protests.txt` is no longer a tests file while `tests/requirements.txt`
  still is (#505).
- PEP 508 / 440 acceptance matches `packaging`: URL schemes are
  case-insensitive and any RFC 3986 scheme is accepted, a URL ends at
  whitespace, `===` takes any string, huge version numbers saturate instead of
  dropping the requirement, a single trailing comma (`pkg>=1,`) is accepted,
  `foo.whl` is a file path only in requirements files, and a malformed
  `name @ url` is invalid. Without a line number, the invalid-requirement
  warning shows a label (`project.dependencies[2]`) (#503).
- CHK008 reads only command words: `tox.ini` `deps` / `description` /
  `allowlist_externals`, words inside Python files under `scripts/` / `bin/`,
  shell comments, command arguments, and remote pre-commit hooks' `entry` no
  longer count a tool as used or unlisted (#494).
- CHK009 is no longer reported when a dependency group or extra re-declares a
  runtime dependency with extras the runtime declaration lacks
  (`streamlit` + `dev = ["streamlit[auth,charts]"]`), including with a
  `[tool.uv.sources]` path source; the group declaration refines the runtime
  one instead of duplicating it. Self-referential extras
  (`all = ["pkg[s3,sqs]"]`) are never duplicates (#507, #494).
- CHK009 no longer reports a requirements line read twice, such as
  `requirements.txt` read directly and through `-r requirements.txt` in
  `requirements-dev.txt`, as a runtime and dev duplicate (#494).
- `--fix` reports applied and skipped fixes in plan order (#443), keeps
  trailing comments attached to the right entries, and preserves CRLF (#561).
- Fixes to behavior that predates v0.6 (#561): `pkg/foo.bar.py`, `.hidden/`
  and `pkg/.py` get no module name (`pkg/foo.bar.py` used to overwrite
  `pkg/foo/bar.py` and cause CHK001 false positives); `./packages/*` member
  globs match; a uv workspace `*` no longer crosses `/`; uv members that share
  a basename are keyed by their full path; ignore directives work in CRLF / CR
  files and a BOM no longer blocks a line-1 file ignore; an annotated
  `__all__: list[str] = [...]` is read.

## [0.6.0] - 2026-10-03

v0.5.1 was prepared but never published to PyPI; its fixes ship here.

### Changed
- Breaking change for the Rust library API (the CLI, config keys, and JSON /
  SARIF output are unchanged): unused or test-only public items are removed,
  including the in-memory parse cache (`ParseCacheStore`, `ParseCacheStats`,
  the `cache` argument of `parse_project_sources_with_cache`), `SymbolReport`,
  `TransitiveIndex`, `ResolvedMode` (folded into `ProjectMode`),
  `ProjectRoot.start`, `PluginsWarning::PluginNoOp`, never-constructed error
  variants, graph edges only tests read, and the `Git` / `Url` / `Index`
  payloads of `UvSourceKind` (#446).
- Invalid `mode` / severity / plugin values in config now report serde's error
  text; accepted values are unchanged (#458).
- `[tool.uv] default-groups` is no longer parsed; `--production` never read it
  (#456).

### Fixed
- Resolver counts only importable local modules and dedupes ambiguity warnings.
- Manifest parsing accepts arbitrary version strings after `===`.
- pytest import settings follow pytest's config file precedence.
- Resolver models pytest `prepend` import mode for test-local imports (#360).
- CHK006 no longer reports symbols referenced only from `tests/` (a v0.5.0
  regression), with or without `tests/__init__.py` (#410).
- CHK006 tracks `from pkg import module; module.name` references, including
  aliases and `from . import module` (#411).
- `--fix` removes a requirements line only when it names the target
  distribution, reading names the way the manifest parser does, and applies
  several removals in one file bottom-up. It no longer deletes a used
  dependency after an earlier removal shifts line numbers; a stale line number
  is reported as an error instead (#438).
- `--fix` keeps CRLF line endings and a missing final newline when removing a
  requirements line, and leaves an empty file when removing the only line (#437).
- `--fix` applies several `pyproject.toml` array removals bottom-up per array
  and removes an entry only when it names the target distribution. It no longer
  deletes a used dependency after an earlier removal shifts array indices; a
  stale index is reported as an error instead (#436).

## [0.5.0] - 2026-09-28

### Added
- Prebuilt wheels for Windows arm64 / i686, manylinux i686 / armv7 / ppc64le /
  s390x, and musllinux i686 / armv7, so these platforms no longer build from
  the sdist (which needs a Rust toolchain).
- Composite GitHub Action (`action.yml`): `uses: watany-dev/chokkin@vX.Y.Z`
  runs chokkin from PyPI via `uv`, with `version`, `working-directory`,
  `baseline`, `reporter`, `sarif-file`, and `args` inputs. The SARIF report is
  written before the gating run so it can be uploaded even when that run fails.
- PEP 735 `{include-group = "..."}` in `[dependency-groups]` is expanded
  transitively (group names normalized). A group pulled into a runtime group
  counts as runtime for CHK002 / CHK005; requirements stay under their declaring
  group, so CHK009 and `--fix` never act on the including group. `--explain`
  shows the include path, and undefined or circular includes become manifest
  warnings.
- `[build-system].requires` and `build-backend` are inventoried as build
  context (R-05, #311). They never feed CHK002/CHK003. An unused project or dev
  dependency that is also a build requirement (e.g. `hatch-vcs`,
  `setuptools-scm`) gets `also in build-system.requires` in its CHK002 evidence.
  `--probe` shows the backend and requires.
- Library mode derives the public surface from wheel target settings (R-05,
  #312). These are hatch `packages` / `only-include`, setuptools `packages` /
  `packages.find` / `package-dir` / `py-modules`, pdm `includes`, flit
  `module`, and maturin `python-source` / `module-name`. Unreachable files
  outside the surface keep app-mode CHK001 confidence, and CHK006 stays a
  warning for symbols outside it. Without such settings nothing changes.
- The manifest cache format moved to v3, so existing manifest cache entries are
  rebuilt once.
- `[tool.uv]` is read beyond workspace members:
  - legacy `dev-dependencies` join the `dev` dependency group, and `--fix` can remove them
  - `constraint-dependencies` / `override-dependencies` are kept as constraints, never declarations
  - `default-groups` is stored, but `--production` does not use it
  - `[tool.uv.sources]` path / editable entries resolve imports from the local tree without a venv
  - `workspace = true` dependencies count as used when imported
- PEP 723 inline script metadata (`# /// script`): each script is an entry root
  with its own dependency scope. Its third-party imports are checked against
  the script block instead of the project manifest, reported as
  `CHK002` / `CHK003` with subject `script:<path>:<distribution>`, and its
  `requires-python` lower bound drives parse and stdlib classification for that
  file. `--probe` lists detected scripts; invalid or duplicate blocks produce a
  warning and the file stays an ordinary source. `--fix` does not rewrite
  script blocks.
- More sources for binary / plugin usage (R-06):
  - PDM scripts (`cmd` / `shell` / `composite`; `call` as module reference),
    Makefile and justfile recipes, Dockerfile / Containerfile `RUN` / `CMD` /
    `ENTRYPOINT`, Procfile, and `.gitlab-ci.yml` scripts.
  - pytest `addopts` (`-p` plugins and options such as `--cov` / `-n` /
    `--benchmark-*`) from pyproject, `pytest.ini`, `tox.ini`, and `setup.cfg`,
    plus `pytest11` entry points in `.venv`.
  - mypy `plugins` (`pydantic.mypy`, `mypy_django_plugin.main`, ...) and
    ty / pyright / basedpyright configs.
  - CHK008 details and `--explain` evidence name the origin as `file:line`.
- `pylock.toml` / `pylock.<name>.toml` (PEP 751), `poetry.lock` (1.x / 2.x), and
  `pdm.lock` are read for the CHK004 transitive check. When several are present,
  one is chosen by priority uv.lock > pylock > poetry.lock > pdm.lock.
  `--probe` shows the lockfile path and kind.
- Fix reminders suggest `pdm lock` when the lockfile is `pdm.lock`.
- Plugins are enabled automatically from declared dependencies (root or any
  workspace member) and config files such as `mkdocs.yml`, `alembic.ini`,
  `tox.ini`, `noxfile.py`, `.pre-commit-config.yaml`, and `docs/conf.py`. An
  explicit `[tool.chokkin.plugins] x = false` still wins. `--probe` shows each
  plugin's reason (`default`, `config`, `enabled-by: ...`, `disabled-by: config`).

### Changed
- Release wheels are built with a pinned Rust toolchain (`RUST_TOOLCHAIN` in
  `release.yml`, currently 1.98.1) instead of the latest stable of the day.
- Fewer string clones in the reachability BFS and the dependency rules'
  reachable-file sets (#136). Findings are unchanged.
- CHK004 now separates a transitive edge from a declared dependency (Certain)
  from a package that is only pinned in the lockfile (Likely, new message).
  Previously the latter was reported as CHK003.
- Library API: `ManifestSources.uv_lock: bool` is replaced by
  `ManifestSources.lockfile: Option<LockfileSource>`.
- Manifest cache unit bumped to `manifest-extract-v3`; every lockfile candidate
  is part of the cache key.
- CHK006 treats FastAPI `@router.websocket` / `@app.websocket` handlers as
  externally used, like route handlers (#119). The parser now records every
  statically named decorator, and the list of framework-registration decorators
  lives only in the CHK006 rule. Parse cache unit bumped to `parse-v7`.
- The FastAPI plugin no longer adds root `main.py` / `asgi.py` as entries,
  since the §8 auto-detection already does; it keeps `src/main.py` /
  `src/asgi.py`.
- The Python parser is now `ruff_python_parser` (exact pin `=0.0.15`) instead
  of `rustpython-parser` (ADR 0001, #351). MSRV rises from 1.93 to 1.96. A
  symbol on a decorated `def` / `class` is reported on the `def` / `class`
  line, and `elif` branches get the same `TYPE_CHECKING` / platform guard
  handling as `if`. Syntax errors no longer carry the text-matched
  `(requires pyXY)` hint. Parse cache unit bumped to `parse-v8`.
- PEP 695 syntax is walked: `type X = ...` records `X` as a module-level
  symbol, and attribute references in the alias value and in type parameter
  bounds / defaults (`def f[T: m.A = m.B]`, `class C[T: m.A]`) now count as
  uses (#352). Parse cache unit bumped to `parse-v9`.
- Performance: files are parsed across worker threads, warm-run parse cache
  keys come from file stat `(size, mtime)` instead of hashing contents, and
  parse results are stored in one bundle per cache context.
- The module-index scan cache was removed; it could never beat a rebuild.
- JSON and SARIF reporters are rendered with `serde_json`. Field order and
  values are unchanged, but whitespace of the pretty-printed output may differ.
- Parse and manifest cache unit versions are now `parse-v10` and
  `manifest-extract-v3`, so existing `.chokkin/cache` entries from v0.4.0 are
  rebuilt on the first run after upgrading.

### Fixed
- The bundled package map no longer maps distributions to import names they
  do not ship (`pynacl` -> `nacl`, `pyzmq` -> `zmq`, `dnspython` -> `dns`,
  `attrs` -> `attr`, `setuptools` -> `pkg_resources`, and 12 more), and adds
  `azure-*` / `opentelemetry-exporter-*` namespace entries plus `vertexai`,
  `a2a`, `vcr`, `ddtrace`, `onelogin` and `ulid`, so these no longer raise
  CHK010 / CHK002 without a venv. `generate-package-map.py --verify-wheels`
  checks every entry against the latest PyPI wheel (#362).
- Reachability (#266): `import pkg.sub.mod` (static, dynamic literal or
  plugin module reference) now also reaches the parent packages'
  `__init__.py` files. Framework-glob files such as Django migrations are
  followed by the import walk, so their imports reach first-party files and
  count as used third-party imports. CHK001 confidence drops to `likely` when
  reachable code has an opaque dynamic import; an unreachable file's own
  opaque import no longer affects its confidence.
- Star imports (`from m import *`, `from . import *`) are no longer dropped by
  the parser. They reach `m` for CHK001 and count as imports for the
  dependency rules, but are not CHK007 re-exports. The parse cache moved to
  `parse-v8` (found by dogfooding on litellm).
- An import root that is missing from the bundled map but matches a declared
  or locked distribution by exact name (e.g. `openai`, `tokenizers`) now
  resolves to that distribution. Before, it stayed unresolved, which caused
  false CHK002 / CHK010 reports.
- `[sys.executable, "-m", "pkg", ...]` argument lists count as a use of `pkg`
  but are never reported as missing. A PEP 723 script that runs another file
  with `sys.executable` gets no script CHK002, since the child shares the
  block's environment. Parse cache unit bumped to `parse-v10`.
- `importlib.import_module("pkg.commands." + name)` (or an f-string with a
  literal prefix) reaches every first-party module under `pkg.commands`, so
  lazily loaded command modules and their imports count as used. In a PEP 723
  script that imports `subprocess`, a declared dependency named by the first
  word of a command-line string literal (`"ruff format ..."`) counts as used.
- A `workspace = true` dependency counts as used when a used workspace
  member's own files import a module from its tree (a root that ships
  `airflow-core` and `task-sdk` uses both, since core imports `airflow.sdk`).
- The bundled stdlib lists are generated from each version's
  `sys.stdlib_module_names`, adding 124 missing modules such as `unicodedata`,
  `msvcrt`, `winreg`, `_thread`, `_ssl` and `sre_parse` that caused CHK010 /
  CHK003 / CHK004 false positives. `test` (CPython's regression suite, not
  in that list) is no longer treated as stdlib (#357).
- A root `tests/`, `scripts/` or `docs/` directory with `__init__.py` is now a
  first-party package, so `from tests.helpers import x` no longer raises
  CHK010 and helpers reached only this way are not CHK001. These packages keep
  their test/dev/docs context, never count as the flat-layout distribution
  package, and their symbols are not checked by CHK006 / CHK007 (#359).
- JSON reporter: CHK003 / CHK004 issues now fill `distribution` with the
  resolved distribution name (it was always `null`), and CHK003 / CHK004 /
  CHK010 put only the imported module in `symbol` instead of
  `"<path>:<line> <module>"`; the location stays in `file` / `line`.
  `schema_version` stays `"1"`. `target`, fingerprints, baselines and SARIF are
  unchanged. `--fix --add-missing` reads the distribution from the issue
  instead of the explain text. Library API: `IssueSubject::Import` gains
  `distribution: Option<String>` (#363).
- A module is stdlib when it is stdlib on any Python minor from
  `target_version` up to the `requires-python` upper bound (the newest bundled
  set when unbounded). `tomllib` behind a `sys.version_info` guard in a
  `>=3.10` project is no longer a CHK010. An unguarded `import tomllib` there
  is no longer reported either (#358).
- An import root with `_` or capitals that no map names (e.g. a local
  `e2e_config`) no longer turns into a guessed third-party distribution
  (`e2e-config`) that hid the CHK010. It resolves only when a declared or
  locked distribution (including the importing file's PEP 723 block or
  workspace member manifest) has the same normalized name (`import foo_bar`
  with `Foo_Bar` declared); otherwise it is CHK010 (#361).
- `try` / `except*` blocks and every expression slot in statements are now
  walked, so imports, attribute accesses, and dynamic imports inside them are
  collected.
- Dynamic imports through aliases (e.g. `from importlib import import_module as im`)
  and keyword arguments (`name=`) are recognized, and static imports on the same
  line are no longer tagged as dynamic.
- Submodules imported via absolute `from pkg import name` are reachable.
- Re-export `source_module` no longer applies relative resolution twice, and
  `from . import x` re-exports are collected.
- CHK003/004/005 are no longer double-reported or missed in `--strict` mode with
  workspace members.
- CHK008 config ignores also match distribution names.
- Flask route / Celery task decorators are detected in files with syntax errors
  (regression of #167), and the text-scan and parse paths agree on the same set.
- PEP 508 bare requirement names that look like archives (e.g. `A.tlz`) in
  `pyproject.toml` / `setup.cfg` / `setup.py` are no longer dropped.
- Cache correctness:
  - config-scan cache hits are validated against every file the scan reads
    (`tox.ini`, `.pre-commit-config.yaml`, `mkdocs.yml`, `scripts/`, `bin/`, ...);
  - manifest cache inputs track absent `-r` / `-c` include candidates;
  - manifest cache hits are rebased onto the current project root after a
    project is moved or copied;
  - parse cache racy-mtime checks use the filesystem clock, guard against key
    collisions, and report the first failing file in discovery order.

## [0.4.0] - 2026-08-17

### Added
- Offline, deterministic wheel-metadata harvesting for package-map candidates;
  harvested data is review-only and never overwrites bundled seeds.
- Accepted safe-autofix and semver contracts in ADR 0003 and ADR 0004.
- Regression fixtures for dev/type, optional, and platform-guarded missing imports.

### Changed
- Default CHK003 reporting now focuses on runtime imports; type, test, docs, and
  dev imports remain available under `--strict`.
- Optional and platform-guarded undeclared imports are retained as conditional,
  informational CHK003 candidates instead of hard errors.
- `TYPE_CHECKING` detection now follows aliases such as `import typing as t` and
  invalidates earlier parse-cache entries.
- The fixed 20-project corpus reports 131 CHK003 findings, down from the v0.4
  Step 0 baseline of 964, with 0 unknown labels and all §17 gates passing.

## [0.3.0] - 2026-07-01

### Added
- `schema_version` on JSON reporter (`"1"`) and baseline files, with v0.2 baseline
  reader compatibility when the field is omitted.
- Published JSON Schema files under `docs/schema/` for report and baseline formats.
- `[tool.chokkin.severity]` per-rule overrides (`off` / `info` / `warning` / `error`)
  wired through issue emission, reporters, and exit codes.
- SARIF rule metadata stabilization: `helpUri`, `fullDescription`, and shared CHK
  rule metadata.
- Ignore directive syntax regression tests (`# chokkin: ignore[...]`,
  `# chokkin: file-ignore[...]`).
- Plugin API RFC at `docs/adr/0002-plugin-api-rfc.md` (documentation only; no
  external plugin loading).

### Changed
- Version bumped to 0.3.0 as the contract stabilization release (Phase 3).

## [0.2.0] - 2026-06-16

### Added
- Baseline filtering with checked-in dogfood baseline support for CI adoption.
- GitHub Actions and SARIF reporters for inline CI annotations and code scanning.
- uv/chokkin workspace member resolution, member-owned import tagging, and strict
  member-local dependency declaration checks.
- Conservative cache plumbing, including parse-cache key primitives, disk-backed
  parsed module entries, and typed scan payload storage for config, manifest, and
  module-index scans.
- Expanded static config/plugin detection for pytest, Django, FastAPI, Flask,
  Celery, tox, nox, pre-commit, GitHub Actions, Sphinx, MkDocs, and Alembic.
- Notebook code-cell parsing for `.ipynb` sources.
- Draft JSON/baseline schema migration notes for the future stable schema work.

### Changed
- Default CLI behavior now runs the full analysis pipeline with default,
  compact, json, markdown, github, and sarif reporters plus `--explain`,
  `--trace`, `--fix`, and baseline filtering.
- v0.2 release validation was recorded with Rust 1.93: `make check`,
  in-repo OSS fixtures, the 20-project OSS gate, baseline dogfood CI, and
  Criterion cache benchmarks passed.

### Notes
- JSON reporter and baseline schema remain draft in v0.2. Stable schema
  guarantees are deferred to Phase 3.

## [0.1.0] - 2026-06-14

### Changed
- **BREAKING:** Renamed the project from `yokei` to `chokkin` — CLI binary, PyPI
  package, `[tool.chokkin]` config table, `chokkin.toml` / `.chokkin.toml` config
  files, `# chokkin: ignore[…]` directives, and rule codes `CHK001`–`CHK010`.

### Added
- Minimal Rust bin+lib crate scaffold (`--version`, `--help`).
- `pyproject.toml` with maturin `bin` bindings for Python wheel distribution.
- User-facing README (English and Japanese) covering the designed UX.
- Full design specification in `docs/dev/spec.ja.md` (§1–§21).
- Hardened CI/CD pipeline ported from `watany-dev/ptuf`:
  - `ci.yml`: fmt / clippy / nextest (ubuntu + macOS + Windows) / MSRV /
    coverage / cargo-deny / cargo-machete / semver-checks / actionlint / zizmor.
  - `audit.yml`: daily `cargo-audit`.
  - `release.yml`: maturin wheel build matrix + PyPI Trusted Publishing.
- Static analysis configs: `clippy.toml`, `rustfmt.toml`, `deny.toml`,
  `.cargo/config.toml`.
- `Makefile` with `make check` pre-commit gate.
- Agent and guardrail configs: `AGENTS.md`, `CLAUDE.md`, `.claude/settings.json`,
  `.cursor/` (ptuf hooks), `scripts/bootstrap-agent.sh`.
