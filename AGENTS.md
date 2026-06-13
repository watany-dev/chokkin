# AGENTS.md — Coding agent guidance for yokei

This file is read by Claude Code, GitHub Copilot, Codex, Cursor, and other
coding agents working on this repository.

## Project overview

`yokei` is a Rust binary shipped as a Python wheel (via maturin `bin` bindings).
It builds a project-wide reachability graph for Python projects and reports
unused files, dependencies, and public symbols — a [Knip](https://knip.dev/)
equivalent for Python.

**Status:** pre-alpha. Pipeline steps 1–4 run via `probe_project` (CLI probe
mode). Issue reporting and steps 5–13 are not wired yet. Phase 0 adds
`src/graph/` and `src/parser/` skeletons.
Implementation follows the phased roadmap in `docs/dev/spec.ja.md`.

## Repository structure

```
src/
  main.rs         CLI entry point — argument dispatch and process exit only
  lib.rs          Library crate — all logic goes here as the project grows
  discovery/      Project root discovery (pipeline step 1)
  config/         Config loading ([tool.yokei], pipeline step 2)
  manifest/       Manifest extraction (pipeline step 3; util.rs shared helpers)
  sources/        Source file discovery (pipeline step 4)
  graph/          Project graph skeleton (Phase 0; import edges)
  parser/         Python parse spike (`parse_file`, pipeline step 6 前提)
  cli.rs          CLI argument parsing (Phase 0 probe flags)
  pipeline/       probe_project — pipeline steps 1–4 orchestration
  resolver/       (future) import-name → distribution-name resolution
  rules/          (future) YOK001–YOK010 rule implementations
  reporters/      (future) default / compact / JSON / Markdown reporters
  fix/            (future) --fix: manifest-level edits only
  plugins/        (future) pytest / django / fastapi plugin implementations
pyproject.toml    maturin bin bindings — yokei ships as a Python wheel
docs/dev/
  spec.ja.md      Full design specification (§1–§21) — read before implementing
  ci-porting-notes.md  Deferred CI items to enable as code matures
```

## Critical design constraint: never execute project code

yokei analyzes Python projects via **static parse only**. It must never
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
- **Cross-platform** — yokei ships wheels for Linux/macOS/Windows. Always use
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
extraction, `sources` for the file-discovery walk) with synthetic fixtures
generated in `benches/support/mod.rs`. They are not part of `make check`;
run them when touching hot paths:

```bash
make bench                      # run all benchmarks
make bench-save BASELINE=main   # save a named baseline
make bench-cmp BASELINE=main    # compare current code against it
```

Baselines live under `target/criterion/`, so avoid `cargo clean` between
save and compare. Only land optimizations that show a significant
improvement in `bench-cmp`.

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

- `wrapup` — clean up changed code (`simplify`) then `update-docs`, run
  `make check` if files changed, then remind to compact context.
- `update-docs` — sync `src/` changes into `README.md`, `README.ja.md`,
  `docs/dev/spec.ja.md`, `CLAUDE.md`, and `AGENTS.md`.
- `update-design` — score `docs/dev/spec.ja.md` (5 categories × 20 pts) and
  flag design/code drift.
- `update-plan` — validate a plan to `update-design` standards before finalizing.
- `grill-me` — interview you about a plan/design, then record an ADR under
  `docs/adr/`.

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
