# AGENTS.md

`chokkin` is a Rust binary shipped as a Python wheel (maturin `bin`) that
reports unused files, dependencies, and symbols in Python projects. The design
spec is `docs/dev/spec.ja.md`; read the relevant section before changing
analysis logic.

## Rules

- **Never execute the analyzed project's code.** Static parse only: no
  `import`, `exec`, or spawning it. Django settings, `setup.py`, etc. may have
  side effects.
- Logic lives in `src/lib.rs` and submodules; `main.rs` only dispatches
  arguments and maps exit codes.
- The library is not a public API (ADR 0004): modules are private, items are
  `pub(crate)`, and tests/benches import from `chokkin::internals` — add an
  item there only when they need it.
- Use `std::path` for all paths; wheels ship for Linux/macOS/Windows.
- Run `make check` before every push.
- Commit messages: imperative mood with a type prefix (`feat:`, `fix:`,
  `refactor:`, `chore:`, `docs:`, `ci:`). Push to a feature branch, not `main`.
