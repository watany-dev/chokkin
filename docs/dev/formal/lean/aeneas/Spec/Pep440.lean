/-
PEP 440 version specifiers on final releases, from the PEP text
(https://peps.python.org/pep-0440/):

* "Final releases" / "Version scheme": a release is `N(.N)*`.
* "Comparison and ordering of release segments": compared segment by
  segment; "the shorter one is padded with zeros as needed".
* "Version matching" (`==V`): equal after zero padding; with `.*`, "the
  candidate version … is zero padded … and compared only up to the length
  of the prefix".
* "Inclusive ordered comparison" (`<=`, `>=`) and "Exclusive ordered
  comparison" (`<`, `>`), restricted to final releases: plain order.
* "Compatible release" (`~=V.N`, two segments or more): `>= V.N, == V.*`.

PEP 508 markers: `python_full_version` is the interpreter's
`platform.python_version()`; `python_version` is
`'.'.join(platform.python_version_tuple()[:2])`, compared as a version
(packaging builds `Specifier(op + value)` and asks whether it contains the
environment's value).
-/

namespace Spec.Pep440

/-- A release segment list. -/
abbrev Release := List Nat

def segAt (r : Release) (i : Nat) : Nat := r.getD i 0

/-- Zero-padded comparison (both padded to the longer length). -/
def cmp (a b : Release) : Ordering :=
  go (max a.length b.length) 0
where
  go : Nat → Nat → Ordering
    | 0, _ => .eq
    | n + 1, i =>
      if segAt a i < segAt b i then .lt
      else if segAt b i < segAt a i then .gt
      else go n (i + 1)

/-- `==prefix.*` on a final release. -/
def prefixMatch (pre v : Release) : Bool :=
  (List.range pre.length).all fun i => segAt v i == segAt pre i

inductive Spec where
  | eq (v : Release) | ne (v : Release)
  | eqStar (v : Release) | neStar (v : Release)
  | le (v : Release) | ge (v : Release) | lt (v : Release) | gt (v : Release)
  | compat (v : Release)

def contains : Spec → Release → Bool
  | .eq v, c => cmp c v == .eq
  | .ne v, c => cmp c v != .eq
  | .eqStar p, c => prefixMatch p c
  | .neStar p, c => !prefixMatch p c
  | .le v, c => cmp c v != .gt
  | .ge v, c => cmp c v != .lt
  | .lt v, c => cmp c v == .lt
  | .gt v, c => cmp c v == .gt
  | .compat v, c => cmp c v != .lt && prefixMatch v.dropLast c

/-- A CPython interpreter's version as the `X.Y.Z` triple. -/
abbrev Interp := Nat × Nat × Nat

def fullVersion : Interp → Release := fun (x, y, z) => [x, y, z]
def pythonVersion : Interp → Release := fun (x, y, _) => [x, y]

/-! Facts about the specification. -/

theorem segAt_nil (i : Nat) : segAt [] i = 0 := by simp [segAt]

/-- On 3-segment releases, `==X.Y.Z.*` and `==X.Y.Z` agree. -/
theorem prefixMatch_full_iff (a b c x y z : Nat) :
    prefixMatch [a, b, c] [x, y, z] = (cmp [x, y, z] [a, b, c] == .eq) := by
  simp only [prefixMatch, cmp, segAt, List.length, List.range, List.range.loop,
    List.all, List.getD, List.getElem?_cons_zero, List.getElem?_cons_succ, Option.getD_some]
  simp only [show max (0 + 1 + 1 + 1) (0 + 1 + 1 + 1) = 3 from rfl, cmp.go, segAt,
    List.getD, List.getElem?_cons_zero, List.getElem?_cons_succ, Option.getD_some]
  (repeat' split) <;> grind

/-- Padding: a trailing zero segment never changes a comparison. -/
theorem cmp_pad3 (x y z a b : Nat) : cmp [x, y, z] [a, b] = cmp [x, y, z] [a, b, 0] := by
  simp [cmp, cmp.go, segAt]

end Spec.Pep440
