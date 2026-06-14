# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
