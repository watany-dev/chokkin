# ADR 0004: Compatibility and semver contract

## Status

Accepted for v0.4.

## Context

v0.3 introduced schema version `"1"`, published JSON Schema files, stable rule
metadata, and baseline compatibility with v0.2. v0.4 needs a single rule for
deciding whether later reporter, CLI, baseline, and rule changes require a
breaking release before those contracts are frozen for v1.0.

## Decision

### Stable compatibility surface

The compatibility surface is:

- JSON reporter and baseline fields, types, meanings, and schema versions;
- baseline fingerprint inputs and existing fingerprint readers;
- CLI flag names and meanings;
- process exit-code meanings, including the issue threshold for an unchanged
  issue set and invocation;
- accepted ignore-directive syntax;
- existing CHK rule IDs and their default severities.

Human-readable prose and formatting are not machine-readable contracts. The
Rust library API remains pre-1.0 and is monitored by `cargo-semver-checks`, but
this ADR does not freeze it before v1.0.

### Breaking changes

The following require a breaking release, or an explicit versioned migration
that keeps the old reader behavior:

- removing or renaming an existing required JSON/baseline field, changing its
  type, or changing its meaning;
- changing baseline fingerprint inputs without continuing to read the previous
  fingerprint shape;
- changing exit-code meaning for the same invocation and issue set;
- removing a CLI flag, changing an existing flag's meaning, or making an
  existing workflow require a new flag;
- rejecting previously accepted ignore syntax;
- removing or renaming a rule ID, or incompatibly changing an existing rule's
  default severity.

### Non-breaking changes

The following are compatible when documented in the changelog:

- adding optional top-level or issue fields within the schemas'
  `additionalProperties: true` policy;
- adding reporter metadata or a new CLI flag;
- adding a rule code, provided its default severity and exit-code impact are
  stated in release notes;
- improving messages or warning/info descriptions;
- changing issue order;
- reducing false positives or improving detection while retaining the existing
  rule meaning, field shapes, default severity, and exit-code policy.

Consumers must ignore unknown fields, key issues by `fingerprint` (or stable
rule/subject fields), and not parse message text or depend on array order.

### Migration policy

A breaking machine-readable format change gets a new `schema_version` and a
release note with an upgrade path. Baseline fingerprint migrations either keep
reading the previous shape or require an explicit documented
`--update-baseline` rewrite. Cache formats are excluded because `.chokkin/cache`
is disposable and `--no-cache` remains available.

The v1.0 readiness condition is two consecutive minor releases without a
breaking change to this compatibility surface. v0.3 is the first such release;
v0.4 qualifies as the second only if its release validation confirms this ADR.

## Consequences

- Optional schema growth does not require a schema-version bump.
- Detection quality can improve during v0.x without treating corrected output
  as a format break.
- A proposed compatibility break must include migration tests and release notes,
  rather than silently changing schema `"1"`.

## Verification

- `tests/schema_contract.rs` validates current JSON and baseline schema behavior.
- `baseline::store::tests::baseline_reads_v02_without_schema_version` preserves
  the v0.2 reader path.
- `tests/ignore_syntax.rs` freezes accepted ignore syntax.
- `tests::exit_codes_are_stable` and `rules::filter` tests freeze exit meanings.

## References

- `docs/dev/schema-migration-notes.md`
- `docs/schema/chokkin-report.schema.json`
- `docs/schema/chokkin-baseline.schema.json`
- `docs/dev/plans/phase-3x-v0.4-reliability-contract.md` §6
