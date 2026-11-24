/-!
PEP 440 versions and specifiers, restricted to what PEP 508 markers on
`python_version` / `python_full_version` and `requires-python` need:
release segments plus an optional pre-release. Written from PEP 440
("Version scheme", "Version specifiers"), not from chokkin's Rust.

Out of the model (parse to `none`, so the oracle skips the case instead of
guessing): epochs, post / dev releases, local versions.
-/
namespace MarkerNaming.Pep440

/-- Pre-release phase: `a` < `b` < `rc` (PEP 440 "Pre-releases"). -/
inductive Phase | a | b | rc
  deriving DecidableEq, Repr, Inhabited

def Phase.rank : Phase → Nat
  | .a => 0 | .b => 1 | .rc => 2

structure Version where
  release : List Nat
  pre : Option (Phase × Nat) := none
  deriving DecidableEq, Repr, Inhabited

/-- Release comparison with implicit zero padding ("Release segment":
`1.0` = `1.0.0`). -/
def cmpRelease : List Nat → List Nat → Ordering
  | [], [] => .eq
  | [], b :: bs => if b = 0 then cmpRelease [] bs else .lt
  | a :: as, [] => if a = 0 then cmpRelease as [] else .gt
  | a :: as, b :: bs =>
    if a < b then .lt else if b < a then .gt else cmpRelease as bs

def cmpPre : Option (Phase × Nat) → Option (Phase × Nat) → Ordering
  | none, none => .eq
  | some _, none => .lt          -- a pre-release sorts before its final
  | none, some _ => .gt
  | some (p, n), some (q, m) =>
    if p.rank < q.rank then .lt else if q.rank < p.rank then .gt
    else compare n m

def cmp (v w : Version) : Ordering :=
  match cmpRelease v.release w.release with
  | .eq => cmpPre v.pre w.pre
  | o => o

/-! ## Parsing -/

def digitsToNat (cs : List Char) : Option Nat :=
  if cs.isEmpty || !cs.all Char.isDigit then none
  else some (cs.foldl (fun n c => n * 10 + (c.toNat - '0'.toNat)) 0)

def parseRelease (s : String) : Option (List Nat) :=
  (s.splitOn ".").mapM (fun part => digitsToNat part.toList)

/-- Pre-release spellings PEP 440 normalizes: `alpha`/`a`, `beta`/`b`,
`c`/`rc`/`pre`/`preview`, with an optional `.`/`-`/`_` before and after and
an implicit `0`. -/
def phaseSpellings : List (String × Phase) :=
  [("alpha", .a), ("beta", .b), ("preview", .rc), ("pre", .rc), ("rc", .rc),
   ("a", .a), ("b", .b), ("c", .rc)]

def isSep (c : Char) : Bool := c == '.' || c == '-' || c == '_'

def parsePre (s : String) : Option (Phase × Nat) :=
  let s := (s.dropWhile isSep).toLower
  phaseSpellings.findSome? fun (spelling, phase) =>
    if s.startsWith spelling then
      let rest := (s.drop spelling.length).dropWhile isSep
      if rest.isEmpty then some (phase, 0)
      else (digitsToNat rest.toList).map (phase, ·)
    else none

/-- A version in the model: `[v]N(.N)*[pre]`. -/
def parseVersion (s : String) : Option Version :=
  let s := s.trim
  let s := if s.startsWith "v" || s.startsWith "V" then s.drop 1 else s
  let relText := s.takeWhile (fun c => c.isDigit || c == '.')
  let rest := s.drop relText.length
  -- A trailing `.` belongs to the pre-release separator (`1.0.rc1`).
  let (relText, rest) :=
    if relText.endsWith "." && !rest.isEmpty then (relText.dropRight 1, "." ++ rest)
    else (relText, rest)
  match parseRelease relText with
  | none => none
  | some release =>
    if rest.isEmpty then some { release }
    else (parsePre rest).map fun pre => { release, pre := some pre }

