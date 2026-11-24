/-!
# Lockfile closure: the specification

Written from the lockfile formats, not from chokkin's Rust code:

* `uv.lock` (uv's documented lock format): a `[[package]]` lists
  `dependencies = [{ name, extra?, marker? }]` and
  `[package.optional-dependencies] <extra> = [...]`. A dependency entry with
  `extra = ["x"]` means "this package needs `name[x]`", so the packages of
  `name`'s extra `x` are installed too.
* `poetry.lock`: `[package.dependencies]` maps a name to a constraint, a table
  `{ version, optional?, extras? }` or an array of such tables;
  `[package.extras]` maps an extra to the requirements it turns on. An
  `optional = true` dependency is installed only through an extra.
* `pdm.lock`: a package requested with extras is its own `[[package]]` entry
  with `extras = [...]`, whose `dependencies` are the base package plus what
  the extras add.
* `pylock.toml` (PEP 751): `[[packages]].dependencies` names the entries a
  package depends on. It has no extras table, so extras are not knowable.

All formats reduce to one graph over *nodes* `(package, extra?)`. A node
`(p, none)` installs `p`'s base dependencies, `(p, some e)` installs what
`p`'s extra `e` adds. A dependency on `q[x, y]` reaches `(q, none)`,
`(q, some x)` and `(q, some y)`.

Markers are deliberately not evaluated: chokkin's spec (§10, #1276) treats a
lock edge as evidence that a package is installed on some platform, so the
closure here over-approximates markers in the same way.

Names are PEP 503 normalized before they enter the model (`normalize`).
-/

namespace Lockfile

/-- PEP 503: runs of `-`, `_`, `.` become one `-`, ASCII lowercased. -/
def normalize (s : String) : String :=
  let rec go : List Char → Bool → List Char
    | [], pending => if pending then ['-'] else []
    | c :: cs, pending =>
      if c == '-' || c == '_' || c == '.' then go cs true
      else (if pending then ['-'] else []) ++ c.toLower :: go cs false
  String.mk (go s.toList false)

structure Dep where
  name : String
  extras : List String := []
deriving Repr, BEq, DecidableEq, Inhabited

structure Pkg where
  name : String
  deps : List Dep := []
  /-- extra name ↦ the dependencies it adds -/
  optional : List (String × List Dep) := []
deriving Repr, Inhabited

abbrev Lock := List Pkg

/-- A package with the extra it turns on (`none`: the base package). -/
abbrev Node := String × Option String

/-- Every entry for one name contributes (uv forks, pdm extras variants,
pylock per-platform entries). -/
def depsOf (L : Lock) : Node → List Dep
  | (p, none) => (L.filter (·.name == p)).flatMap (·.deps)
  | (p, some e) =>
    (L.filter (·.name == p)).flatMap fun k => (k.optional.filter (·.1 == e)).flatMap (·.2)

/-- What a dependency on `name[extras]` turns on. -/
def depNodes (d : Dep) : List Node := (d.name, none) :: d.extras.map fun e => (d.name, some e)

def succ (L : Lock) (n : Node) : List Node := (depsOf L n).flatMap depNodes

/-- The specification: the least set containing the seeds and closed under
`succ`. -/
inductive Reach (L : Lock) (seeds : List Node) : Node → Prop
  | seed {n} : n ∈ seeds → Reach L seeds n
  | step {n m} : Reach L seeds n → m ∈ succ L n → Reach L seeds m

/-! ## Executable closure -/

def stepAll (L : Lock) (vs : List Node) : List Node :=
  vs ++ (vs.flatMap (succ L)).filter (fun m => !(vs.contains m))

def iterate (L : Lock) : Nat → List Node → List Node
  | 0, vs => vs
  | k + 1, vs =>
    let next := stepAll L vs
    -- Nothing new: the remaining rounds would return `vs` unchanged.
    if next.length == vs.length then vs else iterate L k next

def isClosed (L : Lock) (vs : List Node) : Bool :=
  vs.all fun n => (succ L n).all fun m => vs.contains m

/-- Every node the lock can name, as a fuel bound: each round that does not
converge adds at least one new node. -/
def nodeBound (L : Lock) (seeds : List Node) : Nat :=
  seeds.length + 1 + (L.map fun p =>
    1 + (p.deps.map fun d => 1 + d.extras.length).foldl (· + ·) 0
      + (p.optional.map fun (_, ds) => 1 + (ds.map fun d => 1 + d.extras.length).foldl (· + ·) 0).foldl (· + ·) 0
    ).foldl (· + ·) 0

def closure (L : Lock) (seeds : List Node) : List Node :=
  iterate L (nodeBound L seeds) seeds

/-! ## What chokkin should report for one undeclared import -/

inductive Verdict
  /-- Declared, or added by a declared `p[e]` (chokkin §10, #516). -/
  | declared
  /-- CHK004, transitive edge (Certain). -/
  | transitiveEdge
  /-- CHK004, listed in the lock but unreachable (Likely). -/
  | lockedOnly
  /-- CHK003. -/
  | missing
deriving Repr, BEq, DecidableEq

def Verdict.code : Verdict → String
  | .declared => "none"
  | .transitiveEdge => "CHK004/certain"
  | .lockedOnly => "CHK004/likely"
  | .missing => "CHK003"

/-- Rank by strength of the lock evidence. -/
def Verdict.rank : Verdict → Nat
  | .missing => 0
  | .lockedOnly => 1
  | .transitiveEdge => 2
  | .declared => 3

def seedsOf (declared : List Dep) : List Node := declared.flatMap depNodes

/-- Packages a declared `p[e]` adds directly. -/
def extraAdded (L : Lock) (declared : List Dep) : List String :=
  declared.flatMap fun p => p.extras.flatMap fun e => (depsOf L (p.name, some e)).map (·.name)

def classify (L : Lock) (declared : List Dep) (extrasKnown : Bool) (d : String) : Verdict :=
  if declared.any (·.name == d) then .declared
  else if extrasKnown && (extraAdded L declared).contains d then .declared
  else if (closure L (seedsOf declared)).any (·.1 == d) then .transitiveEdge
  else if L.any (·.name == d) then .lockedOnly
  else .missing

end Lockfile
