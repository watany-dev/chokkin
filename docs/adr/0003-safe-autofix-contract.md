# ADR 0003: Safe autofix contract

## Status

Accepted for v0.4.

## Context

`chokkin --fix` edits dependency manifests and can optionally remove unreachable
files. A false-positive fix is more costly than a false-positive report, so the
fix boundary must remain narrower than the detector boundary and must be
reviewable before it writes.

## Decision

### Scope and opt-ins

- `--fix` enables fix planning.
- `--dry-run` runs the same plan without writing or deleting anything.
- `--allow-remove-files` is required before a `Certain` CHK001 file can be
  deleted.
- `--add-missing` is required before a `Certain` CHK003 dependency can be added.
- CHK002, CHK005, and CHK009 dependency edits are eligible only at `Certain`
  confidence. Other rules and lower-confidence findings are not auto-fixed.

CHK003 insertion supports non-Poetry `pyproject.toml` manifests with static
`project.dependencies`. A workspace finding is written only when that member's
manifest was inventoried. Dynamic dependencies, unsupported manifest shapes,
ambiguous declarations, and missing origins produce a `SkippedFix`; they do not
fall back to a guess.

### Filesystem safety

Every target is resolved relative to the discovered project root before an
action runs. Absolute paths, `..`, unresolved parents, and symlink targets that
resolve outside the root are rejected.

Manifest edits use a temporary file in the manifest's directory, flush it,
copy the existing permissions when available, and atomically persist it over
the original. File removal is intentionally different: it is a direct delete,
is never enabled by `--fix` alone, and is previewable with `--dry-run`.

### Reporting and external tools

The CLI writes applied previews/edits, skipped fixes with `SkippedReason`, and
lockfile reminders to stderr. A manifest edit can remind the user to run
`uv lock` or `poetry lock`, but chokkin does not run either command and does not
execute analyzed project code.

### Idempotency

Duplicate CHK003 actions for the same distribution and manifest collapse to
one action. Adding a dependency already present in `project.dependencies` is a
successful no-op rather than a duplicate entry. A fresh analysis after any
other successful fix is the source of truth for whether another action remains.

## Consequences

- New fix kinds must be opt-in when they can delete project content or broaden
  the current trust boundary.
- Supporting another manifest format requires an unambiguous edit and a
  regression test; unsupported inputs remain skipped.
- A failure applying one action is reported as skipped and does not panic.

## Verification

The contract is covered by existing tests:

- `fix::apply::tests::dry_run_does_not_write_files`
- `fix::containment::tests::rejects_missing_file_under_symlinked_outside_parent`
- `fix::write::tests::atomic_write_preserves_permissions`
- `fix::pyproject::tests::add_runtime_dependency_is_idempotent`
- `fix::plan::tests::deduplicates_chk003_add_missing_actions`
- `fix::plan::tests::add_missing_workspace_member_skip_names_member`

## References

- `src/fix/`
- `docs/dev/spec.ja.md` §13
- `docs/dev/plans/phase-3x-v0.4-reliability-contract.md` §6
