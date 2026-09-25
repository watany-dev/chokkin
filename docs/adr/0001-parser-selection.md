# ADR 0001: Python parser selection

## Status

Accepted (amended 2026-09-22 and 2026-09-25; the 2026-09-25 migration is a go at `=0.0.15`, see "Measurement results")

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

## Amendment 2026-09-25 (issue #294, R-14)

Status of this amendment: **accepted; go for the v0.6 cut-over at
`ruff_* =0.0.15`.** When first written, the pin and the go/no-go waited on the
PoC (#320) and the wheel matrix (#321). Both results are under
[Measurement results](#measurement-results-2026-09-25). The cold parse 10k
benchmark is still open. It does not block the go (see there).

### Why the 2026-09-22 decision no longer holds

The 2026-09-22 amendment kept `rustpython-parser` because no replacement had a
stable API and no speed measurement existed. Neither point has changed, but the
cost of staying has:

- Python 3.12+ syntax does not reach the AST. PEP 695 generics fail as syntax
  errors, and PEP 750 t-strings, PEP 758 (`except A, B:`) and PEP 810
  `lazy import` will never be supported by a crate with no release since
  2024-08. Files using them lose every import edge, so this is a correctness gap
  (`CHK001` / `CHK002` false positives), not only a speed question.
- `deny.toml` already ignores six `unmaintained` advisories
  (RUSTSEC-2025-0075/0080/0081/0090/0098/0100) that come in only through
  `rustpython-parser` → `unic-*`.

"Stable public API" is therefore dropped as a precondition. It is replaced by an
exact pin, a lockstep update procedure, and an adapter boundary that keeps the
parser's AST types inside `src/parser/`.

### Decision

1. **Target: `ruff_python_parser`**, together with the crates it moves in
   lockstep with (`ruff_python_ast`, `ruff_text_size`, and transitively
   `ruff_python_trivia`).
2. **Pinning: crates.io with an exact version** (`version = "=0.0.N"`) on every
   `ruff_*` crate we depend on directly, all at the same `N`. A crates.io source
   keeps `deny.toml` `[sources]` and `maturin sdist` unchanged.
3. **Git dependency only as a fallback**, when syntax we need exists on
   `astral-sh/ruff` `main` but is not yet published. Then the dependency is
   `git = "https://github.com/astral-sh/ruff", rev = "<full 40-char sha>"`
   (never a branch or tag), `deny.toml` gets `allow-git` for that URL only, and
   we go back to crates.io at the next publish that contains the rev.
4. **Adapter boundary.** Ruff AST types appear only under `src/parser/` and in
   the `setup.py` literal evaluator (`src/manifest/literals.rs`,
   `src/manifest/setup_py.rs`). The same 10 files hold the rustpython types
   today. `ParsedModule`, the manifest output types, and everything downstream
   (`graph`, `reachability`, `rules`, `cache`) stay backend-neutral.
   Production ships one backend; a second backend exists only on the PoC branch
   behind a `parser-ruff` feature for head-to-head measurement.
5. **`lazy import` (PEP 810)** creates the same edge as a plain `import` /
   `from … import` (same `ImportKind`, same `ImportContext` rules). The
   side-effect difference at import time is not modelled.

### Lockstep update procedure

- **Cadence:** at most once per chokkin minor release, plus an out-of-band bump
  when a new Python syntax we need, or a security fix, lands upstream. Not every
  upstream publish.
- **One PR bumps every `ruff_*` crate to the same version.** Cargo treats a
  `0.0.x` → `0.0.y` change as semver-incompatible, so Dependabot's existing
  `cargo-minor-and-patch` group would open one PR per crate. The migration PR
  adds a Dependabot group matching `ruff_*` so the bump arrives as one PR.
- **Gate for the bump PR:** `make check`; the parser fixture diff
  (`tests/fixtures/parse`, `tests/fixtures/parser_spike`, `tests/parser_parse.rs`,
  `tests/fuzz_corpus.rs`); `make bench-cmp BASELINE=<previous>` on the #139
  pipeline bench; and the `Release` workflow's wheel matrix, which already runs
  all 7 targets on every pull request.
- **Cache:** the parse cache key includes the chokkin version but not the parser
  version. The migration PR, and any bump that changes `ParsedModule` output,
  bumps the parse cache `unit_version` (`parse-vN` in `src/parser/parse.rs`) so dev builds do not reuse stale entries.

### Migration stages

| Stage | Release | Content | Exit |
| --- | --- | --- | --- |
| 0 | v0.5 | This amendment; PoC (#320, draft PR #349); wheel matrix on the PoC branch (#321) | Done: measurements below recorded, pin set to `=0.0.15` |
| 1 | v0.6 | Swap the backend in `src/parser/` (`parse.rs`, `visit.rs`, `exports.rs`, `dynamic.rs`, `decorators.rs`, `attributes.rs`, `type_checking.rs`, `platform_guard.rs`, plus a byte-offset → line index) and in `src/manifest/literals.rs` / `setup_py.rs`; remove `rustpython-parser` and the six ignores in `deny.toml` and `.cargo/audit.toml`; raise MSRV to 1.96; bump the parse cache `unit_version`; add the Dependabot group | All existing parser / golden tests pass or have a classified diff; 7-target wheels build |
| 2 | v0.6 | Use the new syntax: PEP 695 and `type` aliases in the AST (retire the text-match `syntax_target_hint` for `type`), t-strings visited like f-strings, PEP 758 handlers, `lazy import` edges | Fixtures for each PEP; R-14 closed |

### Measurement results (2026-09-25)

The PoC swaps the backend directly (no `parser-ruff` feature, because wheels
build with default features) on draft PR #349. It is verified in GitHub Actions,
since the agent environment cannot download from `static.crates.io`. Details and
per-target numbers are in
[`docs/dev/issue-294-parser-migration-plan.md`](../dev/issue-294-parser-migration-plan.md).

- **Pin: `=0.0.15`** for `ruff_python_parser`, `ruff_python_ast` and
  `ruff_text_size`. These crates need **Rust 1.96**, so the MSRV rises from 1.93
  to 1.96 with the cut-over.
- **#320, correctness:** every integration test under `tests/` passes unchanged
  on every CI OS, including the line numbers checked against
  `tests/fixtures/parse` and `tests/fixtures/parser_spike`. Only the unit tests
  that build ASTs directly were rewritten. PEP 695 (`class C[T]`,
  `def f[T]`, `type X = …`), PEP 750 t-strings, PEP 758 `except A, B:` and
  PEP 810 `lazy import` / `lazy from` parse without syntax errors. The
  `lazy` imports become edges, and so do attribute references in the new
  syntax (return annotations, class bases, t-string fields).
- **#320, speed:** not measured. CI has no bench job, and the agent environment
  cannot build the crates. This does not block the go: per the plan, new syntax
  support is a correctness fix and is not rejected for speed alone. The
  `make bench-cmp` on the Stage 1 PR is the gate. A median ratio above 1.2 is
  recorded here and handled in the v0.7 performance work.
- **#321, wheels:** all 7 targets and the sdist build, including `stacker` →
  `psm` on both musl targets. Wheels are 0.4–1.4% smaller. The whole Release run
  took 3:51 against 3:23 on main, far from the 60-minute job timeout.
- **Supply chain:** crates.io only, so `deny.toml` `[sources]` is unchanged and
  the six `unic-*` ignores go away. `cargo deny` needs one crate-scoped license
  exception, for `ar_archive_writer` (`Apache-2.0 WITH LLVM-exception`, a build
  dependency of `psm`). `cargo audit` passes with an empty ignore list.

The fallback in the original text (stay on `rustpython-parser` if a wheel
target cannot build) is not needed.
