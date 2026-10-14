# ADR 0005: Unknown input contract

## Status

Proposed. Open questions 1 and 2 below must be decided before acceptance.

## Context

The rules that report something as unused (CHK001 unused file, CHK002 unused
dependency, CHK006 unused export, CHK007 unused re-export) claim that something
is absent. That claim is sound only if chokkin read every place the thing could
have been used. The rules that report a missing declaration (CHK003, CHK004,
CHK005) have the matching premise: they assume the list of declarations is
complete.

Every extractor recognizes a fixed set of forms: parser visitors, manifest
readers, entry builders, and plugins (55 to 65 hand-written extractors and
tables). Nothing in the type system or the spec says what happens to a form an
extractor does not recognize. A stocktake on 2026-10-14 (main `23409be`) traced
that path for each extractor. The "could not read this" signal stops at one of
four layers:

| Layer | What happens | Examples |
|---|---|---|
| L0 | No signal: the form is dropped silently | Unsupported `__all__` forms; unknown loaders and guards; Django settings packages; scripts in task files; Poetry `optional = true`; setup.cfg `key: value` |
| L1 | A signal exists but never reaches the user | Every `ParseDiagnostic`, including syntax errors before #886; every `EntryWarning`; `AmbiguousImport` and `VenvUnreadable`; warnings from member manifests. Non-JSON reporters print no diagnostics at all |
| L2 | The user sees the signal, but no rule reads it | `PluginsWarning`, `SetupPyPartiallyStatic`, `InvalidRequirementLine`, `ResolveConfidence` |
| L3 | A rule reads it, but the exit code does not change | App-mode CHK001 stays `Error` at `Likely` |

Only three signals reach both the user and the rules:

- `ParsedModule::skipped`;
- `has_opaque_dynamic_import`;
- the root manifest's `runtime_dependencies_unknown`.

Each of these covers one or two rules.

As a result, an unrecognized form becomes a confident false positive. In the
worst case, `--fix` deletes a dependency that is in use. Users report it, and
the fix adds the form to the recognized set, but nothing defines what happens
to the next unrecognized form. At least 22 of the 46 open issues on
2026-10-14 are instances of this. Among recently closed issues, about 20 are
recurrences of the same structure. Adding forms one issue at a time does not
converge.

The existing code already contains the pattern this ADR generalizes:

- An undecodable source is `skipped`. It is not a CHK001 candidate, and it
  lowers CHK002 to `Likely`. Its warning reaches stderr and JSON. Since #886,
  sources with a syntax error take the same path.
- `RuntimeDependenciesUnknown` lowers CHK003 and CHK004 to info/`Maybe`, and
  `--explain` gives the reason.
- A star import from a module that was not parsed binds an opaque set of
  names (`rules/symbols/graph.rs`).
- Config uses `deny_unknown_fields`, so an unknown key is an error, not a
  silent drop.

## Decision

### 1. An extractor reports what it could not read

When a rule treats an extractor's output as complete, the output type must
also be able to say it is incomplete. The output is one of:

- **Known**: everything relevant was read;
- **Partial**: some items were read, and there is a gap;
- **Unknown**: nothing usable was read.

A plain `Vec` or `Option` that collapses Partial and Unknown into Known does
not meet this. Current examples:

- `exports: Vec<String>` (`unwrap_or_default`);
- `EntrySpec.symbol: Option<String>` (the last value wins);
- extras that are not a dict becoming `&[]`;
- the dropped `push_items` completeness bool.

### 2. One channel for gaps

Each gap is recorded once, as a value that states three things:

- its **source**: file and line;
- its **cause**: what form was not understood;
- its **scope**: which rule subjects it could affect.

Per-module ad hoc flags (`skipped`, `has_opaque_dynamic_import`,
`runtime_dependencies_unknown`) become kinds of gap. During migration they
remain as inputs to it.

### 3. Every gap reaches the user

Each gap produces one warning. The warning goes to stderr and to JSON
`diagnostics`, and the human-readable reporters print it too. There are no
gaps that are recorded but never shown. A false positive shown together with
its cause ("could not read X at line N") is something the user can act on. A
false positive with no visible cause becomes a bug report.

### 4. Rules apply gaps in one stage

