# AGENTS.md — Coding agent guidance for chokkin

This file is read by Claude Code, GitHub Copilot, Codex, Cursor, and other
coding agents working on this repository.

## Project overview

`chokkin` is a Rust binary shipped as a Python wheel (via maturin `bin` bindings).
It builds a project-wide reachability graph for Python projects and reports
unused files, dependencies, and public symbols — a [Knip](https://knip.dev/)
equivalent for Python.

**Status:** v0.4.0 released on top of v0.1.0–v0.3.0. Default CLI runs
`analyze_project` (pipeline steps 1–13) with `default` / `compact` / `json` /
`markdown` / `github` / `sarif` reporters, `--explain`, `--trace`, `--fix`,
and baseline filtering. `--probe` runs steps 1–4 only (`probe_project`) and
reports resolved/inventoried workspace member counts when uv/chokkin workspaces are found;
resolver tags member-owned imports, treats cross-member imports as first-party, `--strict`
requires member-local direct dependency declarations, and reporters expose member ids on
workspace findings. Phase 2 cache policy plumbing exists via `CacheOptions` / `--no-cache`
(`.chokkin/cache`), and parse cache key primitives exist (`CacheKeyContext`,
`SourceFingerprint`, `ParseCacheKey`) with in-memory `ParseCacheStore` reuse and disk
`ParsedModule` JSON entries under `.chokkin/cache/parse/`. Config/manifest scan input
fingerprints and record metadata exist via `ScanInputFingerprints` / `ScanCacheKey` /
`ScanCacheRecord`; typed scan payload storage is wired for config scan, manifest
extraction, and module index cache. v0.2 release validation measurements were
recorded on 2026-06-15 with Rust 1.93: `make check`, OSS fixtures, full
20-project OSS gate, and Criterion cache benches passed; synthetic 10k warm
cache measured under 205 ms. Baseline CI adoption is dogfooded by this repo's
`chokkin-baseline` workflow job and checked-in `chokkin-baseline.json` (see
`docs/dev/v0.2-release-validation.md`). Phase 3 (v0.3) adds `schema_version` on
JSON reporter and baseline output, published JSON Schema under `docs/schema/`,
`[tool.chokkin.severity]` per-rule overrides wired through emit/reporters/exit codes,
stabilized SARIF rule metadata (`helpUri`, `fullDescription`), ignore-syntax
regression tests, and plugin API RFC (`docs/adr/0002-plugin-api-rfc.md`; no
external plugin loading). v0.4 focuses default CHK003 reporting on runtime
imports, keeps optional/platform-guarded missing imports informational, recognizes
aliased `TYPE_CHECKING` guards, adds an offline deterministic wheel metadata
harvester, and records safe-autofix/semver contracts in ADR 0003/0004. The fixed
20-project corpus reports 131 CHK003 findings (down from 964 at Step 0), with
0 unknown labels and all §17 gates passing. PyPI
v0.1 release is gated on §17 exit criteria (OSS dogfooding, false-positive
rate, cold-run performance) — measured by `make oss-clones` + `make oss-metrics`
over a 20-project set (`docs/dev/oss-validation-report.md`); `make oss-fixtures`
is the no-network in-repo skeleton. **The §17 CHK002 gate is met** (see
`docs/dev/oss-validation-report.md`): 0 false positives across the 20-project
validation set after Phase 1.5 remediation. Crashes 0, cold-run speed within
budget. PyPI **v0.1.0** through **v0.4.0** have been released.
`src/graph/` provides skeleton nodes, import edges, distribution → module links,
entry → file edges, and file → file reachability edges.
Implementation follows the phased roadmap in `docs/dev/spec.ja.md`.

## Repository structure

```
src/
  main.rs         CLI entry point — argument dispatch and process exit only
  lib.rs          Library crate — all logic goes here as the project grows
  discovery/      Project root discovery (pipeline step 1)
  config/         Config loading ([tool.chokkin], pipeline step 2)
  manifest/       Manifest extraction (pipeline step 3; util.rs shared helpers)
  sources/        Source file discovery (pipeline step 4)
  plugins/        Config/plugin extraction (pipeline step 5; pytest/django/fastapi,
                  Flask/Celery static app refs, tox/nox/pre-commit/GitHub Actions
                  binary usage detection, Sphinx/MkDocs/Alembic config entries)
  graph/          Project graph skeleton + import edges (Phase 0; extended in step 6)
  parser/         Python parse and notebook code-cell parsing (`parse_file`,
                  `parse_project_sources`, attribute access collection; pipeline step 6)
  cli.rs          CLI argument parsing (`clap`, Phase 1 flags)
  pipeline/       `probe_project` (steps 1–4), `analyze_project` (steps 1–13)
  resolver/       Import resolution (`resolve_imports`, bundled maps, venv RECORD/entry_points, pipeline step 7)
  entry/          Entry root construction (`build_entry_roots`, pipeline step 8)
  reachability/   Reachability analysis (`analyze_reachability`, positive/negative `--trace`; pipeline step 9)
  rules/          Dependency reconciliation (step 10), symbol usage (step 11),
                  and issue emission (step 12: `emit_issues`, `explain_issue`;
                  `severity.rs` / `metadata.rs` for Phase 3 overrides)
  reporters/      Built-in reporters: default, compact, json, markdown,
                  github, sarif
  schema/         JSON/baseline `schema_version` constants (Phase 3)
  fix/            Optional manifest fixes (step 13: `apply_fixes`; atomic writes, root containment)
pyproject.toml    maturin bin bindings — chokkin ships as a Python wheel
docs/dev/
  spec.ja.md      Full design specification (§1–§21) — read before implementing
  schema/         Published JSON Schema for report and baseline (Phase 3)
  ci-porting-notes.md  Deferred CI items to enable as code matures
```

## Critical design constraint: never execute project code

chokkin analyzes Python projects via **static parse only**. It must never
`import`, `exec`, or spawn the analyzed project's code. Django settings,
`setup.py`, and similar entry points may have side effects (DB connections,
env var dependencies, arbitrary code execution). Keep analysis in the static
parser and graph layers. If runtime tracing is ever needed, it must be an
explicit opt-in separated from the default flow.

## Development principles

- **New logic lives in `src/lib.rs`** (and its submodules), not in `main.rs`.
  The CLI layer only dispatches arguments and maps `ExitStatus` to process
  exit codes.
- **No `unsafe` code** — enforced by `Cargo.toml` `[lints.rust] unsafe_code = "forbid"`.
- **No `unwrap`/`expect`/`panic` in production code** — use `Result`/`Option`.
  These are allowed in `tests/` and `#[cfg(test)]` blocks via `clippy.toml`.
- **No debug helpers in production** — `dbg!`, `print!`, `println!`,
  `eprintln!` are denied by clippy except in tests and `main.rs` (which has
  `#![allow(clippy::print_stdout, clippy::print_stderr)]`).
- **Cross-platform** — chokkin ships wheels for Linux/macOS/Windows. Always use
  `std::path` APIs; never assume POSIX path separators.

## Pre-commit gate

Run before every push:

```bash
make check
```

This runs: `cargo fmt --check` → `cargo clippy -D warnings` + `cargo doc` →
`cargo test` → `cargo deny check` → `cargo machete`.

Install tools once with `make tools`.

## Benchmarks

Criterion benchmarks live in `benches/` (`manifest` for parsing-heavy
extraction, `sources` for the file-discovery walk, `cache` for warm parse-cache
reuse) with synthetic fixtures generated in `benches/support/mod.rs`. They are not part of `make check`;
run them when touching hot paths:

```bash
make bench                      # run all benchmarks
make bench-save BASELINE=main   # save a named baseline
make bench-cmp BASELINE=main    # compare current code against it
```

Baselines live under `target/criterion/`, so avoid `cargo clean` between
save and compare. Only land optimizations that show a significant
improvement in `bench-cmp`.

## OSS validation (Phase 1 §17 release gate)

The v0.1 release is gated on the §17 exit criteria, measured over a fixed set
of 20 real OSS projects (library / app / server / Django / FastAPI mix):

```bash
make oss-clones                 # clone the 20 pinned projects -> target/oss-clones/
make oss-metrics                # measure FP rate / crashes / speed -> target/oss-metrics/report.md
make oss-metrics ARGS=--gate    # same, but non-zero exit on any §17 miss
scripts/run-oss-fixture.sh --build   # in-repo regression skeleton (no network)
```

- `scripts/oss-clones.manifest` — the 20-project set (pinned tags; resolved
  SHAs land in `target/oss-clones/clones.lock.tsv`).
- `scripts/oss-fixtures.labels.tsv` — labels keyed by `(slug, code, target)`
  for CHK001–CHK010 findings (`fp` / `tp` are ground truth; `deferred` is
  heuristic triage awaiting validation);
  the §17 FP gate requires every **CHK002** finding classified (unknown and
  `deferred` both block the gate). CHK003 labels and per-rule coverage are
  informational; see `docs/dev/v0.3-stocktake-coverage.md`.
- `scripts/oss-recall.manifest` — in-repo recall sentinels (CHK002 unused deps +
  CHK003 missing dep) measured alongside clones; every `tp` label must appear in
  findings or the recall gate fails.
- `scripts/generate-chk003-labels.py` — heuristic CHK003 label generator (re-run
  after `make oss-metrics` when refreshing CHK003 triage).
- `docs/dev/oss-validation-report.md` — committed §17 CHK002 scorecard.
  **Current status: CHK002 FP gate met** (0/0 after Phase 1.5). Per-rule label
  coverage stocktake: `docs/dev/v0.3-stocktake-coverage.md`.

## PR hygiene

- One logical change per PR.
- Commit messages: imperative mood with type prefix — `feat:`, `fix:`,
  `refactor:`, `chore:`, `docs:`, `ci:`.
- Include a test plan in the PR body.
- Squash-merge by default.

## Agent skills (shared across Claude / Codex / Cursor)

Reusable task workflows ("Agent Skills") live under `.claude/skills/` — the
single source of truth. Each is a `SKILL.md` directory with `name`/`description`
frontmatter, the format Claude Code, Codex, and Cursor all share. The other two
hosts reuse the same files via symlink, so a skill is authored once:

- `.codex/skills` → `../.claude/skills` (Codex scans `.codex/skills/`)
- `.cursor/skills` → `../.claude/skills` (Cursor scans `.cursor/skills/`)

Available skills:

**Project skills** (authored in-repo):

- `wrapup` — clean up changed code (`simplify`) then `update-docs`, run
  `make check` if files changed, then remind to compact context.
- `update-docs` — sync `src/` changes into `README.md`, `README.ja.md`,
  `docs/dev/spec.ja.md`, `CLAUDE.md`, and `AGENTS.md`.
- `update-design` — score `docs/dev/spec.ja.md` (5 categories × 20 pts) and
  flag design/code drift.
- `update-plan` — validate a plan to `update-design` standards before finalizing.
- `grill-me` — interview you about a plan/design, then record an ADR under
  `docs/adr/`.

**Ponytail skills** ([DietrichGebert/ponytail](https://github.com/DietrichGebert/ponytail);
vendored via `skills-lock.json`; on-demand in Cursor with `/ponytail`, `@ponytail`, or
by name):

- `ponytail` — lazy senior dev mode: YAGNI ladder, stdlib-first, shortest diff.
- `ponytail-review` — over-engineering review of a change or file.
- `ponytail-audit` — whole-repo over-engineering audit (ranked deletion list).
- `ponytail-debt` — harvest `ponytail:` shortcut comments into a tracked ledger.
- `ponytail-gain` — measured-impact scoreboard from the benchmark harness.
- `ponytail-help` — quick reference for ponytail modes and commands.

Refresh vendored ponytail skills:

```bash
npx skills@latest add https://github.com/DietrichGebert/ponytail/tree/main/skills -y
mv .agents/skills/ponytail* .claude/skills/ && rm -rf .agents
```

**Host-specific notes.** The Stop-hook auto-trigger, `/compact`, `simplify`,
and `ExitPlanMode` referenced by some skills are Claude Code features. On Codex
and Cursor the agent auto-selects skills by relevance (or you invoke them by
name); run the `simplify` → `update-docs` → `make check` core manually and skip
the Claude-only marker/compact steps. The Stop-hook enforcement that nags
`wrapup` after edits lives only in `.claude/` (`hooks/stop-wrapup.sh` +
`settings.json`).

## Guardrail: ptuf

This repository uses [ptuf](https://github.com/watany-dev/ptuf) as a
pre-tool-use guardrail in Cursor sessions. See `.cursor/hooks.json` and
`scripts/bootstrap-agent.sh`.

For Claude Code, the `.claude/settings.json` restricts shell execution to
`make`, `cargo`, `git`, and `uvx`.
