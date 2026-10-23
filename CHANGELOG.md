# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- The module a loader API names in its first string argument is a dynamic
  import: `import_string`, `symbol_by_name`, `import_from_string`,
  `load_object`, and cyclopts `command("pkg.mod:app")`, plus the functions
  listed in the new `module_loaders` setting. The trailing
  attribute counts as a use of that module's symbol (no CHK006), and an
  unresolved name is not CHK010, since tests pass bad names on purpose.
  Strings like `"pkg.mod:Cls"` are also read as module references (#493).
- The Airflow plugin reads Airflow 3's `[dag_processor]
  dag_bundle_config_list` from `airflow.cfg`: the `path` of each
  `LocalDagBundle` is a DAG folder like `dags_folder`, under the same
  safe-mode check, so DAGs a bundle loads are no longer CHK001. A value that
  is not a JSON list, a malformed item, a bundle of another class
  (`GitDagBundle`) and an absolute or templated `path` are gaps that cap
  CHK001 at `likely` and show in `--explain` (#1058).
- The Sphinx plugin reads a discovered `conf.py` beside an `index.rst` or
  `index.md` root document as a Sphinx configuration, wherever it is
  (`--trace` origin `conf.py beside index.rst`), not only `docs/conf.py`.
  Its `extensions` resolve and its extension files are reached, as
  mlflow's `docs/api_reference/source/conf.py` and airflow's
  `providers-summary-docs/conf.py` need. Its top-level names are settings
  Sphinx reads, not CHK006, even inside a package, and it is docs context
  wherever it is, so the extensions it loads are no CHK005 against a docs
  group. A `conf.py` with no root document beside it is not read (#814).

### Fixed
- A notebook cell with a help escape after unbalanced brackets
  (`y = (1,` then `b]` then `?obj.attr`) no longer aborts the run with a
  panic in the parser: the cell is lexed, not parsed, to find where its
  statements start (#1104).
- A PEP 723 `dependencies` entry is reported on the line that lists it. A
  short name such as `s` took the `dependencies = [` line, and a repeated
  entry took the line of the first one (#1104).
- A `uv.lock` that locks one name more than once (one entry per version of a
  fork resolution) keeps what every entry depends on. Only the last entry's
  dependencies were kept, so those of an earlier version looked unlocked
  (#1104).
- An `[options.entry_points]` group name in `setup.cfg` keeps its case, as
  setuptools reads it: `MyPlugins` was read as `myplugins` (#1104).
- A workspace member only a pytest `-p` option or the `pytest-*` naming
  convention uses is no longer a certain CHK002, and the members its tree
  imports are used through it: the member fixed point now reads the plugin
  references of config files, as third-party use does, and runs after the
  config and pytest evidence (#1109).
- A notebook line that starts with `%`, `!` or `?` inside brackets or a
  multi-line string (`x = (10` then `% 3)`) is read as Python, as `IPython`
  reads it, not as a magic or shell command. The notebook was skipped as a
  syntax error and the packages it imports were CHK002 (#994).
- A cold run of a large workspace is fast again: since 0.7.8 every string
  naming a module was looked up in each workspace member over every project
  file, which made airflow's run 1.5 s -> 4.5 s. Only the files whose path
  ends in the name are looked at now; output is unchanged (airflow 1.8 s).
- A cold run does less work per file again: the nesting check added for
  over-deep trees skips nodes too short to reach the limit, the module-level
  platform-guard scan runs only when a platform module is imported under
  another name, and loader-name lookups and module-reference strings are
  rejected sooner. Output is unchanged; sqlalchemy runs about 3% fewer
  instructions (#1087).
- `--fix` no longer moves a Poetry group dependency whose table says where
  it comes from (`git`, `path`, `url`, `source`, or any key besides
  `version`, `extras`, `markers`, `platform`, `python` and `optional`;
  `extras` carries over only from a single table, not an array of forms) into
  `[project].dependencies` as a bare name, which would resolve it from the
  default index. The CHK005 move is skipped as `ambiguous` (#1041).
- `--fix` no longer creates a `[project]` table holding only `dependencies`
  when it moves a CHK005 dependency out of a group in a pyproject.toml with
  no `[project]` (Poetry 1.x), or into a dynamic `project.dependencies`. The
  move is skipped as `unsupported-target`, as `--add-missing` already did.
- `--fix` skips a CHK005 move out of a dev requirements file as
  `unsupported-target` instead of failing to parse that file as TOML.
- With the Airflow plugin on, every `.py` under a directory named exactly
  `dags` is a DAG-folder entry in its path's context, not only files that
  pass the DAG processor's safe-mode check (`airflow` and `dag` in the
  text). Test suites read such folders by path, so airflow's
  `tests/unit/dags/no_dags.py` and the `.airflowignore` case
  `subdir2/subdir3/should_ignore_this.py` are no longer CHK001. Folders with
  a partial name (`dags_*`, `*_dags`, `example_dags`) keep the check (#1064).
- A formatter module that `[tool.datamodel-codegen] custom-formatters` (or
  `custom_formatters`) names in the root or a workspace member's
  `pyproject.toml` is no longer CHK001. A name that resolves through the
  import roots, or else to the only module of that name under that
  pyproject's directory, is a dev entry root with origin
  `command: tool.datamodel-codegen.custom-formatters (<file>:<line>)`, so
  `--production` drops it like other dev commands. A name matching no
  project file or several, or a value that is not a list of module names,
  is a gap that lowers CHK001 to likely, with a warning and an `--explain`
  line (#1063).
- An unused file that the wheel of a workspace member ships is reported as
  `maybe` instead of `certain` when a metapackage root distributes that member
  (a root whose wheel targets ship no file, such as hatch `bypass-selection`,
  and that has no app signal of its own), as in fastmcp's `fastmcp-slim`. The
  member's console script into its own package is then a CLI shipped with the
  library. The member is still judged as an app, so files outside its wheel
  and unused public symbols (CHK006 / CHK007) are reported as before; a
  member without declared wheel targets ships its layout's packages. Names
  match after PEP 503 normalization; dependency groups and roots that ship
  code do not count, and airflow's root distributes nothing because of its own
  console script. `--explain` and `--trace` name the requirement behind the
  lower confidence (#1060).
- A Sphinx `conf.py` that star-imports a first-party module
  (`from provider_conf import *`) also reads that module's top-level
  `extensions` assignment, `append` / `extend` / `insert` and `+=`, and
  resolves its names through that module's own imports and `sys.path`
  hints, one level deep. Airflow's `sphinx_exts` extensions are no longer
  CHK001. An `extensions` value in a form not read (`EXTS + [...]`, a
  comprehension, a tuple) and a star import not followed are a gap
  (`UnreadSphinxExtensions`) with a plugin warning, which lowers CHK001 and
  distribution CHK002 to likely (#1062).

## [0.7.8] - 2026-10-20

### Added
- A file with a module-level `if __name__ == "__main__":` block is an entry
  root (`--trace` origin `auto: main-block`), at any depth and whatever its
  name, since it is written to be run as a script. Its imports count as used
  and it is no longer CHK001 (#1026).
- Scripts (`python X.py`) and `-m` modules that task files and tool configs
  run (Makefile, justfile, tox, local pre-commit hooks, PDM / poe / hatch
  scripts, GitLab CI, `scripts/*.sh`, Dockerfile, Procfile) are entry roots
  with origin `command: <label> (<file>:<line>)`. Task-file targets are dev
  entries; Dockerfile and Procfile targets keep the file's own context and
  count as production roots. A `-m` relocated by `cd`, `--directory` or
  `PYTHONPATH=` stays a gap (#912).
- A `module:attr` app that a WSGI/ASGI server loads (gunicorn, uvicorn,
  hypercorn, daphne, waitress-serve, granian) from any command source, and a
  literal app string passed to `uvicorn.run` / `uvicorn.Config`, is an entry
  root when the module resolves to a discovered file, with the attribute as
  its used symbol. Task files also read `[tool.poe.tasks]` and hatch
  environment scripts. Options that move the import root (`--app-dir`,
  `--chdir`, `PYTHONPATH=`, a poe task's `cwd`, ...) are gaps (#1004).
- Plugins root the files Airflow, Dagster, Prefect and Scrapy load by path or
  module name: Airflow DAG folders, Dagster code locations (`[tool.dagster]`,
  dg projects, `workspace.yaml`) and modules building `Definitions`, Prefect
  deployment entrypoints, and Scrapy settings, `SPIDER_MODULES` and named
  components. A value that is not a literal is a gap (#401).

### Changed
- CHK001 is a warning in every mode, as the rule table in the README and
  spec §3 say, instead of an error outside library-mode `maybe`. An unused
  file is judged by the import graph, which misses files a framework, tool or
  test loads by name or path, so it no longer fails a default run on its own;
  `--strict` still counts it. Library mode keeps lowering its confidence.
- A string literal anywhere in the project that is a whole dotted module name
  (`"pkg.mod"`, `"pkg.mod.attr"`, `"python:pkg.mod"`) and names a project
  file no entry reaches lowers that file's CHK001 to an info note at
  `likely`, with the literal's site in `--explain`; one `parse:` warning per
  run names the first such literal. Airflow's `lazy_load_command` targets and
  gunicorn's `python:` config no longer report as unused files (#1026).

### Fixed
- Files a plugin names as entries by path (Django `manage.py` and settings,
  Sphinx `docs/conf.py`, Alembic `alembic/env.py`) join the inventory even
  when the layout's globs or an explicit `[tool.chokkin] project` leave them
  out, so their imports count and the skipped-entry warning no longer lowers
  unrelated findings. Only the named file joins, not its directory (#932).
- Every layout reads root `examples/**`, `docs/**` and `**/*.ipynb`. A
  notebook outside the shipped package is dev context, so its imports no
  longer make dev-group dependencies CHK002; a notebook inside the package
  stays runtime. Workspace members under `examples/` keep their own layout
  (#1023).
- PEP 723 script CHK002 marks a runtime peer used when its provider is used,
  sharing the project-level table, which now also covers transformers,
  evaluate, datasets, torchmetrics and inspect-ai backends. An argv list
  passed to `subprocess` (`["transformers", "serve", model]`) counts as a
  command use.
- Flask `--app` / `FLASK_APP` values that are file paths (`hello.py`,
  `./app.py:create_app()`, `pkg/app`) are read as `flask.cli` reads them, and
  a module name for a root file the layout names no module for resolves to
  that file, so the app and its helpers are no longer CHK001 (#866).
- `--probe`, `--trace`, `--explain`, fatal errors, the baseline summary and
  `--init` escape control characters, bidi embedding / override / isolate
  characters (U+202A–U+202E, U+2066–U+2069) and U+2028 / U+2029 in project
  text, as the reporters do. Fatal errors keep their line breaks; JSON and
  SARIF output are unchanged (#996).
- Django's usual dotted `WSGI_APPLICATION` and `ASGI_APPLICATION` targets
  are recognized alongside the previously supported colon notation.
  Unread targets report a settings gap; unset handlers remain optional.
- Django settings strings now keep authentication, storage, logging, and other
  configured modules reachable in production analysis. Custom settings also
  count when their dotted strings name a project module; computed values in
  known import settings report a gap instead of implying complete coverage
  (#1003).
- Root and workspace-member `setup.py` files are dev entry roots, including
  in src and flat layouts, so build scripts and the helpers they import are
  no longer reported as unused files. Their imports use the build environment's
  requirements (or the legacy setuptools default), and `--production` leaves
  the scripts out (#1031).
- The bundled binary map names `zensical`, so a declared `zensical` that a
  task runner calls (`[tool.pdm.scripts] doc = "zensical serve"`) is used
  instead of reported as CHK002.
- A dependency whose `python_version` / `python_full_version` marker holds
  on every interpreter `requires-python` allows (intersected with Poetry's
  `python` dependency, or a script block's own `requires-python`), such as
  `foo; python_version >= "3.8"` under `>=3.9`, is unconditional for CHK002
  and reported when unused; a marker with other variables or forms outside
  the model stays conditional (#919).
- In `setup.py`, `reqs[0] = "b"` and `del reqs[0]` with an integer literal
  index in range replace or remove that element, so the old requirement is
  no longer declared and reported as CHK002; other indexes, and lists whose
  order a method call, a loop or a branch merge may have changed, still
  leave the list partial (#868) and now also declare the assigned value,
  so `reqs[i] = "c"` no longer leaves `import c` unresolved.
- A URL requirement whose `#egg=` is not a project name
  (`#egg=acme;python_version<"3"`, `#egg=name[extra]`, a quoted
  `-e "...#egg=with space"`) is an invalid requirement, as pip 25 reads it,
  instead of a dependency named `acme;python-version<"3"` reported as
  CHK002; the egg's leading name lowers its CHK003 to a hint (#929).
- A declared backport of a removed stdlib module (`legacy-cgi` for `cgi` /
  `cgitb`, `standard-imghdr`, `standard-telnetlib` and the other PEP 594
  `standard-*` packages, `audioop-lts`) counts as used by the module's
  import on every target, instead of CHK002; on 3.13+ an undeclared one is
  a CHK003 naming the backport rather than a CHK010 (#939).
- `requires-python = ">=3.8"` (or `>=3.9`) makes the modules 3.10 removed,
  such as `formatter`, `parser`, `symbol` and `_bootlocale` (and
  `dummy_threading` on 3.8), stdlib instead of CHK010 / CHK003: the stdlib
  table now covers 3.8 and 3.9 (#940).
- A bare `except:` and `except Exception:` / `except BaseException:` catch a
  failed import like `except ImportError:` does: the import they fall back
  from keeps its `fallback_for` link (so a dead fallback such as
  `dummy_threading` drops to info), and `x = None` in them sets an
  availability flag when the `try` body imports something; a broad handler
  around `token = os.environ[...]` sets none (#967).
- An import inside a function (or `lambda`) defined in a `try:` body or
  `with suppress(ImportError):` is no longer optional: the body runs when
  called, after the `try` stops catching, so a missing package there is
  reported at full severity again. A function defined under `else:`, a flag,
  or an availability check keeps its imports optional, since it exists only
  when the package is installed (#968).
- `if not HAS_X: raise ImportError(...)` with a module-level try flag makes
  the statements after it optional, like `if not is_x_available():` does.
  The test is read like an `if` body's, through `not` / `and` / `or`; one
  that can be true with the package installed still guards nothing (#958).
- A flag set to `None` in an `except ImportError:` handler reads
  `x is None` as true without the package, so the `else:` of `if x is None:`
  and the statements after `if x is None: raise ImportError(...)` are
  optional. A flag set to `True` in the `try` body is `False` without the
  package, not `None`, so `has_x is not None` no longer makes its body
  optional.
- `sys.platform.startswith(...)`, `os.name == ...`, `platform.system() == ...`
  and `in` / `not in` against them are platform guards like
  `sys.platform == ...`, so an import under `if os.name == "nt":` is
  platform-guarded. The receiver must be the stdlib module in that scope: a
  parameter or other binding named `sys` makes the test an unknown leaf
  (#957).

## [0.7.7] - 2026-10-17

v0.7.7 closes the v0.7.6 `uvx` findings and the remaining gaps of the
unknown-input contract (ADR 0005). The JSON / baseline `schema_version`
stays `"1"`, and fingerprints are unchanged, so baselines keep matching.
Django projects, pytest `testpaths`, lockfiles, notebooks with IPython
syntax, `__all__` built in blocks and loops, and requirements lines that pip
accepts are read more fully, and the cold run is about 28% faster.

### Changed
- The cold run is about 28% faster on the validation corpus: module
  resolution, symbol analysis and the dependency and symbol rules run in
  parallel, attribute accesses not bound by an import are dropped at parse
  time, and PEP 723 blocks are looked for only in files with a `# /// script`
  marker. Output is unchanged (#944–#948, #761).

### Fixed
- `__all__ = __all__ + [...]`, `__all__ = [...] + __all__` and
  `__all__.insert(i, "name")` add their literal names to `__all__` like `+=`
  and `.append` do, instead of leaving the list partial with an
  `unsupported __all__ assignment form` warning (#876).
- A literal `__all__` assignment inside `if` / `try` / `with` is read: each
  path through the block is followed and the paths join as the union of their
  names, so `try: __all__ = ["Client", "Auth"] except ImportError: __all__ =
  ["Client"]` exports both instead of leaving the list partial or unknown. A
  block that assigns `__all__` on some paths only, with no list before it,
  leaves the list partial with a warning that `__all__` may be unset (#906).
- Literal additions to `__all__` inside `for` / `while` bodies and their
  `else`, and literal assignments and additions in `match` cases, are read,
  so `match sys.platform: case "win32": __all__ += ["win_only"]` exports
  `win_only` instead of leaving the list partial (#875).
- `Migration` in a module of a Django app's `migrations` package is no longer
  CHK006: Django's migration loader reads it by name (#926).
- Relative imports in packages the layout names no module for (Django's
  `tests/admin_views/`, a `scripts/tool/` package) resolve against their
  `__init__.py` chain instead of warning about a missing package context, and
  are always first-party, so `from .api import get` in `scripts/requests/`
  no longer counts as using `requests`. Reachability follows them into the
  script's or test's own package (#896).
- Notebooks with IPython syntax are read instead of skipped. Shell escapes
  (`!ls`, `x = !ls`), help (`obj?`) and magics that neither run Python nor
  import a module (`%pip`, `%matplotlib`, `%%bash`) are dropped, magics that
  run Python keep it (`%time`, `%%time`, `%%capture`), and `%load_ext` /
  `%aimport` count as imports. A cell with any other magic (`%run`,
  `%%writefile`) or a syntax error is dropped on its own with a
  `code cell not read` warning at its line, and the rest of the notebook is
  still read (#962).
- `-r`, `-c` and `-e` lines in requirements files are read with pip's option
  parser, so other options on the line (`-r base.txt --pre`,
  `--requirement=more.txt --no-index`, `-e <url> --config-settings=k=v`) no
  longer become part of the file name or the editable spec, quoted values
  (`-r "my reqs.txt"`) are unquoted, and a flag line pip rejects (`-r` with no
  value, an unknown option) is reported as an invalid requirement line instead
  of stopping the file (#964).
- A requirements line pip installs from a directory or an archive without
  `-e` (`../../../llama-index-core`, `./pkg`, `pkg/sub`, `wheels/foo.whl`,
  `foo-1.0-py3-none-any.whl`) is recorded as an opaque dependency, as
  `-e ./path` is, instead of being reported as an invalid requirement line;
  PEP 508 manifest fields and constraints files still reject it (#963).
- A string `install_requires` / `extras_require` in `setup.py`, the string
  items of its lists, and `setup.cfg` requirement values are read the way
  setuptools' `parse_strings` reads them: ` #` starts a comment
  (`rich  # pretty output` declares `rich`) and a line ending in `\` joins
  the next one (`requests \` + `>=2` declares `requests>=2`), instead of
  being reported as invalid requirement lines (#965).
- A root `[tool.uv.sources]` path source inside the root that is not read as
  a member now says why: a missing directory warns that it does not exist,
  and a directory with no `pyproject.toml` `[project]` table (a `setup.py`
  sub-project) warns that its code is analyzed as the root's without its own
  entries or dependencies. Findings under such a directory are lowered:
  unused files and unused declarations of what its files import to `likely`,
  undeclared and misplaced imports from it to hints (#1000).
- Django projects whose settings live outside `DJANGO_SETTINGS_MODULE`'s
  usual setters are read: `runtests.py`, `conftest.py` and pytest-django's
  `DJANGO_SETTINGS_MODULE` ini option set the module, a template placeholder
  (`{{ project_name }}.settings`) no longer hides later setters, `examples/`
  directories are skipped, and a fallback candidate must assign
  `INSTALLED_APPS` / `ROOT_URLCONF` (also in a django-configurations class),
  star-import the module that does, or call django-split-settings'
  `include(...)`. A reusable app's `migrations/**` is used whenever Django is
  declared, with or without settings (#979).
- Imports of common Django packages (`rest_framework`, `django_filters`,
  `taggit`, `mptt`, `ninja`, `phonenumbers`, `graphql` and more) map to their
  distributions; when an import can come from two distributions
  (`phonenumbers` / `phonenumberslite`, `ninja` / `django-ninja`), the
  declared one is used (#971).
- A lockfile that lists no packages (a stub `uv.lock` beside `poetry.lock`)
  is skipped with a warning for the next candidate, and `--probe` lists the
  candidates not used. A lockfile over 32 MiB or 20,000 packages is skipped
  with a warning instead of slowing the run, and each lock closure is walked
  once per declaration scope instead of once per import (#972, #976).
- Names with control characters are escaped in the `default`, `compact`,
  `markdown` and `github` output, and a dist-info whose `Name:` is not a
  PEP 508 name credits nothing and warns (#973).
- `[tool.poetry]` `name` / `version` are read when `[project]` has none,
  unless `package-mode = false` (#974).
- An extra that could not be read caps CHK003 and CHK004 at `likely` (#975).
- A wheel-declared directory holding `.py` files directly is read as a PEP 420
  package, and `from sibling import X` in a script counts as a reference to
  the sibling module for CHK006. `--probe` lists undeclared module directories
  without `__init__.py` on an `Unpackaged:` line (#977).
- pytest `testpaths` entries widen to the top of their `__init__.py` chain
  (celery's `t/unit` makes `t/integration` test code too), and root-level
  suites become local packages, so `t.*` imports resolve (#978).
- `--fix --dry-run` applies the same root containment check as the real
  write, and `--init` through a symlink that leaves the root is an error
  (exit 2) (#980).
- chokkin config paths that leave the project root are validation errors;
  manifest paths that do (`-c`, root `[tool.uv.sources]` paths) warn that they
  were not read, and `-r` outside the root says so instead of "not found"
  (#981).
- The warning for a relative import that cannot be resolved names which case
  applies: no module name in the layout, a top-level module with no parent
  package, or a `level` past the top-level package (#982).
- CHK002 names the distribution and points at its line in `pyproject.toml`
  (#983).
- CHK002 from a PEP 723 script block is `likely`: the block lists the
  environment `uv run` builds, including runtime peers the script never
  imports (#984).
- A symlink that leaves the root, loops or leads nowhere is named once in a
  warning when it could have held code; the walk still follows no link
  (#986).
- A `project.scripts` / `[tool.chokkin] entry` target in a file that exists
  but is left out of discovery (by `exclude`, or a member entry the root does
  not walk) is a skipped entry that lowers the findings it could hide, instead
  of being dropped; a target that names no file still lowers nothing (#933,
  #934).
- An invalid dev or type requirement line no longer lowers CHK005, and a
  PEP 723 block's gaps reach only that script's findings (#935, #936).
- `runpy.run_path()` and `globals()` / `vars()` / `locals()` `.update()` in
  Django settings are treated as unread settings, like `include()` and
  `exec()` (#937).
- An unresolved import in a public orphan module lowers that distribution's
  CHK002 (#942).
- A requirements line naming a wheel or sdist archive
  (`wheels/bar-1.0-py3-none-any.whl`) lowers the findings of that
  distribution, read from the archive's file name (#988).
- `setup.py` evaluation keeps list and dict aliases in sync: `reqs = base`
  followed by `base += ["click"]` declares `click` through `reqs` too, and
  `extras = {"all": base}` sees later additions to `base` (#565).

## [0.7.6] - 2026-10-16

v0.7.6 ships the first version of the unknown-input contract (ADR 0005).
The JSON / baseline `schema_version` stays `"1"`, and fingerprints are
unchanged, so baselines keep matching. What chokkin could not read — an
import it cannot resolve, a skipped entry, an unreadable module, notebook or
Django settings, an invalid requirement line, an assumed target version —
now lowers the findings it could hide instead of being dropped, so a
dependency such a gap might use is reported as Likely and `--fix` leaves it.
The same signals reach every reporter as warnings, not only `json`: expect
new `Warnings: N` lines, `::warning` annotations and SARIF notifications on
projects that chokkin cannot fully read. Requirements files are read more
like pip (line continuations, inline options, whitespace and markers), and
transformers, llama_index and airflow analyze markedly faster.

### Added
- The GitHub Action's `version` input also takes the path of a chokkin wheel
  (`*.whl`, relative to `working-directory`), which it runs with
  `uvx --from` (#834).
- Literal `__all__ += [...]`, `__all__.extend([...])` and
  `__all__.append("...")` add to the export list after a literal
  `__all__` assignment, also from inside top-level `if` / `try` / `with`
  blocks, so the added names count as public API (CHK006) and pass through
  star imports. A non-literal addition (`__all__ += sub.__all__`) warns and
  keeps the list (#567).
- Signals the pipeline used to drop now reach every reporter as warnings:
  parse diagnostics, entry and member warnings, manifest warnings of members
  and extra requirement files, ambiguous imports, an unreadable venv, member
  `sources:` problems, `--production` test-walk warnings, notebooks with no
  readable `cells` or unreadable code cells, and unreadable directories.
  `default` and `compact` end with `Warnings: N (see stderr)`, `markdown`
  lists them, `github` emits `::warning` annotations with `file=` / `line=`,
  `sarif` writes them to `invocations[].toolExecutionNotifications` with
  `locations`, and `json` keeps them in `diagnostics`. Duplicate lines are
  dropped; stderr, `markdown` and `github` show the first 5 of a kind and
  `… and M more like the above`, while `json` and `sarif` keep every line.
  No finding changes from this alone (#877, #892–#895, #898–#900).

### Changed
- CHK005 for a package that runtime code imports only inside functions is
  info instead of warning when the same workspace member also treats the
  package as optional: it imports it under `try/except ImportError` (a
  platform guard alone does not count), or code the member reaches defines an
  `is_<name>…` predicate for it. Imports in test support a package ships (a
  `test` / `tests` / `testing` module or subpackage below the top-level
  package, such as `django/test/selenium.py`) are info too, unless a
  top-level import already makes them certain. Django's `pillow` and
  `selenium`, werkzeug's `cryptography`, streamlit's `polars` and `xarray`
  and mlflow's `pyspark` drop to info (#808).
- What chokkin could not read now lowers the findings it could hide, with a
  warning naming it (#888):
  - a reachable import that resolves to no distribution (CHK010) lowers
    CHK002 to likely (#911);
  - a command in a task file or tool config (`Makefile`, `justfile`,
    `Dockerfile`, `Procfile`, GitLab CI, PDM scripts, pre-commit local hooks,
    tox, `scripts/` and `bin/` shell scripts) that runs Python code no entry
    reaches, or code chokkin cannot read, lowers CHK001 to likely (#913);
  - Django settings that cannot be found or read lower CHK001 and Django's
    CHK002 to likely. `DJANGO_SETTINGS_MODULE` set beside a nested
    `manage.py`, `wsgi.py` or `asgi.py` now resolves from that directory (#914);
  - a nested `.gitignore` that cannot be read (#915) and a `requires-python`
    that cannot be read (#917) now warn instead of being silently ignored;
  - a notebook with code cells chokkin cannot read lowers CHK001 and CHK002
    to likely, and `--explain` names the gap with its first file (#902);
  - a module imported by a name computed at runtime (`importlib.import_module(x)`)
    warns once per run that unused files are reported below certain (#909);
  - a workspace member whose runtime dependencies cannot be read lowers only
    that member's CHK003 and CHK004 (#910);
  - an entry outside discovery (`docs/conf.py` excluded by the walk) lowers
    CHK001 and CHK002 of its project or member to likely (#923);
  - only imports whose stdlib status depends on the Python version
    (`tomllib`, `binhex`) drop to info / maybe for CHK003, CHK004 and CHK010
    when the target version is assumed (#924);
  - Django settings that `include()` or `exec()` other files at import time
    count as unread (#925);
  - an invalid requirement line turns imports of the name it starts with into
    info / maybe hints, and its warning ends with
    ``; undeclared imports of `name` are reported as hints`` (#928);
  - in a library, a public module chokkin skipped lowers CHK002 for what it
    may import to likely, so `--fix` keeps a dependency only that module
    imports; `--explain` evidence reads the same files as the verdict (#550).
  Every lowered finding stays in the report; `--fix` touches only certain
  findings, so a lowered one is no longer auto-removed.
- `--production` no longer walks the tree a second time for the dropped test
  files: a cold run is about 17% faster on llama_index and 11% on airflow,
  with identical output (#844).
- Parsed ignore files are shared between the root and member walks, which now
  run side by side: a cold `--probe` on llama_index takes about half as long,
  with identical output (#757, #758).
- The sdist ships only `src/`, `Cargo.toml`, `Cargo.lock`, `pyproject.toml`,
  `README.md` and `LICENSE` (188 files instead of 1100), and no longer the
  `.cargo/config.toml` that asked for `lld`, so building it no longer needs
  `lld` installed (#701).

### Fixed
- Python code after a backslash continuation (`python \` then `-c '...'` on the
  next line) and a script run through a variable interpreter after `uv run`
  (`uv run $(PYTHON) gen.py`) are now read from commands (#861).
- `--fix` on `setup.cfg` removes only the reported entry: an extra's entry no
  longer takes the same name from other extras or from unrelated sections
  (`[flake8] exclude`), line endings (CRLF) and the rest of the file stay as
  written, and an emptied `install_requires` keeps its key like an emptied
  extra (#861).
- `--fix` appending to a multi-line `project.dependencies` (CHK003
  `--add-missing`, a CHK005 move) puts the new entry on its own line at the
  existing indent; the last entry's `# comment` stays on that entry's line
  instead of moving to the new one, and `[\n]` no longer becomes `["x"\n]`
  (#861).
- Import guards are read through `not` / `and` / `or` and along an `if` /
  `elif` / `else` chain: a branch that runs only when an earlier
  `sys.platform`, `TYPE_CHECKING`, `__main__` or package-availability test
  failed is classified like that test's own body, and a condition chokkin
  cannot read keeps its imports runtime. The `else:` after
  `if sys.platform == "darwin":` is platform-guarded, so Airflow's undeclared
  `setproctitle` is a CHK004 warning instead of an error (#861).
- A source with a syntax error is skipped like an undecodable one, with a
  `parse: skipped …: syntax error at line N: …` warning, instead of being read
  as a module without imports. Its imports are unknown, so CHK001 and CHK002
  for what it may use drop to likely and `--fix` no longer removes a
  dependency only that file imports (#886).
- The negative `--trace` names an automatic entry root's rule once
  (`auto: __main__.py` instead of `auto: auto:__main__.py`) (#850).
- A runtime module that defines or re-exports (`__all__`) both PEP 517 hooks,
  `build_wheel` and `build_sdist`, is an entry root, so a build backend that
  frontends import by its `build-backend` name (poetry's `poetry.masonry.api`)
  is no longer CHK001 (#782).
- An in-tree build backend, a top-level module under `[build-system].backend-path`,
  is a dev build script like a hatchling hook: it is an entry root and its
  imports resolve through `[build-system].requires` (#782).
- The PEP 517 and PEP 660 hooks of either kind of build backend are no longer
  CHK006, since the frontend calls them (#782).
- A build script's import of a `[build-system].requires` distribution counts
  as a use of that distribution where the project declares it, so `--strict`
  no longer reports CHK002 for openai-python's `hatchling` in its `build`
  group. Such imports still need no declaration for CHK003–CHK005 (#783).
- A dependency group that declares a `[build-system].requires` distribution
  no longer gets CHK002 for its entries that `[tool.uv]
  build-constraint-dependencies` also pins: they export the build
  environment and are never imported (openai-python's `pathspec`, `pluggy`
  and `trove-classifiers`). The same name in another group is still checked
  (#819).
- Sphinx extensions that `docs/conf.py` takes from a constant in an imported
  first-party module (`extensions = BASIC_SPHINX_EXTENSIONS`) are now read,
  and the `sys.path` edits of that module apply to `conf.py`, so airflow's
  `devel-common/src/sphinx_exts` modules are no longer CHK001 and their
  `setup` is no longer CHK006 (#779).
- Sphinx's `html_theme` in `docs/conf.py` counts as a use of the declared
  distribution of that name (`furo`), an import that resolves to no package
  falls back to the declared or locked `<namespace>-<portion>` distribution
  (`sphinxcontrib.log_cabinet` → `sphinxcontrib-log-cabinet`,
  `sphinxext.opengraph` → `sphinxext-opengraph`), and declared `pytest-*`
  distributions are used whenever pytest is, even without a venv. `--strict`
  no longer reports these as CHK002, and airflow's unresolved `sphinxcontrib`
  imports are no longer CHK010 (#809).
- A name that a module takes through `from m import *` is recorded as a
  reference only in the modules on the star path that define or import it,
  instead of in every module on the path. Findings are unchanged; a cold run
  on transformers drops from about 3.8 s to 1.1 s (#803).
- The bundled package map maps the documentation extensions whose import
  name differs from the distribution: `sphinx-argparse` to `sphinxarg`,
  `sphinx-autoapi` to `autoapi`, `sphinx-notfound-page` to `notfound`,
  `sphinx-autodoc2` to `autodoc2`, `pymdown-extensions` to `pymdownx`,
  `mkdocs-macros-plugin` to `mkdocs_macros`, and `mkdocstrings-python` to
  `mkdocstrings_handlers.python`. airflow's `sphinxarg` and `autoapi` Sphinx
  extensions are no longer CHK010 (#822).
- A package re-export that an entry point names (`[project.scripts] instr =
  "instr:main"` with `from .cli import main` in `instr/__init__.py`) or a
  plugin references is no longer CHK007, since the console script or host
  reads it without an import (#743).
- An entry point or plugin target with an attribute path or extras
  (`tool = "tool.cli:Cli.main [cli]"`) marks the object it starts from
  (`Cli`) as used, so that class is no longer CHK006, nor CHK007 when a
  package re-exports it (#872).
- A host hook that a package re-exports from a submodule (a Sphinx
  extension's `from .core import setup`, a pytest plugin's `pytest_*` hook,
  a PEP 517 backend package's `build_wheel`) is no longer CHK007, since the
  host reads it from the package (#873).
- A bare `celery -A proj.app` or `flask --app web` module is an entry root,
  so it is no longer CHK001, and the app objects the tool looks up (`app`,
  `celery`; `app`, `application`, `create_app`, `make_app`) are no longer
  CHK006. Quoted values (`--app "web.factory:create_app()"`) are unquoted
  and a factory call marks the factory as used (#851).
- A setup.py subscript assignment whose base is not a dict
  (`reqs[0] = ...`) marks setup.py as partially read, so the findings it
  could hide are lowered, instead of being dropped silently; an unknown key
  on a known extras dict keeps the other extras (#827).
- Sphinx `extensions` / `html_theme`, Django `INSTALLED_APPS` /
  `MIDDLEWARE` / `ROOT_URLCONF` / `WSGI_APPLICATION`, `DJANGO_SETTINGS_MODULE`
  in `manage.py` and `pytest_plugins` references carry the line of the string
  that names the module, so an inline ignore on that line works; CHK003 and
  CHK010 messages name the setting instead of an `import` (#830).
- An arbitrary-equality (`===`) value in a manifest requirement is read the
  way `packaging` reads it: up to whitespace, `;` or `)`, commas included,
  with empty items dropped. `foo===1.0,,>=1` is accepted, and
  `foo===1.0, >=1` is an invalid requirement. `foo_ @ <url>` is an invalid
  requirement instead of a dependency with an empty name. Names and extras
  ending in `_` stay invalid even though `packaging` accepts them (#572).
- Only space and tab separate the tokens of a manifest requirement, as in
  `packaging`: `foo\u00a0>=1` and `foo>=1\u00a0,<2` are invalid requirements.
  Any whitespace is still allowed between an operator and its version and
  around the whole requirement (#879).
- `requires-python` with empty items (`>=3.9,`, `>=3.9,,<4`, ` , >=3.9`) sets
  the target version from its bounds, as `packaging`'s `SpecifierSet` reads
  it, instead of falling back to py311 (#880).
- Two entry points into one module both mark their objects as used; before,
  only the last one did, so the other was CHK006 (#884).
- `__all__` changed inside a loop, function or class
  (`for m in mods: __all__.append(...)`) and a non-dict setup.py
  `extras_require` warn instead of being dropped silently (#884).
- A partly unread setup.py `extras_require` keeps the extras it read, and a
  dynamic `optional-dependencies` is tracked as unread (#869, #905).
- A broken ignore file is reported once, naming the file, instead of once per
  walked directory (#903).
- Poetry `python = …` and `platform = …` dependency constraints become markers,
  so the dependencies they guard are no longer CHK002 (#888).
- `setup.cfg` accepts `key: value` lines and reads `file:` directives in
  `install_requires` and `extras_require`; a missing or escaping `file:` path
  marks those requirements unread instead of empty (#888).
- Requirement lines strip whitespace the way Python's `str.strip` does
  (`click>=\x1f8`), and requirements and constraints files split the marker
  the way pip does (`bar\u00a0; …`, `"; "` after a URL). `foo ;` has no
  marker. These were invalid requirements or CHK002 / CHK003 false positives
  (#889, #890).
- A requirements line ending in `\` continues on the next line as in pip, and
  the options pip accepts after a requirement (`--hash`, `--config-settings`,
  `--global-option`, …) are dropped, so hash-pinned and multi-line
  requirements declare their distribution instead of being invalid. An
  option pip rejects keeps the line invalid. `--fix` removes every physical
  line of a continued requirement and refuses any file with `--hash` in any
  form (#927).
- A partial or unknown `__all__` lets every name through `from m import *`,
  `_` names included, so a name used only through that star import is no
  longer CHK006; the same applies to a library's public API and to Sphinx and
  Django config star imports (#951).
- CHK009 `--fix` no longer removes a line that another context still reads
  through `-r`. When `requirements.txt` and `requirements-dev.txt` both
  include `base.txt` and only `requirements.txt` repeats its `requests`, the
  fix removes the `requirements.txt` line, and the issue is reported on the
  line the fix removes. A repeat with no line safe to remove is reported as
  info / likely (#860).

## [0.7.5] - 2026-10-12

v0.7.5 is a fix release on top of v0.7.4 that continues to target false
positives on real OSS projects. The JSON / baseline `schema_version` stays
`"1"`, and fingerprints are unchanged, so baselines keep matching. A uv
workspace root with several members is judged app or library like any other
root, deprecation shim modules and opt-in integration imports become info,
and functions a host calls by name, scripts run by path from CI, pre-commit
or skill docs, and packages in setup.py requirement tables are no longer
reported. Requirements files named after a context declare that context, and
member pytest `pythonpath` settings are honored. In library mode, CHK005 for an
import that only public subpackages reach stays Likely, so `--fix` leaves it.

### Changed
- A uv workspace root with two or more members is no longer always app mode.
  `mode = "auto"` judges it like any other root, so a root that names its
  distribution and ships its own package without app signals (mlflow,
  mcp-python-sdk) is a library: its public API is CHK006 info instead of
  warning, unreachable test files are no longer CHK001, and CHK005 counts
  imports in public modules that nothing reaches. Members of a
  library root are judged in library mode too (#776).
- A module that calls `warnings.warn` at module top level with a
  deprecation category (a name containing `Deprecat` or matching
  `Removed*Warning`) is a compatibility shim kept for outside callers: its
  CHK001 and its symbols' CHK006 are info. Airflow's `stats.py` and
  `security/permissions.py` are no longer reported (#778).

### Fixed
- A workspace member's own pytest config (`pytest.ini`,
  `[tool.pytest]` / `[tool.pytest.ini_options]`, `tox.ini`, `setup.cfg`) now
  replaces the root's for the tests below that member: its `pythonpath` is
  read relative to the member directory and its `--import-mode=importlib`
  applies there. Imports in airflow's `scripts/tests` (`pythonpath =
  [".", "ci/prek", "in_container"]`) and `task-sdk/tests` (`pythonpath =
  ["tests"]`) no longer report CHK010 (#775).
- A `setup.py` that builds an extra by concatenating lists
  (`extras["dev"] = extras["all"] + extras["testing"]`) no longer reports
  CHK009 for a requirement that the concatenation puts into one list twice.
  A requirement string is now kept once per evaluated list, since setup.py
  items carry no line to point a fix at; a different specifier is still a
  separate entry. transformers' six CHK009 findings (`accelerate`,
  `datasets`, ...) and `torch` go away (#726).
- The bundled package map maps `guardrails-ai` to `guardrails`, `faiss-cpu`
  and `faiss-gpu` to `faiss`, `griffe` to `griffe` (alongside `griffelib`),
  and `llama-index-core` and the `llama-index` umbrella to the
  `llama_index.core` namespace. A declared distribution of these no longer
  reports its import as CHK010, and a declared `griffe` is no longer CHK002
  with its import CHK003 against `griffelib`. `llama_index.core` imports with
  neither declared are now reported as CHK003 (#773, #780).
- A requirements file outside the fixed root names now declares its
  entries in the context its own path names instead of only naming the
  packages: `requirements/tests.in` declares a test group,
  `docs/requirements.txt` a docs group, and `requirements-optional.txt` an
  optional extra. Each file is its own group, so it never repeats another
  file or a `[dependency-groups]` mirror under CHK009. A file whose path
  names no context stays names-only, and a `.txt` compiled from a sibling
  `.in` is not a declaration (#786).
- A `setup.py` requirement table that `setup()` looks entries up from
  (transformers' `_deps`) now names every package in it, so `peft` and the
  other entries no extra uses are no longer CHK010. django's
  `tests/requirements/` files declare a test group, and `autogen` (mlflow's
  ag2 flavor) maps to `ag2`, `autogen` and `pyautogen`. CHK005 lists each
  declaration bucket once instead of "dev, dev" (#749).
- CHK005 reports a dev-only distribution as info / Likely when every
  runtime import of it is opt-in: under an `is_x` flag or predicate
  (django's `if is_psycopg3:`, streamlit's `if is_sympy_expression(body):`),
  in a subpackage named after it (mlflow's `pyspark` and `langchain` for
  `langchain_community`), or in airflow's `example_dags`. The top-level
  package's own name does not count, and example DAGs stay runtime files
  for CHK003 / CHK004 / CHK010 (#781).
- In library mode, CHK005 reports a top-level import as warning / Likely,
  not Certain, when only public modules that the package itself does not
  load reach it. Such modules are public subpackages that neither the
  package's own `__init__` nor an entry point imports, and modules that
  only their own Flask route or Celery task decorators register. Tests
  importing them do not count either. `--fix` no longer moves mlflow's
  `dspy`, `httpx` and `torch` to runtime, and `openai`, `polars`, `psutil`
  and `opentelemetry-exporter-otlp-proto-http` are Likely. CHK001 and CHK006
  reachability is unchanged (#802).
- Functions a host calls by name are no longer CHK006: a `pytest_*` hook in a
  module pytest loads as a plugin (listed in `pytest_plugins`, a `pytest11`
  entry point, or a module that imports pytest at run time), the `setup` of a
  Sphinx extension listed in `conf.py`, and a function decorated with a
  pluggy marker (`@hookimpl`, `@hookspec`, airflow's
  `@local_settings_hookspec`). airflow's `pytest_runtest_setup`, policy and
  listener hookspecs and mlflow's `pytest_sessionfinish` are no longer
  reported (#777).
- CHK001 no longer reports a `.py` file that a command runs by path:
  `python[3] <path>.py` or `uv run [options] <path>.py` (`-s` included) in a
  CI workflow `run:` step, a local pre-commit hook's `entry:`, or a code
  block or inline code span of `.agents/skills/*/SKILL.md` /
  `.claude/skills/*/SKILL.md`, also behind wrappers such as `bash -c` or
  `poetry run`. The path is read from the root, under a preceding `cd` or
  `uv run --directory`, for workflows also without the checkout `path:`
  prefix, and for skill docs also under the skill directory. These files are
  dev entries, and outside a package also dev files, so a dependency group
  they import is not misplaced and an undeclared import is reported only
  under `--strict`. fastmcp's `tests/downstream/_harness.py` and
  `changelog_entry.py` and airflow's `task-sdk/dev/generate_task_sdk_models.py`
  are no longer reported (#797).

## [0.7.4] - 2026-10-10

v0.7.4 is a fix release on top of v0.7.3 that continues to target false
positives on real OSS projects. The JSON / baseline `schema_version` stays
`"1"`, and fingerprints are unchanged, so baselines keep matching. Some
CHK003 / CHK005 / CHK009 / CHK010 findings become info, files under
`examples/` become dev context, nested projects that are not workspace
members are skipped, and many CHK001 / CHK006 / CHK007 / CHK008 / CHK010
findings go away. `--fix` no longer removes a CHK009 requirement that a
group, extra or build requirement repeats from the runtime declaration.
`python_requires` in `setup.cfg` / `setup.py` now counts as
`requires-python`.

The `v0.7.4` tag points at `34f187b`, not at the release commit
(`ab5a001`), so the published wheels also contain PRs #765–#772 and #774
(#576, #577, #722, #729, #730, #731, #748, #763, #764). They were listed
under Unreleased at release time and are folded into this section below.

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
- CHK010 for an import in an `except ImportError:` (or
  `ModuleNotFoundError`) handler is info instead of warning when the `try`
  body only imports stdlib modules that every Python `requires-python`
  allows ships, from its lower bound up: the fallback never runs
  (`dummy_threading`, Python 2's `SimpleHTTPServer` / `BaseHTTPServer`).
  A module added within the range (`tomllib` for `>=3.8`), a platform-only
  or optional-build module (`fcntl`, `sqlite3`), a lower bound below 3.8
  and a missing `requires-python` keep the warning. `python_requires` in
  `setup.py` / `setup.cfg` counts too (#764), so requests' fallbacks are
  info (#721).
- A workspace member used only through another used member's imports is no
  longer CHK002 when the root inventories only its own files (a src-layout
  root). The member files the root does not inventory are parsed for this
  check alone, so no other rule sees them. A name imported from a member's
  module (`from acme_util import y`) now reaches that member; the parent of
  a namespace package (`airflow` in a provider) still does not (#712).
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
- An import of a module in a directory that the file's own `sys.path` edit
  adds is no longer CHK010. The same holds for a directory added by a
  `conftest.py` above the file. The edits covered are
  `sys.path.insert` / `append` / `extend`, `site.addsitedir` and
  `monkeypatch.syspath_prepend`. Corpus examples: transformers'
  `Path(__file__).parents[1] / "utils"`, flask's `test_apps`, and mlflow's
  `syspath_prepend(".")`. Any path-like string literal in that file names
  a candidate directory, and the import counts only when the module is
  there, so a typo still reports. Sphinx extensions resolve the same way
  through `docs/conf.py`, so one found in `../exts` is no longer CHK001.
  `extensions.append(...)`, `.extend([...])` and `+= [...]` literals now
  count too (#719).
- Modules that exist only at build time are no longer CHK010. A version
  file that hatch-vcs (`[tool.hatch.build.hooks.vcs] version-file`),
  setuptools-scm (`[tool.setuptools_scm] write_to` / `version_file`) or
  pdm-backend (`[tool.pdm.version] write_to`) generates is first-party
  (`src/_black_version.py` → `_black_version`), and an import root that
  matches a root `[build-system].requires` name, exactly or by
  `py` / `python` affix, resolves as third-party (sqlalchemy's
  `from cython.cimports... import`). No distribution is attached, so
  CHK003–CHK005 are unchanged (#720).
- The bundled package map now knows the import names of more distributions
  whose name differs from the module: `pymupdf` (`fitz`), `weaviate-client`,
  `gremlinpython`, `presto-python-client`, `smbprotocol` (`smbclient`),
  `jpype1`, `shopifyapi`, `atlassian-python-api`, `hubspot-api-client`,
  `ag-ui-protocol`, `haystack-ai` / `farm-haystack`,
  `paddlepaddle` / `paddlepaddle-gpu` and a few others, plus `vertex_ray`
  from `google-cloud-aiplatform`. Namespace distributions get dotted
  entries: `databricks-agents` (`databricks.agents`, `databricks.rag_eval`),
  `google-cloud-firestore` (`google.cloud.firestore*`) and
  `azure-search-documents`, `azure-data-tables`, `azure-ai-agents`,
  `azure-ai-projects`, `azure-ai-inference`. Declaring one of them no
  longer reports its imports as CHK010 (or `google.cloud.firestore` as
  CHK003 / CHK004 on `google-api-core`) and the dependency as unused (#748).
- Names in a quoted type outside an annotation now count as read by their
  module, like quoted annotations (#545): the type of `cast("T", x)`, the
  constraints, `bound=` and `default=` of `TypeVar("M", bound="Foo")`, and
  the value of `X: TypeAlias = "list[Foo]"` or `type X = "Foo"`. A class or
  alias referenced only there is no longer CHK006 (airflow, langchain).
  Strings passed to other calls, such as sgqlc's generated
  `Field("UserConnection")`, still do not count; list generated code under
  `vendored` or `[tool.chokkin.ignore]` (#577).
- `python_requires` in `setup.cfg` (`[options]`) and `setup.py` (a
  `setup()` argument that reads as a string without running the file) now
  counts as `requires-python` when `pyproject.toml` declares none, with
  pyproject > setup.cfg > setup.py priority and a `MetadataConflict`
  warning when they differ. As in setuptools, a `[project]` table that does
  not list `requires-python` in `dynamic` rules the setup.* value out. It
  sets the inferred `target_version` and the stdlib range like
  `requires-python` does, so requests' dead
  `dummy_threading` / `BaseHTTPServer` / `SimpleHTTPServer` fallbacks are
  CHK010 info (#764).
- Performance: resolving imports through `sys.path` edits (#719) no longer
  slows down large projects. Each directory a candidate path starts from is
  listed once, and a candidate whose first segment it lacks is never
  `stat`ed; each candidate is checked once; and an unresolved import skips
  candidate directories that hold no file with its top-level name. On
  transformers the `statx` calls are about 15,100, against 27,600 at the
  release commit and 14,800 in v0.7.3, and airflow runs within 1% of
  v0.7.3's instructions instead of 9% above them. Output is unchanged
  (#763).
- CHK005 is info instead of a warning for two more imports that only run
  when the package is already there. A sniffio result compared through a
  variable (`library = sniffio.current_async_library()` then
  `if library == "trio": import trio`, httpx and openai) guards like the
  direct comparison, as long as the scope assigns that name nothing else.
  And a module its host loads may import the host: pytest in a `pytest11`
  entry-point target or a module defining a top-level `pytest_*` hook
  (mlflow), IPython in a module defining `load_ipython_extension`
  (python-dotenv). Other dev-only imports of those modules, and pytest /
  IPython imports elsewhere, are reported as before, and `--fix` does not
  move the info findings to runtime (#731).
- An import guarded by a name that an `except ImportError:` handler set to
  `None` is optional: after `try: import markdown` /
  `except ImportError: markdown = None`, the body of `if markdown:`,
  `if markdown is not None:` or an `and` chain with either only runs with
  the package, so CHK005 for it is info / likely instead of warning /
  certain and `--fix` no longer moves it to the runtime dependencies
  (CHK003 / CHK010 for it are info too). Flags set to `True` in the `try`
  body count in `and` chains too (#730).
- CHK006 no longer reports names that a config entry pulls in with
  `from m import *`: Sphinx reads `html_theme` and Django reads
  `INSTALLED_APPS` out of the namespace of `docs/conf.py` (root and
  workspace members) and of the settings module, so no importer ever names
  them. Chained star imports are followed, and a module with `__all__`
  hands over only the names it lists. A star import from any other module
  is still no reference. This clears airflow's `provider_conf.py`, which
  every provider's `docs/conf.py` star-imports (#729).
- Modules that the Pyodide runtime provides (`js`, `pyodide`,
  `pyodide_js`, `_pyodide`) are no longer CHK010. When nothing else
  resolves such a root, it is third-party without a distribution, so
  CHK003–CHK005 are unchanged and a venv, mapped or declared package of
  the same name still wins. This clears urllib3's emscripten contrib and
  its `@run_in_pyodide` tests (#722).
- CHK006 no longer reports names used through a package that star-imports
  them: after `from .sub.impl import *` in `pkg/__init__.py`, `pkg.name`
  and `from pkg import name` count as uses of `pkg.sub.impl.name`. Chained
  star imports are followed, and at each hop a module with `__all__` hands
  on only the names it lists, one without hands on the names not starting
  with `_`; a module that defines the name itself shadows the star-imported
  one. Whether the use is external is judged by the importing module,
  so a module reaching its own name through the package still counts as
  unused. This clears 33 of litellm's app mode findings, such as
  `litellm.amoderation` and `GCSLoggingConfig` (#576).

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
