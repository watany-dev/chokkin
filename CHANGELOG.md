# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
