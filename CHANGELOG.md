# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- PEP 735 `{include-group = "..."}` in `[dependency-groups]` is expanded
  transitively (group names normalized). A group pulled into a runtime group
  counts as runtime for CHK002 / CHK005; requirements stay under their declaring
  group, so CHK009 and `--fix` never act on the including group. `--explain`
  shows the include path, and undefined or circular includes become manifest
  warnings.
- PEP 723 inline script metadata (`# /// script`): each script is an entry root
  with its own dependency scope. Its third-party imports are checked against
  the script block instead of the project manifest, reported as
  `CHK002` / `CHK003` with subject `script:<path>:<distribution>`, and its
  `requires-python` lower bound drives parse and stdlib classification for that
  file. `--probe` lists detected scripts; invalid or duplicate blocks produce a
  warning and the file stays an ordinary source. `--fix` does not rewrite
  script blocks.

## [0.4.1] - Unreleased

### Fixed
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

### Changed
- Performance: files are parsed across worker threads, warm-run parse cache
  keys come from file stat `(size, mtime)` instead of hashing contents, and
  parse results are stored in one bundle per cache context.
- The module-index scan cache was removed; it could never beat a rebuild.
- JSON and SARIF reporters are rendered with `serde_json`. Field order and
  values are unchanged, but whitespace of the pretty-printed output may differ.
- Parse and manifest cache unit versions were bumped (`parse-v5`,
  `manifest-extract-v2`), so existing `.chokkin/cache` entries are rebuilt on
  the first run after upgrading.

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
