# Changelog

All notable changes to `chokkin` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-14

First MVP release covering the §16 v0.1 scope. All §17 Phase 1 exit criteria
are met on the 20-project OSS validation set (CHK002 false-positive rate 0.0%,
recall sentinels 2/2, crash 0, cold medium-project run ≤ 2s); see
`docs/dev/oss-validation-report.md`.

### Added
- Full 13-step analysis pipeline (`src/pipeline`): root discovery, config load,
  manifest extraction, source-file discovery, config/plugin extraction, Python
  parse, import resolution, entry/root construction, reachability analysis,
  dependency reconciliation, symbol-usage analysis, issue emission, optional fix.
- Rules `CHK001`–`CHK010`: unused file (app mode), unused dependency, missing
  direct dependency, transitive-only import, misplaced dependency, unused public
  export (preview), unused re-export, unlisted binary dependency, duplicate
  dependency declaration, unresolved import.
- Manifest reading for `pyproject.toml` (`[project.dependencies]`,
  `[project.optional-dependencies]`, `[dependency-groups]`) and
  `requirements*.txt`, with `src`/flat layout detection.
- Bundled `package_module_map` / `binary_map` seeds (`data/*.seed.json`) for
  import-name ↔ distribution resolution and dev-tool CLI usage detection,
  augmented at runtime by installed `dist-info` `RECORD` / `entry_points.txt`.
- Plugins for `pytest`, `django`, and `fastapi`.
- Reporters: `default`, `compact`, `json`, `markdown`.
- CLI flags: `--production`, `--strict`, `--no-exit-code`, `--explain`,
  `--trace`, `--fix` (dependency removal only), `--reporter`, `--probe`.
- Optional/conditional import tracing (`try/except ImportError`,
  `sys.platform`, `TYPE_CHECKING`, extra guards) and dev-context policy for
  dependency groups / extras / `requirements-*.txt`.
- `pyproject.toml` with maturin `bin` bindings for Python wheel distribution.
- User-facing README (English and Japanese) and full design specification in
  `docs/dev/spec.ja.md` (§1–§21).
- Hardened CI/CD pipeline ported from `watany-dev/ptuf`:
  - `ci.yml`: fmt / clippy / nextest (ubuntu + macOS + Windows) / MSRV /
    coverage / cargo-deny / cargo-machete / semver-checks / actionlint / zizmor.
  - `audit.yml`: daily `cargo-audit`.
  - `release.yml`: maturin wheel build matrix + PyPI Trusted Publishing.
- OSS validation harness (`scripts/oss-*`, `make oss-clones` / `make oss-metrics`)
  with FP gate and recall sentinels.
- Static analysis configs: `clippy.toml`, `rustfmt.toml`, `deny.toml`,
  `.cargo/config.toml`; `Makefile` with `make check` pre-commit gate.
- Agent and guardrail configs: `AGENTS.md`, `CLAUDE.md`, `.claude/settings.json`,
  `.cursor/` (ptuf hooks), `scripts/bootstrap-agent.sh`.

### Changed
- **BREAKING:** Renamed the project from `yokei` to `chokkin` — CLI binary, PyPI
  package, `[tool.chokkin]` config table, `chokkin.toml` / `.chokkin.toml` config
  files, `# chokkin: ignore[…]` directives, and rule codes `CHK001`–`CHK010`.