A single stage in `rules/emit.rs` applies gaps. Individual rules do not apply
them on their own.

- **Absence findings** (CHK001, CHK002, CHK006, CHK007): if the finding's
  subject lies inside a gap's scope, the finding is lowered below `Certain`.
  `--fix` never acts on it (ADR 0003).
- **Missing-declaration findings** (CHK003, CHK004, CHK005): if the
  declaration list they compare against has a gap, the finding is lowered in
  the same way. This is what `RuntimeDependenciesUnknown` does today for the
  root manifest only.
- `--explain` names the gap that lowered the finding.

The individual demotion paths that exist today move into this stage.

### 5. New forms state their unknown path

A change that adds an extractor, or a form to an existing one, states what
happens to input it does not recognize. Silently dropping is allowed only
when the dropped input cannot affect any rule, and a comment must say why.
Review (`/wrapup`, `/code-review`) checks for this. Issues filed for leftover
work (`kick` step 6) record whether the issue is a missing form or a missing
gap.

## Open questions

### 1. Scope granularity

A gap's scope decides how much recall the contract costs.

- **Global**: any gap lowers every absence finding. This is simple and sound.
  But most real projects have at least one gap, so certain findings would
  almost disappear.
- **Per gap kind (proposed)**: each gap kind declares the narrowest scope that
  is still sound. Some examples:
  - A partial `__all__` in module M affects CHK006 and CHK007 in M only.
  - Unknown extras in member X affect CHK003 and CHK005 for X only.
  - An unresolved entry target affects CHK001 for files under the target's
    package.
  - An unreadable module's imports are unknown, so it affects CHK002 for
    every dependency it could reach and CHK001 for every file. This is what
    `skipped` does today.
- **Per subject with evidence**: narrow the scope further by using partial
  evidence, for example the names on the lines that did parse. This gives the
  most recall, but every narrowing is another hand-written form. That is the
  same trap as the current approach, so this ADR does not adopt it.

Acceptance requires a recall measurement of the proposed scopes on the pinned
corpus (`make oss-mutation`, `make oss-diff`; see
`docs/dev/oss-differential-and-recall.md`). The measurement must compare
certain findings before and after.

### 2. Exit code for lowered findings

App-mode CHK001 is still `Error` at `Likely` (`rules/chk001.rs`), so a
lowered finding still fails CI. There are two options:

- **Keep it**: lowering then protects only `--fix` and `--confidence`
  filtering.
- **Exempt a finding lowered by a gap from the error threshold**: this is a
  change to exit-code policy. Under ADR 0004 it needs a release note, and if
  it changes the meaning for the same invocation and issue set, a breaking
  release.

## Consequences

- Recognizing a new form is still worth doing, but it becomes recall work. It
  is no longer a false-positive fix.
- A false positive caused by an unrecognized form shows up as a lowered
  finding with a named cause, not as a confident error.
- Output gains warnings for gaps that are silent today. Projects that relied
  on a clean stderr will see new lines. Under ADR 0004, prose and warnings are
  not a compatibility contract.
- Some `Certain` findings become `Likely` or `Maybe`. ADR 0004 treats fewer
  false positives with the rule's meaning kept as a non-breaking change. The
  exit-code part is open question 2.

## Migration

This is ordered so that each step is useful without the next ones.

1. Route the L1 signals into `ProbeWarning` without changing rules:
   `ParseDiagnostic`, `EntryWarning`, member manifest warnings, and
   `read_extra_requirements`. Print diagnostics in every reporter.
2. Give the lossy outputs a Partial state: `exports`, `EntrySpec.symbol` (a
   set, #884), and extras per extra.
3. Introduce the gap value and the demotion stage in `rules/emit.rs`. Move
   the three existing signals onto it, with no change in behavior.
4. Connect the L0 and L2 sources one gap kind at a time. Each kind comes with
   its scope and a fixture.

The false positives confirmed in the stocktake become the acceptance fixtures
for steps 3 and 4. They are not filed as separate issues.

## References

- `docs/adr/0003-safe-autofix-contract.md`
- `docs/adr/0004-semver-contract.md`
- `docs/dev/spec.ja.md` (§ on `ParsedModule::skipped`, #486)
- #886: sources with a syntax error are skipped
