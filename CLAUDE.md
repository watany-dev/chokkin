# CLAUDE.md

See [AGENTS.md](./AGENTS.md) for full project context, architecture, and
development principles. This file covers Claude Code-specific notes.

## Allowed shell commands

`.claude/settings.json` pre-approves:

- `make *` — run any Makefile target
- `cargo *` — build, test, clippy, fmt, doc, deny, audit, machete
- `git *` — status, diff, add, commit, push, log
- `uvx *` — maturin build/sdist, or other uv tool runs

For anything outside these prefixes, Claude will prompt for confirmation.

## Workflow

1. Before editing Rust code, read `docs/dev/spec.ja.md` (§6–§15) to understand
   where the new code fits in the module hierarchy.
2. After making changes, run `make check` to verify the pre-commit gate passes.
3. Commit with an imperative-mood message (`feat: ...`, `fix: ...`, etc.).
4. Push to the feature branch; do **not** push directly to `main`.

## Key constraints (quick reference)

- Never execute the analyzed project's Python code.
- All logic in `src/lib.rs` and submodules; `main.rs` is dispatch only.
- No `unsafe`, no `unwrap`/`expect`/`panic` outside tests.
- Use `std::path` for all path handling (cross-platform wheels).
