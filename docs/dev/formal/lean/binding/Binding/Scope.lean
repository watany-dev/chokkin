/-!
Name resolution of the Python Language Reference §4.2.2 ("Resolution of
names"), written on an explicit scope chain so that its properties can be
proved by induction (`Binding/Proofs.lean`).

A chain lists the scopes from the one executing the load outward to the
module scope. `locals` holds the names bound in the block (§4.2.1) minus the
ones declared `global` / `nonlocal` there.
-/

namespace Binding

inductive Kind where
  | module
  | func
  | cls
  | comp
  /-- PEP 695 annotation scope holding type parameters -/
  | tparams
  deriving DecidableEq, Repr, Inhabited

structure ScopeInfo where
  kind : Kind
  parent : Option Nat
  module : String
  locals : List String
  globals : List String
  nonlocals : List String
  deriving Repr, Inhabited

abbrev Chain := List (Nat × ScopeInfo)

/-- The module scope at the end of the chain. -/
def moduleOf : Chain → Option (Nat × ScopeInfo)
  | [] => none
  | (i, s) :: rest => if s.kind = .module then some (i, s) else moduleOf rest

/-- `nonlocal n` binds to the nearest enclosing function scope that binds `n`
(§7.13); class scopes are not candidates. -/
def enclosingFunc : Chain → String → Option (Nat × ScopeInfo)
  | [], _ => none
  | (i, s) :: rest, n =>
    if s.kind = .module then none
    else if s.kind = .func ∧ n ∈ s.locals then some (i, s)
    else enclosingFunc rest n

/-- A free variable: the nearest enclosing function-like scope that binds it,
skipping class scopes ("The scope of names defined in a class block is limited
to the class block; it does not extend to the code blocks of methods"), else
the module (whose miss falls back to builtins). -/
def resolveFree : Chain → String → Option (Nat × ScopeInfo)
  | [], _ => none
  | (i, s) :: rest, n =>
    match s.kind with
    | .module => some (i, s)
    | .cls => resolveFree rest n
    | _ =>
      if n ∈ s.globals then moduleOf rest
      else if n ∈ s.locals then some (i, s)
      else resolveFree rest n

/-- Scopes a load of `n` may read, the first scope of the chain executing it.
A class block that binds `n` reads its own namespace, then the globals. -/
def resolveLoad : Chain → String → List (Nat × ScopeInfo)
  | [], _ => []
  | c@((i, s) :: rest), n =>
    if s.kind = .module then [(i, s)]
    else if n ∈ s.globals then (moduleOf c).toList
    else if n ∈ s.nonlocals then (enclosingFunc rest n).toList
    else if n ∈ s.locals then
      if s.kind = .cls then (i, s) :: (moduleOf c).toList else [(i, s)]
    else (resolveFree rest n).toList

/-- The scope a store to `n` binds in. -/
def resolveStore : Chain → String → Option (Nat × ScopeInfo)
  | [], _ => none
  | c@((i, s) :: rest), n =>
    if n ∈ s.globals then moduleOf c
    else if n ∈ s.nonlocals then enclosingFunc rest n
    else some (i, s)

/-- PEP 572: an assignment expression inside a comprehension binds in the
nearest enclosing scope that is not a comprehension. -/
def walrusTarget : Chain → String → Option (Nat × ScopeInfo)
  | [], _ => none
  | c@((_, s) :: rest), n =>
    if s.kind = .comp then walrusTarget rest n else resolveStore c n

/-- Names `from m import *` binds (§7.11): `__all__` when it is a literal,
otherwise every name in the namespace not starting with an underscore. -/
def starNames (all : Option (List String)) (names : List String) : List String :=
  match all with
  | some l => l
  | none => names.filter (fun n => !n.startsWith "_")

/-- Package that a relative import of `level` dots is resolved against
(§5.7, PEP 328): the module's own package (`__package__`), then one parent per
extra dot. `parts` is the module's dotted name split on dots. `none` when the
dots go above the top-level package. -/
def relBase (parts : List String) (isPkg : Bool) (level : Nat) : Option (List String) :=
  let pkg := if isPkg then parts else parts.dropLast
  if level - 1 < pkg.length then some (pkg.take (pkg.length - (level - 1))) else none

end Binding
