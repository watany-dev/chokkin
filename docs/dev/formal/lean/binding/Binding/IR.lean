import Lean.Data.Json

/-!
The Python syntax the binding spec reads: a subset of `ast` written out by
`harness/py2ir.py`. Constructs outside the subset arrive as `other` with their
sub-expressions, so every load inside them is still evaluated.
-/

namespace Binding

inductive Expr where
  | name (id : String)
  | attr (e : Expr) (a : String)
  | call (f : Expr) (args : List Expr) (kws : List (Option String × Expr))
  | str (s : String)
  | int (n : Int)
  | tuple (xs : List Expr)
  | walrus (id : String) (v : Expr)
  /-- parameters are (name, default, annotation); `*a` / `**k` carry the stars -/
  | lambda (ps : List (String × Option Expr × Option Expr)) (body : Expr)
  /-- generators are (target, iter, ifs) -/
  | comp (gens : List (Expr × Expr × List Expr)) (elts : List Expr)
  | starred (e : Expr)
  | subscript (e i : Expr)
  | ifexp (t a b : Expr)
  | or (xs : List Expr)
  | other (xs : List Expr)
  deriving Repr, Inhabited

abbrev Param := String × Option Expr × Option Expr

inductive Pat where
  | capture (n : Option String) (sub : Option Pat)
  | value (e : Expr)
  | seq (ps : List Pat)
  | star (n : Option String)
  | or (ps : List Pat)
  | cls (e : Expr) (ps : List Pat)
  | mapping (keys : List Expr) (ps : List Pat) (rest : Option String)
  | other
  deriving Repr, Inhabited

inductive Stmt where
  | import (names : List (String × Option String))
  | fromImport (m : Option String) (level : Nat) (names : List (String × Option String))
  | assign (targets : List Expr) (v : Expr)
  | aug (t v : Expr)
  | ann (t a : Expr) (v : Option Expr)
  | defn (name : String) (ps : List Param) (decos : List Expr) (ret : Option Expr)
      (body : List Stmt) (tparams : List String)
  | cls (name : String) (bases : List Expr) (decos : List Expr) (body : List Stmt)
      (tparams : List String)
  | ret (v : Option Expr)
  | expr (e : Expr)
  | ifS (t : Expr) (b o : List Stmt)
  | whileS (t : Expr) (b o : List Stmt)
  | forS (t i : Expr) (b o : List Stmt)
  | withS (items : List (Expr × Option Expr)) (b : List Stmt)
  | tryS (b : List Stmt) (hs : List (Option Expr × Option String × List Stmt))
      (o f : List Stmt)
  | global (ns : List String)
  | nonlocal (ns : List String)
  | del (ts : List Expr)
  | matchS (subj : Expr) (cases : List (Pat × Option Expr × List Stmt))
  | typeAlias (name : String) (tparams : List String) (v : Expr)
  | other (es : List Expr)
  deriving Repr, Inhabited

structure Module where
  name : String
  isPkg : Bool
  file : String
  body : List Stmt
  deriving Repr, Inhabited

/-! ## JSON decoding -/

open Lean (Json)

private def arr (j : Json) : Except String (Array Json) := j.getArr?
private def str (j : Json) : Except String String := j.getStr?
private def optStr (j : Json) : Except String (Option String) :=
  if j.isNull then pure none else some <$> j.getStr?

private def at! (a : Array Json) (i : Nat) : Json := a.getD i Json.null

partial def toExpr (j : Json) : Except String Expr := do
  let a ← arr j
  let tag ← str (at! a 0)
  let exprs (k : Nat) : Except String (List Expr) := do
    (← arr (at! a k)).toList.mapM toExpr
  match tag with
  | "name" => return .name (← str (at! a 1))
  | "attr" => return .attr (← toExpr (at! a 1)) (← str (at! a 2))
  | "call" =>
    let kws ← (← arr (at! a 3)).toList.mapM fun kv => do
      let p ← arr kv
      return ((← optStr (at! p 0)), (← toExpr (at! p 1)))
    return .call (← toExpr (at! a 1)) (← exprs 2) kws
  | "str" => return .str (← str (at! a 1))
  | "int" => return .int ((at! a 1).getInt?.toOption.getD 0)
  | "tuple" => return .tuple (← exprs 1)
  | "walrus" => return .walrus (← str (at! a 1)) (← toExpr (at! a 2))
  | "lambda" => return .lambda (← toParams (at! a 1)) (← toExpr (at! a 2))
  | "comp" =>
    let gs ← (← arr (at! a 1)).toList.mapM fun g => do
      let p ← arr g
      return ((← toExpr (at! p 0)), (← toExpr (at! p 1)),
        (← (← arr (at! p 2)).toList.mapM toExpr))
    return .comp gs (← exprs 2)
  | "starred" => return .starred (← toExpr (at! a 1))
  | "subscript" => return .subscript (← toExpr (at! a 1)) (← toExpr (at! a 2))
  | "ifexp" => return .ifexp (← toExpr (at! a 1)) (← toExpr (at! a 2)) (← toExpr (at! a 3))
  | "or" => return .or (← exprs 1)
  | "other" => return .other (← exprs 1)
  | t => throw s!"unknown expr tag {t}"
