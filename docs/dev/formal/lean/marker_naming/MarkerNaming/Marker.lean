import MarkerNaming.Pep440

/-!
PEP 508 environment markers, evaluated the way the spec and `packaging`
define them: a comparison builds `Specifier(op + rhs)` and asks whether it
contains `lhs`; when that is no PEP 440 specifier, it falls back to Python's
string operators (`in`, `not in`, `<`, `==`, ...), and `~=` without a
specifier is an error (`UndefinedComparison`).

"Always holds" is a property over interpreters: every interpreter
`requires-python` admits (pip checks it against `X.Y.Z`, the release
only) satisfies the marker, on every OS / platform in the domain.
-/
namespace MarkerNaming.Marker
open Pep440

inductive Var | pythonVersion | pythonFullVersion | osName | sysPlatform
  deriving DecidableEq, Repr, Inhabited

inductive Operand | var (v : Var) | lit (s : String)
  deriving Repr, Inhabited

inductive Marker
  | cmp (lhs : Operand) (op : String) (rhs : Operand)
  | and (a b : Marker)
  | or (a b : Marker)
  deriving Repr, Inhabited

/-! ## Tokens and parsing (PEP 508 grammar, `marker` production) -/

inductive Tok | lp | rp | str (s : String) | op (s : String) | word (s : String)
  deriving Repr, BEq, Inhabited

def isOpChar (c : Char) : Bool := "<>=!~".contains c
def isWordChar (c : Char) : Bool := c.isAlphanum || c == '_' || c == '.'

partial def tokenize (cs : List Char) (acc : Array Tok := #[]) : Option (Array Tok) :=
  match cs with
  | [] => some acc
  | c :: rest =>
    if c == ' ' || c == '\t' then tokenize rest acc
    else if c == '(' then tokenize rest (acc.push .lp)
    else if c == ')' then tokenize rest (acc.push .rp)
    else if c == '\'' || c == '"' then
      let body := rest.takeWhile (· != c)
      let after := rest.drop body.length
      match after with
      | [] => none
      | _ :: after => tokenize after (acc.push (.str (String.mk body)))
    else if isOpChar c then
      let o := cs.takeWhile isOpChar
      let s := String.mk o
      if ["<", "<=", "==", "!=", ">=", ">", "~=", "==="].contains s then
        tokenize (cs.drop o.length) (acc.push (.op s))
      else none
    else if isWordChar c then
      let w := cs.takeWhile isWordChar
      tokenize (cs.drop w.length) (acc.push (.word (String.mk w)))
    else none

def varOf : String → Option Var
  | "python_version" => some .pythonVersion
  | "python_full_version" => some .pythonFullVersion
  | "os_name" | "os.name" => some .osName
  | "sys_platform" | "sys.platform" => some .sysPlatform
  | _ => none

abbrev P := StateT Nat (ReaderT (Array Tok) Option)

def peek : P (Option Tok) := do return (← read)[(← get)]?
def advance : P Unit := modify (· + 1)

def operand : P Operand := do
  match ← peek with
  | some (.str s) => advance; return .lit s
  | some (.word w) =>
    match varOf w with
    | some v => advance; return .var v
    | none => failure
  | _ => failure

def cmpOp : P String := do
  match ← peek with
  | some (.op o) => advance; return o
  | some (.word "in") => advance; return "in"
  | some (.word "not") =>
    advance
    match ← peek with
    | some (.word "in") => advance; return "not in"
    | _ => failure
  | _ => failure

mutual
  partial def orExpr : P Marker := do
    let mut m ← andExpr
    while (← peek) == some (.word "or") do
      advance
      m := .or m (← andExpr)
    return m
  partial def andExpr : P Marker := do
    let mut m ← atom
    while (← peek) == some (.word "and") do
      advance
      m := .and m (← atom)
    return m
  partial def atom : P Marker := do
    if (← peek) == some .lp then
      advance
      let m ← orExpr
      if (← peek) == some .rp then advance; return m
      failure
    let l ← operand
    let o ← cmpOp
    let r ← operand
    return .cmp l o r
end

def parse (s : String) : Option Marker := do
  let toks ← tokenize s.toList
  let (m, pos) ← (orExpr.run 0).run toks
  if pos == toks.size then some m else none

/-! ## Evaluation -/

structure Env where
  pythonVersion : String
  pythonFullVersion : String
  osName : String
  sysPlatform : String

def Env.get (e : Env) : Var → String
  | .pythonVersion => e.pythonVersion
  | .pythonFullVersion => e.pythonFullVersion
  | .osName => e.osName
  | .sysPlatform => e.sysPlatform

/-- Evaluation result: `none` is an error or a comparison outside the model;
the oracle then makes no claim about the marker. -/
def evalOp (lhs op rhs : String) : Option Bool :=
  match parseSpecifier (op ++ rhs) with
  | .ok spec =>
    match parseVersion lhs with
    | some v => some (contains spec v lhs)
    | none => if spec.op == .arbitrary then some (contains spec default lhs) else none
  | .outside => none
  | .invalid =>
    match op with
    | "==" => some (lhs == rhs)
    | "!=" => some (lhs != rhs)
    | "<" => some (decide (lhs < rhs))
    | "<=" => some (decide (lhs < rhs) || lhs == rhs)
    | ">" => some (decide (rhs < lhs))
    | ">=" => some (decide (rhs < lhs) || lhs == rhs)
    | _ => none

-- `in` is `lhs in rhs`: substring of the right operand.
def evalIn (lhs rhs : String) : Bool := (rhs.splitOn lhs).length > 1 || lhs.isEmpty

def evalAtom (e : Env) (l : Operand) (op : String) (r : Operand) : Option Bool :=
  let val : Operand → String
    | .var v => e.get v
    | .lit s => s
  let lv := val l
  let rv := val r
  if op == "in" then some (evalIn lv rv)
  else if op == "not in" then some !(evalIn lv rv)
  else evalOp lv op rv

def eval (e : Env) : Marker → Option Bool
  | .cmp l o r => evalAtom e l o r
  | .and a b => do let x ← eval e a; let y ← eval e b; return x && y
  | .or a b => do let x ← eval e a; let y ← eval e b; return x || y

/-! ## Interpreters and "always holds" -/

def interpreters : List (List Nat) := Id.run do
  let mut out := []
  for x in [2, 3, 4] do
    for y in List.range 17 do
      for z in List.range 7 do
        out := [x, y, z] :: out
  return out.reverse

def osNames : List String := ["nt", "posix", "java"]
def platforms : List String := ["linux", "win32", "darwin", "cygwin"]

def envOf (r : List Nat) (os plat : String) : Env :=
  let s := fun (l : List Nat) => ".".intercalate (l.map toString)
  { pythonVersion := s (r.take 2), pythonFullVersion := s r, osName := os, sysPlatform := plat }

/-- `requires-python` admits the interpreter `X.Y.Z` (pip:
`SpecifierSet.contains("X.Y.Z")`). -/
def admits (rp : List Specifier) (r : List Nat) : Bool :=
  let v : Version := { release := r }
  rp.all fun s => contains s v (".".intercalate (r.map toString))

inductive Verdict | always | notAlways | unknown
  deriving DecidableEq, Repr

def verdict (rp : List Specifier) (m : Marker) : Verdict := Id.run do
  let mut sawFalse := false
  for r in interpreters do
    if admits rp r then
      for os in osNames do
        for plat in platforms do
          match eval (envOf r os plat) m with
          | none => return .unknown
          | some false => sawFalse := true
          | some true => pure ()
  return if sawFalse then .notAlways else .always

end MarkerNaming.Marker
