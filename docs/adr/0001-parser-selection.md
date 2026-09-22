# ADR 0001: Python parser selection

## Status

Accepted

## Context

chokkin must parse Python source statically (never execute project code) to build
import edges for the reachability graph. Phase 0 evaluated:

| Criterion | Ruff ecosystem (`ruff_python_parser`) | `rustpython-parser` 0.4 |
| --- | --- | --- |
| crates.io availability | Third-party vendored forks only | Official crate |
| License | MIT (Astral) | MIT |
| Syntax coverage 3.10–3.13 | High | Good |
| Comment / token preservation | High | Moderate (location feature) |
| API stability | Low (no stable public crate) | Moderate |
| Wheel size | Medium (if vendored) | Larger transitive tree |

Spike fixtures target ≥ 95% success on representative inputs.

## Decision

Adopt **`rustpython-parser` 0.4** with the `location` feature for line numbers.

Rationale:

1. First-class crates.io dependency with MIT license and `cargo deny` compatibility.
2. Astral Ruff parser crates are not published as a supported standalone API; vendoring
   adds maintenance cost in Phase 0.
3. Spike scope is top-level `import` / `from … import` extraction only; full comment
   and `ignore` directive parsing lands in Step 6.

## Consequences

- Pin `rustpython-parser = "0.4"` with `default-features = false` and `num-bigint`
  (avoids LGPL `malachite-bigint` default).
- Re-evaluate Ruff parser vendoring in Step 6 if comment/`__all__` extraction quality
  is insufficient.
- Parser spike comparison code stays in tests only; production uses one backend.

## Triggers to revisit

- Step 6 fixture failures on Python 3.12+ syntax.
- `rustpython-parser` unmaintained for > 12 months.
- Astral publishes stable `ruff_python_parser` on crates.io.

## Amendment 2026-09-22 (issue #140)

Two of the triggers above have since fired, and the decision was re-examined:

- `rustpython-parser` 0.4.0 was last published 2024-08-06, about 25 months ago.
- `ruff_python_parser` is now on crates.io, but at 0.0.14 and self-described as
  "an internal component crate of Ruff" — published, not stable.

The decision stands: **keep `rustpython-parser` 0.4** for now. `ruff_python_parser`
does not yet offer the stable public API this ADR made a precondition, and no
head-to-head 10k measurement exists to back the switch on speed. The comparison
table, the known PEP 695 coverage gap, the reproducible spike recipe, and the
replacement re-evaluation conditions are recorded in
[`docs/dev/issue-140-parser-reevaluation.md`](../dev/issue-140-parser-reevaluation.md);
those conditions supersede the trigger list above.
