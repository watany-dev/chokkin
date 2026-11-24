-- Models of the std functions Aeneas has no model of, filled in by hand
-- from the generated `FunsExternal_Template.lean`. Each follows the Rust
-- documentation of the item named in its `rust_fun` attribute; none reads
-- chokkin's code. Where std can only fail by exhausting memory, the model
-- fails on a length above `Usize.max`.
module
public import Aeneas
public import Chokkin.Types
@[expose] public section
open Aeneas Aeneas.Std Result ControlFlow Error
set_option linter.dupNamespace false
set_option linter.hashCommand false
set_option linter.unusedVariables false
set_option linter.style.whitespace false
set_option linter.style.setOption false
set_option linter.style.longLine false
set_option linter.internalConstructors false
set_option maxHeartbeats 1000000
set_option maxRecDepth 2048
open chokkin_extract

namespace ChokkinModel

/-- Lexicographic order over equal-length lists, the order std's
    `impl Ord for [T; N]` and slices document. -/
def lexCmp {T : Type _} (cmp : T → T → Result Ordering) : List T → List T → Result Ordering
  | [], [] => ok Ordering.eq
  | [], _ :: _ => ok Ordering.lt
  | _ :: _, [] => ok Ordering.gt
  | x :: xs, y :: ys => do
    let o ← cmp x y
    match o with
    | Ordering.eq => lexCmp cmp xs ys
    | o => ok o

/-- `partial_cmp` lifted the same way: the first non-equal element decides,
    `None` from an element is `None` for the whole. -/
def lexPartialCmp {T : Type _} (cmp : T → T → Result (Option Ordering)) :
    List T → List T → Result (Option Ordering)
  | [], [] => ok (some Ordering.eq)
  | [], _ :: _ => ok (some Ordering.lt)
  | _ :: _, [] => ok (some Ordering.gt)
  | x :: xs, y :: ys => do
    let o ← cmp x y
    match o with
    | some Ordering.eq => lexPartialCmp cmp xs ys
    | o => ok o

def insertM {T : Type _} (cmp : T → T → Result Ordering) (x : T) : List T → Result (List T)
  | [] => ok [x]
  | y :: ys => do
    let o ← cmp x y
    match o with
    | Ordering.gt => do let r ← insertM cmp x ys; ok (y :: r)
    | _ => ok (x :: y :: ys)

/-- A sort by `cmp`. `sort_unstable` documents only that the result is
    sorted; for a total order whose equal elements are identical (as for
    tuples of integers) every sort gives this list. -/
def sortM {T : Type _} (cmp : T → T → Result Ordering) : List T → Result (List T)
  | [] => ok []
  | x :: xs => do let r ← sortM cmp xs; insertM cmp x r

def filterMut {T : Type _} {F : Type _} (call : F → T → Result (Bool × F)) : List T → F → Result (List T)
  | [], _ => ok []
  | x :: xs, f => do
    let (keep, f) ← call f x
    let rest ← filterMut call xs f
    ok (if keep then x :: rest else rest)

def sliceOf {T : Type _} (l : List T) : Result (Slice T) :=
  if h : l.length ≤ Usize.max then ok (Slice.from l h) else fail .panic

def vecOf {T : Type _} (l : List T) : Result (alloc.vec.Vec T) :=
  if h : l.length ≤ Usize.max then ok (alloc.vec.Vec.from l h) else fail .panic

end ChokkinModel

open ChokkinModel

@[rust_fun "core::array::{core::cmp::PartialOrd<[@T; @N], [@T; @N]>}::partial_cmp"]
def Array.Insts.CoreCmpPartialOrdArray.partial_cmp
  {T : Type _} {N : Std.Usize} (cmpPartialOrdInst : core.cmp.PartialOrd T T) :
  Array T N → Array T N → Result (Option Ordering) :=
  fun a b => lexPartialCmp cmpPartialOrdInst.partial_cmp a.val b.val

@[rust_fun "core::array::{core::cmp::PartialOrd<[@T; @N], [@T; @N]>}::le"]
def Array.Insts.CoreCmpPartialOrdArray.le
  {T : Type _} {N : Std.Usize} (cmpPartialOrdInst : core.cmp.PartialOrd T T) :
  Array T N → Array T N → Result Bool :=
  fun a b => do
    let o ← lexPartialCmp cmpPartialOrdInst.partial_cmp a.val b.val
    match o with
    | some Ordering.lt => ok true
    | some Ordering.eq => ok true
    | _ => ok false

