# v0.2 Schema and Migration Notes

This note documents the v0.2 draft machine-readable contracts for CI adoption.
The contracts here are intentionally narrower than the future v1.0 stability
contract: v0.2 should avoid needless breaking changes, but the stable JSON
schema is still a Phase 3 deliverable.

## Scope

Covered draft formats:

- `--reporter json` output
- `--baseline PATH --update-baseline` file
- Compatibility expectations for CI consumers that read JSON, baseline files,
  GitHub annotations, or SARIF

Not covered:

- Internal `.chokkin/cache/**` records. Cache files are implementation details
  and may be discarded safely.
- Human reporters (`default`, `compact`, `markdown`, `github`) as a stable
  parse target. Use JSON or SARIF for automation.

CI-facing location paths are normalized with `/` separators in JSON, SARIF
artifact URIs, GitHub annotation `file=` properties, and baseline fingerprints.

## Compatibility Policy

Until Phase 3, the JSON reporter and baseline file are draft schemas. v0.2
still treats the following as breaking changes and should avoid them unless a
release note and migration path are provided:

- removing or renaming an existing field
- changing a field type from scalar to object/array, or the reverse
- changing the meaning of an existing field
- changing baseline fingerprint inputs without accepting or regenerating the
  old format
- changing exit behavior for the same issue set without an explicit CLI flag

The following are backward-compatible in v0.2:

- adding new top-level fields
- adding new issue fields
- adding new rule codes
- adding new values to documented string fields such as `mode`, `severity`, or
  `confidence`
- changing issue order when consumers are expected to key by `code` and subject
  fields rather than array position
- adding SARIF properties or GitHub annotation details

Consumers should ignore unknown fields and should not depend on field order.

## JSON Reporter Draft

`chokkin --reporter json` writes a single JSON object:

```json
{
  "version": "0.2.0",
  "project": "project-name",
  "mode": "app",
  "production": false,
  "issues": [],
  "summary": {
    "total": 0,
    "by_code": {}
  },
  "suppressed": {
    "baseline": 0
  }
}
```

Top-level fields:

| Field | Type | Notes |
| --- | --- | --- |
| `version` | string | chokkin version that produced the report |
| `project` | string | project name, or `"(unknown)"` when unavailable |
| `mode` | string | effective analysis mode |
| `production` | boolean | whether production-only filtering was enabled |
| `issues` | array | emitted issues after baseline suppression |
| `summary.total` | integer | count of emitted issues |
| `summary.by_code` | object | issue count keyed by rule code |
| `suppressed.baseline` | integer | count suppressed by baseline fingerprint |

Each issue currently contains:

| Field | Type | Notes |
| --- | --- | --- |
| `code` | string | rule code such as `CHK002` |
| `severity` | string | reporter severity label |
| `confidence` | string | confidence label |
| `message` | string | user-facing message |
| `fingerprint` | string | stable issue fingerprint using the same rule/target shape as baseline |
| `target` | string | stable target identifier used by the fingerprint |
| `workspace_member` | string or null | workspace member id when known |
| `file` | string or null | primary file location when known |
| `line` | integer or null | primary file line when known |
| `path` | string or null | file/import subject path when applicable |
| `distribution` | string or null | dependency subject when applicable |
| `symbol` | string or null | symbol/import subject when applicable |
| `binary` | string or null | binary subject when applicable |
| `manifest` | object or null | manifest origin with `file` and nullable `line` |

Subject fields are mutually sparse. A consumer should choose the first
applicable subject field for its domain instead of assuming every issue has a
`path`.

Recommended consumer keys:

- issue identity: `fingerprint`; if unavailable, use `code` plus the first
  non-null subject field relevant to that rule (`path`, `distribution`,
  `symbol`, or `binary`)
- workspace grouping: `workspace_member`
- baseline visibility: `suppressed.baseline`
- SARIF grouping: prefer `partialFingerprints["chokkin/v0"]`; otherwise use
  SARIF `ruleId` plus the primary location/subject, not message text
- location paths: `/`-normalized strings, regardless of host OS path separator

Message text is not a stable identifier.

## SARIF Draft

`chokkin --reporter sarif` emits SARIF 2.1.0 with the built-in CHK001-CHK010
rules and one result per emitted issue. Each result includes:

- `ruleId` with the CHK code
- `level` mapped from chokkin severity
- `message.text` for display only
- `locations[0].physicalLocation` when a file or manifest origin is available
- `properties.workspaceMember`, nullable, for workspace-scoped findings
- `partialFingerprints["chokkin/v0"]`, a stable fingerprint based on rule code
  plus the normalized target; workspace findings include the member prefix

The `chokkin/v0` fingerprint intentionally mirrors the baseline identity shape
so GitHub code scanning and baseline workflows group the same finding the same
way. It may gain a new fingerprint key in a future schema, but existing keys
should remain readable through the v0.2 draft compatibility window.

## Baseline Draft

`chokkin --baseline chokkin-baseline.json --update-baseline` writes:

```json
{
  "chokkin_version": "0.2.0",
  "generated_at": "unix:1710000000",
  "issues": [
    {
      "fingerprint": "stable-fingerprint",
      "code": "CHK002",
      "target": "stable-target"
    }
  ]
}
```

Fields:

| Field | Type | Notes |
| --- | --- | --- |
| `chokkin_version` | string | chokkin version that generated the baseline |
| `generated_at` | string | `unix:<seconds>` in the v0.2 draft schema |
| `issues` | array | frozen issue entries |
| `issues[].fingerprint` | string | stable issue fingerprint used for suppression |
| `issues[].code` | string | duplicated rule code for reviewability |
| `issues[].target` | string | stable target identifier used by the fingerprint; workspace issues include a `member:` prefix |

Baseline fingerprints are based on `rule_id + stable target`. Paths are
normalized with `/` separators. Dependency and file issue fingerprints do not
include line numbers, so routine code movement should not force a full baseline
refresh. For workspace findings, the stable target includes the workspace member
id before the target so one member's accepted issue does not suppress another
member's finding with the same path or distribution.

## Migration Guidance

For v0.2:

- Prefer regenerating the baseline with `--update-baseline` after intentional
  large cleanup or dependency moves.
- Regenerate baselines after adopting workspace strict mode so member-scoped
  fingerprints include the member prefix.
- Commit baseline changes separately from source fixes when possible, so review
  can distinguish newly accepted debt from resolved findings.
- Treat a malformed baseline as a configuration error. Do not silently ignore a
  checked-in baseline in CI.
- Keep baseline paths inside the project root. chokkin rejects paths that escape
  the root.

For future schema migrations:

- Readers should accept the current draft fields and ignore unknown additions.
- Writers should preserve atomic replacement semantics when rewriting baseline
  files.
- If fingerprint inputs change, the migration must either continue reading the
  previous fingerprint shape or require an explicit `--update-baseline` rewrite
  documented in release notes.
- Cache schema changes do not require migration; users can remove
  `.chokkin/cache` or run with `--no-cache`.

## Phase 3 Handoff

Phase 3 should turn this draft into a stable contract by:

- publishing a JSON Schema for `--reporter json`
- deciding whether baseline files need an explicit `schema_version` field in
  addition to `chokkin_version`
- documenting a semver rule for reporter and baseline compatibility
- keeping at least two minor versions without breaking JSON or baseline readers
  before v1.0
