# Python name-binding oracle (Lean 4)

A differential oracle for CHK006 / CHK007. The spec is written from the
Python Language Reference (§4.2 naming and binding, §5 the import system,
§7.11 `import`), not ported from `src/rules/symbols/`, and is run against the
release binary as a black box.

- `Binding/IR.lean`: the Python syntax subset the spec reads (JSON written by
  `harness/py2ir.py`, which only calls `ast.parse`; nothing is imported or run).
- `Binding/Scope.lean`: §4.2.2 name resolution on an explicit scope chain.
- `Binding/Spec.lean`: a whole-project may-analysis of the values each name
  and attribute can hold, to a fixpoint. A reference `(importer, m, n)` is a
  load written in `importer` that evaluates to the binding `n` of module `m`.
  Anything not modelled has no value, so an unmodelled construct can hide a
  chokkin bug but cannot invent one.
- `Binding/Proofs.lean`: properties proved for all chains and names (class
  scopes are invisible to nested blocks, an assignment expression never binds
  in a comprehension, `import *` and `__all__`, relative-import bases are
  prefixes of the module path).
- `harness/gen.py`: generated projects (module source x binding carrier x
  read, placements, packages and re-exports, negative cases).
- `harness/diff.py`: the differential run; `harness/oss.py`: the same check on
  a real checkout.

## Run

```sh
# Lean 4.15.0 (lean-toolchain); no Mathlib
lake build                      # builds the spec, the proofs and .lake/build/bin/oracle
cargo build --release           # from the repository root
python3 -I harness/diff.py ../../../../../target/release/chokkin .lake/build/bin/oracle /tmp/binding-work
python3 -I harness/oss.py ../../../../../target/release/chokkin .lake/build/bin/oracle /path/to/checkout src
```

`diff.py` prints one JSON line per mismatch (`FP`: the spec shows a read of a
symbol chokkin reports as unused; `FN`: a negative case chokkin does not
report) and a tally on stderr. Findings are recorded in `ledger.ja.md`.