/-! ## Specifiers -/

inductive Op | eq | ne | le | ge | lt | gt | compat | arbitrary
  deriving DecidableEq, Repr, Inhabited

structure Specifier where
  op : Op
  version : Version
  wildcard : Bool := false
  /-- The version as written, for `===`. -/
  raw : String
  deriving Repr, Inhabited

def opSpellings : List (String × Op) :=
  [("===", .arbitrary), ("==", .eq), ("!=", .ne), ("~=", .compat),
   ("<=", .le), (">=", .ge), ("<", .lt), (">", .gt)]

inductive SpecParse
  | ok (s : Specifier)
  /-- Not a PEP 440 specifier: `packaging` raises `InvalidSpecifier`. -/
  | invalid
  /-- A valid specifier outside the model (post, dev, local, epoch). -/
  | outside
  deriving Inhabited

/-- Whether `s` is a version PEP 440 accepts but the model does not hold. -/
def isOutsideVersion (s : String) : Bool :=
  let s := s.toLower
  s.any (· == '!') || s.any (· == '+') ||
    ["post", "dev", "rev", "-1", "-2", "-3", "r1", "r2"].any (fun k => (s.splitOn k).length > 1)

def parseSpecifier (text : String) : SpecParse :=
  let text := text.trim
  match opSpellings.find? (fun (o, _) => text.startsWith o) with
  | none => .invalid
  | some (spelling, op) =>
    let v := (text.drop spelling.length).trim
    if v.isEmpty || v.any Char.isWhitespace then .invalid
    else if op == .arbitrary then
      .ok { op, version := (parseVersion v).getD default, raw := v }
    else
      let wildcard := v.endsWith ".*"
      let body := if wildcard then v.dropRight 2 else v
      if wildcard && !(op == .eq || op == .ne) then .invalid
      else match parseVersion body with
        | none => if isOutsideVersion body then .outside else .invalid
        | some version =>
          if wildcard && version.pre.isSome then .invalid
          else if op == .compat && version.release.length < 2 then .invalid
          else .ok { op, version, wildcard, raw := v }

def pad (n : Nat) (r : List Nat) : List Nat := r ++ List.replicate (n - r.length) 0

/-- `==V.*`: the candidate's release, zero padded, starts with `V`'s release;
its pre-release does not matter. -/
def prefixMatch (v : Version) (prefix_ : List Nat) : Bool :=
  (pad prefix_.length v.release).take prefix_.length == prefix_

/-- `Specifier.contains(v, prereleases=True)`, which is how `packaging`
evaluates a marker. -/
def contains (s : Specifier) (v : Version) (candidate : String) : Bool :=
  match s.op with
  | .eq => if s.wildcard then prefixMatch v s.version.release else cmp v s.version == .eq
  | .ne => if s.wildcard then !prefixMatch v s.version.release else cmp v s.version != .eq
  | .le => cmp v s.version != .gt
  | .ge => cmp v s.version != .lt
  -- `<V` must not allow a pre-release of `V` unless `V` is one itself.
  | .lt => cmp v s.version == .lt &&
      !(v.pre.isSome && s.version.pre.isNone && cmpRelease v.release s.version.release == .eq)
  | .gt => cmp v s.version == .gt
  | .compat =>
      cmp v s.version != .lt && prefixMatch v s.version.release.dropLast
  | .arbitrary => candidate.toLower == s.raw.toLower

/-- A comma-separated specifier set (`requires-python`); `none` when an
item is invalid or outside the model. Empty items are dropped, as
`SpecifierSet` does. -/
def parseSpecifierSet (text : String) : Option (List Specifier) :=
  let items := (text.splitOn ",").map String.trim |>.filter (!·.isEmpty)
  if items.isEmpty then none
  else items.mapM fun item => match parseSpecifier item with
    | .ok s => some s
    | _ => none

end MarkerNaming.Pep440
