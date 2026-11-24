import Chokkin.Funs

/-!
The interval algebra of `python_range.rs` as Aeneas translated it, run
against its set semantics on every interval list over a small version
lattice.

Specification (spec §4 #919, read off the type's own contract): a
`PythonRange` denotes the versions `v` with `low ≤ v < high` for one of
its intervals, versions ordered lexicographically; `from_intervals`
returns the same set as sorted, disjoint, non-empty intervals with a gap
between neighbours; `complement`, `union`, `intersect` and
`is_subset_of` are the set operations below `TOP` (the bound above every
version, which is itself no version).
-/

open Aeneas Aeneas.Std Result chokkin_extract

namespace Check.Intervals

abbrev V := List Nat

def lexLt : V → V → Bool
  | [], [] => false
  | [], _ => true
  | _, [] => false
  | x :: xs, y :: ys => x < y || (x == y && lexLt xs ys)

def lexLe (a b : V) : Bool := !lexLt b a

def topN : Nat := 2 ^ 64 - 1
def top : V := [topN, topN, topN]

def u64 (n : Nat) : Std.U64 :=
  if h : n < 2 ^ 64 then UScalar.ofNatCore n (by simpa using h) else UScalar.ofNatCore 0 (by decide)

def arr (v : V) : Array Std.U64 3#usize :=
  if h : (v.map u64).length = (3#usize).val then Array.from (v.map u64) h
  else Array.repeat 3#usize (u64 0)

def ofArr (a : Array Std.U64 3#usize) : V := a.val.map (·.val)

def toVec (l : List (V × V)) : alloc.vec.Vec ((Array Std.U64 3#usize) × (Array Std.U64 3#usize)) :=
  if h : (l.map fun (a, b) => (arr a, arr b)).length ≤ Usize.max then
    alloc.vec.Vec.from (l.map fun (a, b) => (arr a, arr b)) h
  else alloc.vec.Vec.new _

def ofVec (r : python_range.PythonRange) : List (V × V) :=
  r.val.map fun (a, b) => (ofArr a, ofArr b)

/-- The value of a terminating, non-panicking run. -/
def run? {α : Type} (r : Result α) : Option α :=
  match Result.match r with
  | .ok a => some a
  | _ => none

def mem (r : List (V × V)) (v : V) : Bool :=
  r.any fun (lo, hi) => lexLe lo v && lexLt v hi

/-- Sorted, non-empty, and a gap before each next interval. -/
def normalized : List (V × V) → Bool
  | [] => true
  | [(lo, hi)] => lexLt lo hi
  | (lo, hi) :: (lo', hi') :: rest => lexLt lo hi && lexLt hi lo' && normalized ((lo', hi') :: rest)

def grid (segs : List Nat) : List V := Id.run do
  let mut out := []
  for a in segs do
    for b in segs do
      for c in segs do
        out := out ++ [[a, b, c]]
  return out

/-- The versions membership is compared on: segments in `{0, …, 3}`, one
    step past every bound. -/
def points : List V := grid [0, 1, 2, 3]

/-- Interval bounds: segments in `{0, 1, 2}`, and `TOP`. -/
def bounds : List V := grid [0, 1, 2] ++ [top]

def intervals : List (V × V) := Id.run do
  let mut out := []
  for lo in bounds do
    for hi in bounds do
      out := out ++ [(lo, hi)]
  return out

def failures : IO (List String) := do
  let mut errs : List String := []
  let mut checked := 0
  -- from_intervals: every list of at most two intervals, empty ones included.
  let lists := [[]] ++ intervals.map ([·]) ++
    (intervals.filter (fun (a, _) => a.head? == some 0 || a == top)).flatMap fun i =>
      (intervals.filter fun (_, b) => b.getLast? == some 2 || b == top).map fun j => [i, j]
  for l in lists do
    checked := checked + 1
    match run? (python_range.PythonRange.from_intervals (toVec l)) with
    | some r =>
      let r := ofVec r
      if !normalized r then errs := errs ++ [s!"from_intervals {l}: not normalized {r}"]
      for v in points do
        if mem r v != mem l v then errs := errs ++ [s!"from_intervals {l}: {v}"]
    | _ => errs := errs ++ [s!"from_intervals {l}: failed"]
  -- Set operations on normalized ranges of one interval each.
  let ranges := ([[]] : List (List (V × V))) ++ (intervals.filter fun (a, b) => lexLt a b).map ([·])
  for a in ranges do
    checked := checked + 1
    match run? (python_range.PythonRange.complement (toVec a)) with
    | some c =>
      for v in points do
        if mem (ofVec c) v == mem a v then errs := errs ++ [s!"complement {a}: {v}"]
    | _ => errs := errs ++ [s!"complement {a}: failed"]
  let some' := ranges.filter fun r => r.all fun (lo, _) => lo.head? != some 1
  for a in some' do
    for b in some' do
      checked := checked + 1
      match run? (python_range.PythonRange.union (toVec a) (toVec b)),
            run? (python_range.PythonRange.intersect (toVec a) (toVec b)),
            run? (python_range.PythonRange.is_subset_of (toVec a) (toVec b)) with
      | some u, some i, some s =>
        for v in points do
          if mem (ofVec u) v != (mem a v || mem b v) then errs := errs ++ [s!"union {a} {b}: {v}"]
          if mem (ofVec i) v != (mem a v && mem b v) then errs := errs ++ [s!"intersect {a} {b}: {v}"]
        let sub := points.all fun v => !mem a v || mem b v
        if s != sub then errs := errs ++ [s!"is_subset_of {a} {b}"]
      | _, _, _ => errs := errs ++ [s!"set ops {a} {b}: failed"]
  IO.println s!"intervals: {checked} cases checked"
  return errs

end Check.Intervals
