# GitHub Copilot instructions for yokei

See [AGENTS.md](../AGENTS.md) for full project context. Key points for Copilot:

- `yokei` is a Rust binary distributed as a Python wheel (maturin `bin` bindings).
  Implementation is in `src/lib.rs`; `src/main.rs` is argument dispatch only.
- **Never execute the analyzed project's Python code.** Static parse only.
- No `unsafe`, no `unwrap`/`expect`/`panic` outside `#[cfg(test)]`.
- Run `make check` before committing: fmt → lint (clippy + doc) → test → deny → machete.
- Commits use imperative mood with type prefix: `feat:`, `fix:`, `chore:`, etc.
- When making changes to the analysis engine, read `docs/dev/spec.ja.md` first.
- All new logic goes into `src/lib.rs` (and submodules), not into `main.rs`.
- Use `std::path` for path handling — yokei ships cross-platform wheels.
