import Lake
open Lake DSL

-- The Aeneas library pins its own Lean (v4.36.0) and Mathlib, so this
-- project follows it rather than the v4.15.0 of the other oracles.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas.git" @ "fb44cb400eb6bfe7dce97422ebe8fa748b7e4f9b" / "backends" / "lean"

package «chokkin_aeneas» where
  leanOptions := #[⟨`weak.linter.dupNamespace, false⟩]

/-- Aeneas's translation of `extract/` and the std models it needs. -/
@[default_target] lean_lib Chokkin

/-- Specifications written from the external standards. -/
@[default_target] lean_lib Spec

/-- Theorems relating the translation to the specifications. -/
@[default_target] lean_lib Proofs

lean_lib Check where
  roots := #[`Check.Strings, `Check.Intervals]

/-- Exhaustive checks of the translation against the specifications on
    finite domains, run by `lake exe check`. -/
lean_exe check where
  root := `Check.Main