where
  toOpt (j : Json) : Except String (Option Expr) :=
    if j.isNull then pure none else some <$> toExpr j
  toParams (j : Json) : Except String (List Param) := do
    (← arr j).toList.mapM fun p => do
      let q ← arr p
      return ((← str (at! q 0)), (← toOpt (at! q 1)), (← toOpt (at! q 2)))

def toOptExpr (j : Json) : Except String (Option Expr) :=
  if j.isNull then pure none else some <$> toExpr j

partial def toPat (j : Json) : Except String Pat := do
  let a ← arr j
  let pats (k : Nat) : Except String (List Pat) := do (← arr (at! a k)).toList.mapM toPat
  match ← str (at! a 0) with
  | "capture" =>
    let sub ← if (at! a 2).isNull then pure none else some <$> toPat (at! a 2)
    return .capture (← optStr (at! a 1)) sub
  | "value" => return .value (← toExpr (at! a 1))
  | "seq" => return .seq (← pats 1)
  | "star" => return .star (← optStr (at! a 1))
  | "or" => return .or (← pats 1)
  | "class" => return .cls (← toExpr (at! a 1)) (← pats 2)
  | "mapping" =>
    return .mapping (← (← arr (at! a 1)).toList.mapM toExpr) (← pats 2) (← optStr (at! a 3))
  | _ => return .other

private def names (j : Json) : Except String (List (String × Option String)) := do
  (← arr j).toList.mapM fun p => do
    let q ← arr p
    return ((← str (at! q 0)), (← optStr (at! q 1)))

private def strs (j : Json) : Except String (List String) := do
  (← arr j).toList.mapM str

partial def toStmt (j : Json) : Except String Stmt := do
  let a ← arr j
  let body (k : Nat) : Except String (List Stmt) := do (← arr (at! a k)).toList.mapM toStmt
  let exprs (k : Nat) : Except String (List Expr) := do (← arr (at! a k)).toList.mapM toExpr
  match ← str (at! a 0) with
  | "import" => return .import (← names (at! a 1))
  | "from" =>
    return .fromImport (← optStr (at! a 1)) ((at! a 2).getNat?.toOption.getD 0) (← names (at! a 3))
  | "assign" => return .assign (← exprs 1) (← toExpr (at! a 2))
  | "aug" => return .aug (← toExpr (at! a 1)) (← toExpr (at! a 2))
  | "ann" => return .ann (← toExpr (at! a 1)) (← toExpr (at! a 2)) (← toOptExpr (at! a 3))
  | "def" =>
    return .defn (← str (at! a 1)) (← toExpr.toParams (at! a 2)) (← exprs 3)
      (← toOptExpr (at! a 4)) (← body 5) (← strs (at! a 6))
  | "class" =>
    return .cls (← str (at! a 1)) (← exprs 2) (← exprs 3) (← body 4) (← strs (at! a 5))
  | "return" => return .ret (← toOptExpr (at! a 1))
  | "expr" => return .expr (← toExpr (at! a 1))
  | "if" => return .ifS (← toExpr (at! a 1)) (← body 2) (← body 3)
  | "while" => return .whileS (← toExpr (at! a 1)) (← body 2) (← body 3)
  | "for" => return .forS (← toExpr (at! a 1)) (← toExpr (at! a 2)) (← body 3) (← body 4)
  | "with" =>
    let items ← (← arr (at! a 1)).toList.mapM fun it => do
      let q ← arr it
      return ((← toExpr (at! q 0)), (← toOptExpr (at! q 1)))
    return .withS items (← body 2)
  | "try" =>
    let hs ← (← arr (at! a 2)).toList.mapM fun h => do
      let q ← arr h
      let hb ← (← arr (at! q 2)).toList.mapM toStmt
      return ((← toOptExpr (at! q 0)), (← optStr (at! q 1)), hb)
    return .tryS (← body 1) hs (← body 3) (← body 4)
  | "global" => return .global (← strs (at! a 1))
  | "nonlocal" => return .nonlocal (← strs (at! a 1))
  | "del" => return .del (← exprs 1)
  | "match" =>
    let cs ← (← arr (at! a 2)).toList.mapM fun c => do
      let q ← arr c
      let cb ← (← arr (at! q 2)).toList.mapM toStmt
      return ((← toPat (at! q 0)), (← toOptExpr (at! q 1)), cb)
    return .matchS (← toExpr (at! a 1)) cs
  | "typealias" => return .typeAlias (← str (at! a 1)) (← strs (at! a 2)) (← toExpr (at! a 3))
  | "other" => return .other (← exprs 1)
  | t => throw s!"unknown stmt tag {t}"

def toModule (j : Json) : Except String Module := do
  let name ← (← j.getObjVal? "name").getStr?
  let isPkg ← (← j.getObjVal? "is_pkg").getBool?
  let file ← (← j.getObjVal? "file").getStr?
  let body ← (← (← j.getObjVal? "body").getArr?).toList.mapM toStmt
  return { name, isPkg, file, body }

def toProject (j : Json) : Except String (List Module) := do
  (← (← j.getObjVal? "modules").getArr?).toList.mapM toModule

end Binding
