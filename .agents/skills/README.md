# Codex skills

From the repository root, invoke `$kick #123 #456` in Codex. The supplied issues
are handled in one PR. `$wrapup`, `$ponytail-review`, `$cleanup-comments`, and
`$update-docs` can also be invoked individually.

These directories are relative symlinks to `.claude/skills/`, which remains the
single source for both clients. Edit the target `SKILL.md` files. Claude Code
continues to use `/kick` and its existing skill paths.

Dependencies:

```text
kick
├── wrapup
│   ├── ponytail-review
│   └── cleanup-comments
└── update-docs (when documentation needs synchronization)
```

`/code-review` has a self-review fallback in `wrapup`; no Claude plugin is
required. The `ponytail-debt` mention in `cleanup-comments` explains a comment
convention and does not invoke that skill.

GitHub access requires authenticated `gh` or an available GitHub tool. Build and
validation use the repository's existing toolchain and `make check`; the port
adds no executable dependency. Continuing PR monitoring every 15 minutes needs
a runtime that supports continued execution or scheduling. When unavailable,
`kick` reports the current PR state and the remaining monitoring work.

Start a new Codex session in this repository if it has not discovered the skills.
On checkouts that materialize symlinks as text (for example, Windows without
symlink support), enable Git symlink support and restore these five links.
