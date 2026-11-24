/-
`sys.version_info` comparisons, from the Python Language Reference.

* §6.10.1 (Value comparisons): "Sequences compare lexicographically using
  comparison of corresponding elements … If not equal, the sequences are
  ordered the same as their first differing elements … the shorter
  collection is ordered first." Tuples compare equal only with the same
  length.
* Library reference, `sys.version_info`: a 5-tuple
  `(major, minor, micro, releaselevel, serial)`; `version_info[:2]` is
  `(major, minor)`, `version_info[0]` and `.major` are `major`.

Only integer prefixes are compared here: the tuples chokkin reads hold one
or two integers, so a comparison never reaches `releaselevel`. The
interpreter is CPython 3.`minor`.`micro`.
-/

namespace Spec.VersionInfo

/-- Comparison operators of a branch test. -/
inductive Op where
  | lt | le | gt | ge | eq | ne
  deriving DecidableEq, Repr

/-- Python's lexicographic sequence order on integer sequences
    (§6.10.1), shorter-first on a common prefix. -/
def seqCmp : List Nat → List Nat → Ordering
  | [], [] => .eq
  | [], _ :: _ => .lt
  | _ :: _, [] => .gt
  | x :: xs, y :: ys => if x < y then .lt else if y < x then .gt else seqCmp xs ys

def Op.eval : Op → Ordering → Bool
  | .lt, o => o == .lt
  | .le, o => o != .gt
  | .gt, o => o == .gt
  | .ge, o => o != .lt
  | .eq, o => o == .eq
  | .ne, o => o != .eq

/-- The left operand chokkin reads. -/
inductive Lhs where
  /-- `sys.version_info` -/
  | full
  /-- `sys.version_info[:2]` -/
  | prefix2
  /-- `sys.version_info[0]` / `.major`, compared with an integer -/
  | major
  deriving DecidableEq, Repr

/-- The integers of `version_info` that the comparison can reach. The
    `releaselevel` slot stands at index 3; only its presence matters
    (it makes the tuple longer than any 2-tuple). -/
def lhsSeq (micro minor : Nat) : Lhs → List Nat
  | .full => [3, minor, micro, 0, 0]
  | .prefix2 => [3, minor]
  | .major => [3]

/-- The value of `lhs op rhs` on CPython 3.`minor`.`micro`, where `rhs` is
    `(major,)`, `(major, minor')` or, for `Lhs.major`, the integer
    `major` (an int compared with an int is a 1-tuple compared with a
    1-tuple for ordering purposes). -/
def holds (op : Op) (lhs : Lhs) (rhs : List Nat) (minor micro : Nat) : Bool :=
  op.eval (seqCmp (lhsSeq micro minor lhs) rhs)

/-! Facts about the specification itself. -/

theorem seqCmp_refl (l : List Nat) : seqCmp l l = .eq := by
  induction l with
  | nil => rfl
  | cons x xs ih => simp [seqCmp, ih]

/-- `micro` never matters for a tuple of at most two integers. -/
theorem holds_micro_irrelevant (op : Op) (lhs : Lhs) (rhs : List Nat)
    (h : rhs.length ≤ 2) (minor m1 m2 : Nat) :
    holds op lhs rhs minor m1 = holds op lhs rhs minor m2 := by
  match rhs, h with
  | [], _ => cases lhs <;> rfl
  | [a], _ => cases lhs <;> simp [holds, lhsSeq, seqCmp] <;> split <;> simp_all
  | [a, b], _ =>
    cases lhs <;> simp only [holds, lhsSeq, seqCmp] <;> (repeat' split) <;> simp_all

/-- `sys.version_info` is never equal to a tuple of at most two items. -/
theorem full_ne_short (rhs : List Nat) (h : rhs.length ≤ 2) (minor micro : Nat) :
    holds .eq .full rhs minor micro = false := by
  match rhs, h with
  | [], _ => rfl
  | [a], _ => simp [holds, lhsSeq, seqCmp, Op.eval]; split <;> (try split) <;> simp
  | [a, b], _ =>
    simp only [holds, lhsSeq, seqCmp, Op.eval]
    (repeat' split) <;> simp_all

end Spec.VersionInfo
