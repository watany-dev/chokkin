import Chokkin.Funs
import Spec.VersionInfo

/-!
`VersionTest::holds`, as Aeneas translated it from the copy in
`extract/src/version_guard.rs`, equals Python's tuple comparison
(`Spec.VersionInfo`) for every test `VersionTest::read` builds and every
minor and micro.

`read` builds a test only with `<`, `<=`, `>`, `>=`, `==`, `!=`, and a
major-only test (`major_only`) never carries a minor and is never `full`
(`read_major`).
-/

open Aeneas Aeneas.Std Result chokkin_extract

namespace Proofs.Holds

open version_guard

def specOp : CmpOp → Option Spec.VersionInfo.Op
  | .Lt => some .lt | .LtE => some .le | .Gt => some .gt | .GtE => some .ge
  | .Eq => some .eq | .NotEq => some .ne
  | _ => none

def specLhs (t : VersionTest) : Spec.VersionInfo.Lhs :=
  if t.major_only then .major else if t.full then .full else .prefix2

def specRhs (t : VersionTest) : List Nat :=
  t.major.val :: (t.minor.map (·.val)).toList

theorem holds_eq_spec (t : VersionTest) (op : Spec.VersionInfo.Op)
    (hop : specOp t.op = some op)
    (hmajor : t.major_only → t.minor = none ∧ t.full = false)
    (minor : Std.U32) (micro : Nat) :
    t.holds minor =
      ok (Spec.VersionInfo.holds op (specLhs t) (specRhs t) minor.val micro) := by
  rcases t with ⟨top, major, tminor, full, majorOnly⟩
  simp only at hmajor
  unfold VersionTest.holds
  cases top <;> simp [specOp] at hop <;> subst hop <;>
  cases majorOnly <;> cases full <;> cases tminor <;> simp_all [specLhs, specRhs,
    Spec.VersionInfo.holds, Spec.VersionInfo.lhsSeq, Spec.VersionInfo.seqCmp,
    Spec.VersionInfo.Op.eval, lift, core.cmp.Ordering.then_with, core.cmp.Ordering.then,
    core.cmp.Ordering.is_eq, core.cmp.Ordering.is_ne, core.cmp.Ordering.is_lt,
    core.cmp.Ordering.is_le, core.cmp.Ordering.is_gt, core.cmp.Ordering.is_ge,
    VersionTest.holds.closure.Insts.CoreOpsFunctionFnOnceTupleOrdering.call_once,
    compare, compareOfLessAndEq] <;> (repeat' split) <;> simp_all <;> grind

end Proofs.Holds
