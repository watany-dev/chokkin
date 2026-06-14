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

## Skills

`.claude/skills/` provides task skills (modeled on
[ptuf](https://github.com/watany-dev/ptuf)). Invoke with `/<name>` or via the
Skill tool:

- `wrapup` — runs `simplify` then `update-docs`, runs `make check` if files
  changed, then reminds you to `/compact`. Auto-triggered by the Stop hook.
- `update-docs` — syncs `src/` changes into `README.md`, `README.ja.md`,
  `docs/dev/spec.ja.md`, `CLAUDE.md`, and `AGENTS.md`.
- `update-design` — scores `docs/dev/spec.ja.md` (5 categories × 20 pts) and
  flags design/code drift.
- `update-plan` — validates the plan file to `update-design` standards before
  `ExitPlanMode`.
- `grill-me` — interviews you about a plan/design, then records an ADR under
  `docs/adr/`.

## Stop hook

`.claude/settings.json` registers a `Stop` hook
(`.claude/hooks/stop-wrapup.sh`). When a session that edited files
(`Edit`/`Write`/`MultiEdit`/`NotebookEdit`) tries to end, the hook blocks and
asks you to run `wrapup`. The hook tracks state via a marker file
(`/tmp/chokkin-wrapup-<session_id>`) instead of `stop_hook_active`; `wrapup` must
`touch` that marker on completion or the Stop hook loops indefinitely. The hook
needs `jq`; without it (or outside a git repo) it fails safe and lets the
session end.

## Settings

`.claude/settings.json` also sets `effortLevel: high`, `language: japanese`,
`autoMemoryEnabled: false`, and `CLAUDE_CODE_DISABLE_1M_CONTEXT: "1"` alongside
the shell `permissions` above.

## Key constraints (quick reference)

- Never execute the analyzed project's Python code.
- Default CLI runs full analysis (steps 1–13); `--probe` is steps 1–4 only.
- All logic in `src/lib.rs` and submodules; `main.rs` is dispatch only.
- No `unsafe`, no `unwrap`/`expect`/`panic` outside tests.
- Use `std::path` for all path handling (cross-platform wheels).
