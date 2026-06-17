# ADR 0002: Plugin API RFC (v0.3)

## Status

Accepted (RFC only — no external plugin loading in v0.3)

## Context

v0.2 ships built-in plugins only (`pytest`, `django`, `fastapi`, `flask`, `celery`,
`tox`, `nox`, `pre_commit`, `github_actions`, plus static config scanners for Sphinx,
MkDocs, and Alembic). v0.3 stabilizes machine-readable contracts but does **not**
publish a stable external plugin loading API. This ADR documents the I/O boundary for
future external plugin prototypes.

## Decision

### Plugin outputs (stable contract direction)

Every plugin, built-in or external, must emit only these four hint kinds:

1. **Entry roots** — files or `path:symbol` entry points that seed reachability.
2. **Module refs** — dotted module names referenced by config or static literals.
3. **Symbol refs** — `module:symbol` pairs referenced by config or decorators.
4. **Binary usages** — CLI executable names used by config, scripts, or CI steps.

Plugins must not execute analyzed project Python code. Parsing is limited to
TOML/YAML/INI and Python literal/AST extraction in config files.

### Plugin inputs (proposed)

A future external plugin receives a read-only snapshot:

```text
PluginContext {
  project_root: Path
  config: &ChokkinConfig
  sources: &DiscoveredSources
  manifest: &LoadedManifest
  workspace_members: &[ResolvedWorkspaceMember]
}
```

and returns:

```text
PluginResult {
  plugin_id: String
  entry_roots: Vec<EntrySpec>
  module_refs: Vec<ModuleRef>
  symbol_refs: Vec<SymbolRef>
  binary_usages: Vec<BinaryUsage>
  diagnostics: Vec<PluginDiagnostic>
}
```

### Loading model (deferred)

v1.0 may add one of:

- **Process boundary**: subprocess plugin with JSON stdin/stdout (no Python import).
- **WASM boundary**: sandboxed plugin modules with the same JSON contract.

v0.3 does not implement either path. Built-in plugins remain compiled into the
`chokkin` binary.

### Versioning

Plugin results are not part of the v0.3 JSON reporter or baseline schema. When
external plugins ship, their outputs will be folded into the existing graph layers
(entry, resolver hints, binary usage) before issue emission so reporters stay stable.

## Consequences

- v0.3 can document extension points without committing to a loader.
- Built-in plugin refactors should keep the four hint kinds as the only public
  surface toward the graph layer.
- External plugin authors should prototype against JSON fixtures mirroring
  `PluginResult`, not against private `src/plugins/` internals.

## References

- `docs/dev/spec.ja.md` §9, §16 v1.0 list
- `src/plugins/extract.rs` — current built-in orchestration
- `docs/dev/plans/phase-2-v0.2-adoption.md` §5 — Phase 3 handoff
