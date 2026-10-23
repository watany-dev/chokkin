# Codex skills

From the repository root, invoke `$kick #123 #456` in Codex. The supplied issues
are handled in one PR. `$wrapup`, `$ponytail-review`, `$cleanup-comments`, and
`$update-docs` can also be invoked individually.

The workflow skills listed above are independent copies of the Claude skills.
Only the skill invocation notation changes from `/name` to `$name`; the workflow
and policies follow the original definitions. Edit `.agents/skills/` for Codex.
Claude Code continues to use its original files under `.claude/skills/`.

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

When a skill calls another skill, read its `.agents/skills/<name>/SKILL.md` and
follow those steps in the current task.

Start a new Codex session in this repository if it has not discovered the skills.

## Security audit

Invoke `$security-audit` in Codex or `/security-audit` in Claude Code to use
[Cloudflare's official security audit skill](https://github.com/cloudflare/security-audit-skill).
Automatic invocation is disabled for both tools; invoke the command explicitly
when you want an audit. This skill shares the upstream audit instructions through
`.agents/skills/security-audit -> ../../.claude/skills/security-audit`.
See the [installation notes](../../.claude/skills/security-audit/INSTALLATION.ja.md)
for the pinned source commit, requirements, validation, and update instructions.

## Formal spec extraction

Invoke `$formal-spec-extraction` in Codex or `/formal-spec-extraction` in Claude
Code to extract the de-facto spec from implementation code and hunt bugs with
Z3 / TLA+. The skill is an English translation of
[mizchi's playbook](https://gist.github.com/mizchi/db7817e6fc077d567c41cd9d41bb1c53)
and is shared through
`.agents/skills/formal-spec-extraction -> ../../.claude/skills/formal-spec-extraction`.