@[rust_fun "core::array::{core::cmp::PartialOrd<[@T; @N], [@T; @N]>}::lt"]
def Array.Insts.CoreCmpPartialOrdArray.lt
  {T : Type _} {N : Std.Usize} (cmpPartialOrdInst : core.cmp.PartialOrd T T) :
  Array T N → Array T N → Result Bool :=
  fun a b => do
    let o ← lexPartialCmp cmpPartialOrdInst.partial_cmp a.val b.val
    match o with
    | some Ordering.lt => ok true
    | _ => ok false

@[rust_fun "core::array::{core::cmp::Ord<[@T; @N]>}::cmp"]
def Array.Insts.CoreCmpOrd.cmp
  {T : Type _} {N : Std.Usize} (cmpOrdInst : core.cmp.Ord T) :
  Array T N → Array T N → Result Ordering :=
  fun a b => lexCmp cmpOrdInst.cmp a.val b.val

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_eq"]
def core.cmp.Ordering.is_eq : Ordering → Result Bool := fun o => ok (o == Ordering.eq)

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_ne"]
def core.cmp.Ordering.is_ne : Ordering → Result Bool := fun o => ok (o != Ordering.eq)

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_lt"]
def core.cmp.Ordering.is_lt : Ordering → Result Bool := fun o => ok (o == Ordering.lt)

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_gt"]
def core.cmp.Ordering.is_gt : Ordering → Result Bool := fun o => ok (o == Ordering.gt)

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_le"]
def core.cmp.Ordering.is_le : Ordering → Result Bool := fun o => ok (o != Ordering.gt)

@[rust_fun "core::cmp::{core::cmp::Ordering}::is_ge"]
def core.cmp.Ordering.is_ge : Ordering → Result Bool := fun o => ok (o != Ordering.lt)

/-- `self` unless it is `Equal`, then `other`. -/
@[rust_fun "core::cmp::{core::cmp::Ordering}::then"]
def core.cmp.Ordering.then : Ordering → Ordering → Result Ordering :=
  fun o other => ok (if o == Ordering.eq then other else o)

/-- `self` unless it is `Equal`, then the closure's result. -/
@[rust_fun "core::cmp::{core::cmp::Ordering}::then_with"]
def core.cmp.Ordering.then_with
  {F : Type _} (opsfunctionFnOnceFTupleOrderingInst : core.ops.function.FnOnce
  F Unit Ordering) :
  Ordering → F → Result Ordering :=
  fun o f => if o == Ordering.eq then opsfunctionFnOnceFTupleOrderingInst.call_once f () else ok o

/-- The last element, if any, and the function writing it back. -/
@[rust_fun "core::slice::{[@T]}::last_mut"]
def core.slice.Slice.last_mut
  {T : Type _} : Slice T → Result ((Option T) × (Option T → Slice T)) :=
  fun s => ok (s.val.getLast?, fun
    | none => s
    | some x => s.setAtNat (s.val.length - 1) x)

@[rust_fun "core::slice::{[@T]}::sort_unstable"]
def core.slice.Slice.sort_unstable
  {T : Type _} (cmpOrdInst : core.cmp.Ord T) : Slice T → Result (Slice T) :=
  fun s => do
    let l ← sortM cmpOrdInst.cmp s.val
    sliceOf l

@[rust_fun "core::tuple::{core::cmp::Ord<(@U, @T)>}::cmp"]
def Pair.Insts.CoreCmpOrd.cmp
  {U : Type _} {T : Type _} (cmpOrdInst : core.cmp.Ord U) (cmpOrdInst1 :
  core.cmp.Ord T) :
  (U × T) → (U × T) → Result Ordering :=
  fun (a, b) (c, d) => do
    let o ← cmpOrdInst.cmp a c
    match o with
    | Ordering.eq => cmpOrdInst1.cmp b d
    | o => ok o

