# AGENTS.md — Coding agent guidance for yokei

This file is read by Claude Code, GitHub Copilot, Codex, Cursor, and other
coding agents working on this repository.

## Project overview

`yokei` is a Rust binary shipped as a Python wheel (via maturin `bin` bindings).
It builds a project-wide reachability graph for Python projects and reports
unused files, dependencies, and public symbols — a [Knip](https://knip.dev/)
equivalent for Python.

**Status:** design phase (pre-alpha). The analyzer is not implemented yet.
Implementation follows the phased roadmap in `docs/dev/spec.ja.md`.

## Repository structure

```
src/
  main.rs         CLI entry point — argument dispatch and process exit only
  lib.rs          Library crate — all logic goes here as the project grows
  cli.rs          (future) CLI argument parsing
  config.rs       (future) Config file parsing ([tool.yokei])
  manifest/       (future) pyproject.toml / requirements*.txt / lockfile readers
  parser/         (future) Python AST parser (Rust-based, static only)
  resolver/       (future) import-name → distribution-name resolution
  graph/          (future) project reachability graph
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

This runs: `cargo fmt --check` → `cargo clippy -D warnings` → `cargo test` →
`cargo doc -D warnings` → `cargo deny check`.

Install tools once with `make tools`.

## PR hygiene

- One logical change per PR.
- Commit messages: imperative mood with type prefix — `feat:`, `fix:`,
  `refactor:`, `chore:`, `docs:`, `ci:`.
- Include a test plan in the PR body.
- Squash-merge by default.

## Guardrail: ptuf

This repository uses [ptuf](https://github.com/watany-dev/ptuf) as a
pre-tool-use guardrail in Cursor sessions. See `.cursor/hooks.json` and
`scripts/bootstrap-agent.sh`.

For Claude Code, the `.claude/settings.json` restricts shell execution to
`make`, `cargo`, `git`, and `uvx`.