@[rust_fun "core::tuple::{core::cmp::PartialOrd<(@U, @T), (@U, @T)>}::partial_cmp"]
def Pair.Insts.CoreCmpPartialOrdPair.partial_cmp
  {U : Type _} {T : Type _} (cmpPartialOrdInst : core.cmp.PartialOrd U U)
  (cmpPartialOrdInst1 : core.cmp.PartialOrd T T) :
  (U × T) → (U × T) → Result (Option Ordering) :=
  fun (a, b) (c, d) => do
    let o ← cmpPartialOrdInst.partial_cmp a c
    match o with
    | some Ordering.eq => cmpPartialOrdInst1.partial_cmp b d
    | o => ok o

@[rust_fun "core::tuple::{core::cmp::PartialOrd<(@U, @T), (@U, @T)>}::le"]
def Pair.Insts.CoreCmpPartialOrdPair.le
  {U : Type _} {T : Type _} (cmpPartialOrdInst : core.cmp.PartialOrd U U)
  (cmpPartialOrdInst1 : core.cmp.PartialOrd T T) :
  (U × T) → (U × T) → Result Bool :=
  fun x y => do
    let o ← Pair.Insts.CoreCmpPartialOrdPair.partial_cmp cmpPartialOrdInst cmpPartialOrdInst1 x y
    match o with
    | some Ordering.lt => ok true
    | some Ordering.eq => ok true
    | _ => ok false

@[rust_fun "core::tuple::{core::cmp::PartialOrd<(@U, @T), (@U, @T)>}::lt"]
def Pair.Insts.CoreCmpPartialOrdPair.lt
  {U : Type _} {T : Type _} (cmpPartialOrdInst : core.cmp.PartialOrd U U)
  (cmpPartialOrdInst1 : core.cmp.PartialOrd T T) :
  (U × T) → (U × T) → Result Bool :=
  fun x y => do
    let o ← Pair.Insts.CoreCmpPartialOrdPair.partial_cmp cmpPartialOrdInst cmpPartialOrdInst1 x y
    match o with
    | some Ordering.lt => ok true
    | _ => ok false

@[rust_fun "core::tuple::{core::cmp::PartialEq<(@U, @T), (@U, @T)>}::eq"]
def Pair.Insts.CoreCmpPartialEqPair.eq
  {U : Type _} {T : Type _} (cmpPartialEqInst : core.cmp.PartialEq U U)
  (cmpPartialEqInst1 : core.cmp.PartialEq T T) :
  (U × T) → (U × T) → Result Bool :=
  fun (a, b) (c, d) => do
    let same ← cmpPartialEqInst.eq a c
    if same then cmpPartialEqInst1.eq b d else ok false

/-- Keeps the elements the closure returns `true` for, visiting each once in
    order (std documents both). -/
@[rust_fun "alloc::vec::{alloc::vec::Vec<@T>}::retain"]
def alloc.vec.Vec.retain
  {T : Type _} (A : Type _) {F : Type _}
  (coreopsfunctionFnMutFTupleShared0TBoolInst : core.ops.function.FnMut F T
  Bool) :
  alloc.vec.Vec T → F → Result (alloc.vec.Vec T) :=
  fun v f => do
    let l ← filterMut coreopsfunctionFnMutFTupleShared0TBoolInst.call_mut v.val f
    vecOf l

@[rust_fun
  "alloc::vec::{core::iter::traits::collect::IntoIterator<&'a alloc::vec::Vec<@T>, &'a @T, core::slice::iter::Iter<'a, @T>>}::into_iter"]
def SharedAVec.Insts.CoreIterTraitsCollectIntoIteratorSharedATIter.into_iter
  {T : Type _} (A : Type _) :
  alloc.vec.Vec T → Result (core.slice.iter.Iter T) :=
  fun v => ok ⟨ alloc.vec.Vec.deref v, 0 ⟩

/-- The stand-in for `a.iter().chain(b).copied().collect()`. -/
def python_range.chain_copied_collect
  :
  Slice ((Array Std.U64 3#usize) × (Array Std.U64 3#usize)) → Slice ((Array
    Std.U64 3#usize) × (Array Std.U64 3#usize)) → Result (alloc.vec.Vec
    ((Array Std.U64 3#usize) × (Array Std.U64 3#usize))) :=
  fun a b => vecOf (a.val ++ b.val)
