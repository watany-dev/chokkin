import Std.Data.HashMap
import Std.Data.HashSet
import Binding.IR
import Binding.Scope

/-!
# Which module attributes a Python project reads

A may-analysis of the Python Language Reference (§4.2 naming and binding, §5
the import system, §7.11 `import`, §6.3.1 attribute references) over a whole
project: every value a name or attribute can hold on some run, computed to a
fixpoint. A *reference* `(importer, m, n)` is a load, written in `importer`,
that evaluates to the binding `n` in module `m`'s namespace.

Values the spec does not model (instances of unknown classes, results of
unknown calls, arithmetic on strings, ...) are the empty set: the spec then
claims no reference, so an unmodelled construct can only hide a chokkin bug,
never invent one. The constructs that are modelled:

* `import a.b` binds `a` and makes `a.b` an attribute of `a` (§5.4.2);
  `import a.b as c` binds `c` to `a.b`; `from p import n` reads `p.n`, which
  is the submodule `p.n` when one exists (§7.11 step 2).
* relative imports against `__package__` (§5.7, PEP 328);
* `from m import *` binds `__all__` or the public names (§7.11);
* scopes: module, function, lambda, class (invisible from nested blocks),
  comprehension, PEP 695 type-parameter scopes, `global` / `nonlocal`,
  PEP 572 assignment expressions;
* flow of values through assignment (including tuple unpacking and starred
  targets), attribute stores on modules, classes and instances, `for`
  targets, `match` capture patterns, calls of project functions (arguments
  to parameters, `return` to the call), methods, decorators, default
  arguments, conditional expressions and `or` / `and`;
* the library functions `getattr`, `__import__`, `importlib.import_module`
  and `sys.modules[...]`.
-/

namespace Binding
open Std

inductive Atom where
  | mod (p : String)
  | dfn (m n : String)
  | func (fid : Nat)
  | klass (cid : Nat)
  | inst (cid : Nat)
  | meth (fid cid : Nat)
  | ext (path : String)
  | builtin (n : String)
  | str (s : String)
  | int (n : Int)
  /-- a tuple (`exact`) or another iterable of the given element values -/
  | seq (exact : Bool) (xs : List (List Atom))
  deriving BEq, Repr, Inhabited

partial def Atom.depth : Atom → Nat
  | .seq _ xs => 1 + xs.foldl (fun d x => x.foldl (fun d a => max d a.depth) d) 0
  | _ => 0

def maxSeqDepth : Nat := 3

def addNew (old : Array Atom) (new : Array Atom) : Array Atom :=
  new.foldl (fun acc a => if acc.contains a || a.depth > maxSeqDepth then acc else acc.push a) old

def union (xs : List (Array Atom)) : Array Atom := xs.foldl addNew #[]

structure St where
  env : HashMap (Nat × String) (Array Atom) := {}
  returns : HashMap Nat (Array Atom) := {}
  fns : HashMap Nat (List String) := {}
  imported : HashSet String := {}
  starBound : HashSet (String × String) := {}
  refs : HashSet (String × String × String) := {}
  scopes : HashMap Nat ScopeInfo := {}
  next : Nat := 0
  changed : Bool := false
  deriving Inhabited

structure World where
  modules : HashMap String Module
  moduleIds : HashMap String Nat
  moduleLocals : HashMap String (List String)
  /-- absent: no `__all__`; `none`: bound but not a literal list -/
  allOf : HashMap String (Option (List String))

abbrev M := ReaderT World (StateM St)

structure Ctx where
  mod : String
  isPkg : Bool
  scope : Nat
  fn : Option Nat

/-! ## Binders of a block (§4.2.1) -/

def stripStars (s : String) : String := (s.dropWhile (· == '*'))

partial def subExprs : Expr → List Expr
  | .attr e _ => [e]
  | .call f a k => f :: a ++ k.map (·.2)
  | .tuple xs | .or xs | .other xs => xs
  | .walrus _ v => [v]
  | .lambda ps _ => ps.filterMap (·.2.1)
  | .comp gs es => gs.foldr (fun (t, i, fs) acc => t :: i :: fs ++ acc) es
  | .starred e => [e]
  | .subscript e i => [e, i]
  | .ifexp t a b => [t, a, b]
  | _ => []

/-- Assignment-expression targets in `e`, looking into comprehensions (whose
walrus binds in the containing block) but not into lambdas. -/
partial def walrusNames (e : Expr) : List String :=
  let here := match e with | .walrus n _ => [n] | _ => []
  here ++ (subExprs e).foldr (fun x acc => walrusNames x ++ acc) []

partial def targetNames : Expr → List String
  | .name n => [n]
  | .tuple ts => ts.foldr (fun t acc => targetNames t ++ acc) []
  | .starred t => targetNames t
  | _ => []

partial def patNames : Pat → List String
  | .capture n sub => n.toList ++ (sub.map patNames).getD []
  | .seq ps | .or ps | .cls _ ps => ps.foldr (fun p acc => patNames p ++ acc) []
  | .star n => n.toList
  | .mapping _ ps r => ps.foldr (fun p acc => patNames p ++ acc) [] ++ r.toList
  | _ => []

structure Binders where
  bound : List String := []
  globals : List String := []
  nonlocals : List String := []

def Binders.add (b : Binders) (ns : List String) : Binders := { b with bound := b.bound ++ ns }

def wal (es : List Expr) : List String := es.foldr (fun e acc => walrusNames e ++ acc) []

partial def blockBinders (body : List Stmt) : Binders :=
  body.foldl stmt {}
where
  stmt (b : Binders) : Stmt → Binders
    | .import ns => b.add (ns.map fun (d, a) => a.getD ((d.splitOn ".").headD d))
    | .fromImport _ _ ns => b.add ((ns.filter (·.1 != "*")).map fun (n, a) => a.getD n)
    | .assign ts v => b.add (ts.foldr (fun t acc => targetNames t ++ acc) [] ++ wal (v :: ts))
    | .aug t v => b.add (targetNames t ++ wal [t, v])
    | .ann t a v => b.add (targetNames t ++ wal (t :: a :: v.toList))
    | .defn n ps ds r _ _ =>
      b.add (n :: wal (ds ++ r.toList ++ ps.filterMap (·.2.1) ++ ps.filterMap (·.2.2)))
    | .cls n bs ds _ _ => b.add (n :: wal (bs ++ ds))
    | .ret v => b.add (wal v.toList)
    | .expr e => b.add (wal [e])
    | .ifS t x o | .whileS t x o => (o.foldl stmt (x.foldl stmt (b.add (wal [t]))))
    | .forS t i x o => o.foldl stmt (x.foldl stmt (b.add (targetNames t ++ wal [t, i])))
    | .withS items x =>
      x.foldl stmt (b.add (items.foldr (fun (e, t) acc =>
        wal [e] ++ (t.map targetNames).getD [] ++ acc) []))
    | .tryS x hs o f =>
      let b := x.foldl stmt b
      let b := hs.foldl (fun b (t, n, hb) => hb.foldl stmt (b.add (n.toList ++ wal t.toList))) b
      f.foldl stmt (o.foldl stmt b)
    | .global ns => { b with globals := b.globals ++ ns }
    | .nonlocal ns => { b with nonlocals := b.nonlocals ++ ns }
    | .del ts => b.add (ts.foldr (fun t acc => targetNames t ++ acc) [])
    | .matchS s cs =>
      cs.foldl (fun b (p, g, cb) => cb.foldl stmt (b.add (patNames p ++ wal g.toList)))
        (b.add (wal [s]))
    | .typeAlias n _ _ => b.add [n]
    | .other es => b.add (wal es)

def Binders.locals (b : Binders) (extra : List String := []) : List String :=
  (extra ++ b.bound).eraseDups.filter fun n => !(b.globals.contains n || b.nonlocals.contains n)

/-! ## `__all__` (literal forms only; anything else makes it unreadable) -/

def strLits : Expr → Option (List String)
  | .tuple xs => xs.mapM fun | .str s => some s | _ => none
  | _ => none

/-- `none`: not bound; `some none`: bound in a form that is not read. -/
partial def scanAll (body : List Stmt) : Option (Option (List String)) :=
  let (cur, bad) := go body (none, false)
  match cur, bad with
  | none, false => none
  | _, true => some none
  | some l, false => some (some l)
where
  set (st : Option (List String) × Bool) (v : Option (List String)) :=
    match v with | some l => (some l, st.2) | none => (st.1, true)
  app (st : Option (List String) × Bool) (v : Option (List String)) :=
    match v, st.1 with
    | some l, some c => (some (c ++ l), st.2)
    | _, _ => (st.1, true)
  go (b : List Stmt) (st : Option (List String) × Bool) : Option (List String) × Bool :=
    b.foldl (fun st s => match s with
      | .assign [.name "__all__"] v => set st (strLits v)
      | .ann (.name "__all__") _ (some v) => set st (strLits v)
      | .aug (.name "__all__") v => app st (strLits v)
      | .expr (.call (.attr (.name "__all__") "extend") [v] []) => app st (strLits v)
      | .expr (.call (.attr (.name "__all__") "append") [.str s] []) => app st (some [s])
      | .assign ts _ =>
        if ts.any (fun t => (targetNames t).contains "__all__") then (st.1, true) else st
      | .ifS _ x o => go o (go x st)
      | .tryS x hs o f => go f (go o (hs.foldl (fun st (_, _, hb) => go hb st) (go x st)))
      | .withS _ x => go x st
      | _ => st) st

/-! ## The evaluator -/

def getScope (i : Nat) : M ScopeInfo := do return (← get).scopes.getD i default

partial def chainOf (i : Nat) (fuel : Nat := 64) : M Chain := do
  let s ← getScope i
  match s.parent, fuel with
  | some p, f + 1 => return (i, s) :: (← chainOf p f)
  | _, _ => return [(i, s)]

def envAt (sid : Nat) (n : String) : M (Array Atom) := do
  return (← get).env.getD (sid, n) #[]

def bindAt (sid : Nat) (n : String) (xs : Array Atom) : M Unit := do
  let old ← envAt sid n
  let new := addNew old xs
  if new.size != old.size then
    modify fun st => { st with env := st.env.insert (sid, n) new, changed := true }

def addReturn (fid : Nat) (xs : Array Atom) : M Unit := do
  let old := (← get).returns.getD fid #[]
  let new := addNew old xs
  if new.size != old.size then
    modify fun st => { st with returns := st.returns.insert fid new, changed := true }

def addRef (imp m n : String) : M Unit := do
  if imp != m then modify fun st => { st with refs := st.refs.insert (imp, m, n) }

def isModule (p : String) : M Bool := do return (← read).modules.contains p

def globalScope (m : String) : M (Option Nat) := do return (← read).moduleIds.get? m

def prefixes (d : String) : List String :=
  let parts := d.splitOn "."
  (List.range parts.length).map fun k => ".".intercalate (parts.take (k + 1))

def markImported (d : String) : M Unit := do
  for p in prefixes d do
    if !(← get).imported.contains p then
      modify fun st => { st with imported := st.imported.insert p, changed := true }

def bindsGlobal (m n : String) : M Bool := do
  let w ← read
  if ((w.moduleLocals.getD m []).contains n) || (← get).starBound.contains (m, n) then return true
  match w.moduleIds.get? m with
  | some g => return !(← envAt g n).isEmpty
  | none => return false

def newScope (kind : Kind) (parent : Nat) (m : String) (locals globals nonlocals : List String) :
    M Nat := do
  let st ← get
  let i := st.next
  set { st with next := i + 1,
                scopes := st.scopes.insert i { kind, parent := some parent, module := m,
                                               locals, globals, nonlocals } }
  return i

/-- Expand pointers into module namespaces to the values bound there. -/
partial def deref (xs : Array Atom) (fuel : Nat := 6) : M (Array Atom) := do
  let mut out := xs
  if fuel = 0 then return out
  for x in xs do
    if let .dfn m n := x then
      if let some g ← globalScope m then
        out := addNew out (← deref (← envAt g n) (fuel - 1))
  return out

def elems (xs : Array Atom) : Array Atom :=
  xs.foldl (fun acc x => match x with
    | .seq _ es => es.foldl (fun a e => addNew a e.toArray) acc
    | _ => acc) #[]

def strsOf (xs : Array Atom) : List String :=
  xs.toList.filterMap fun | .str s => some s | _ => none

def readName (ctx : Ctx) (n : String) : M (Array Atom) := do
  let c ← chainOf ctx.scope
  let mut out := #[]
  for (sid, s) in resolveLoad c n do
    out := addNew out (← envAt sid n)
    if s.kind = .module && !(((← read).moduleLocals.getD s.module []).contains n) then
      out := addNew out #[.builtin n]
      -- module attributes set by the import system (§5.4.4)
      if n == "__name__" then out := addNew out #[.str ctx.mod]
      if n == "__package__" then
        let pkg := if ctx.isPkg then ctx.mod else ".".intercalate (ctx.mod.splitOn ".").dropLast
        out := addNew out #[.str pkg]
  for x in out do
    if let .dfn m k := x then addRef ctx.mod m k
  return out

def storeName (ctx : Ctx) (n : String) (xs : Array Atom) : M Unit := do
  let c ← chainOf ctx.scope
  match resolveStore c n with
  | some (sid, _) => bindAt sid n xs
  | none => pure ()

/-- `module.a` where `module` is `p` (§6.3.1, §5.4.2). -/
def modAttr (ctx : Ctx) (p a : String) : M (Array Atom) := do
  let sub := p ++ "." ++ a
  if !(← isModule p) then return #[.ext sub]
  let mut out := #[]
  if (← isModule sub) && (← get).imported.contains sub then out := out.push (.mod sub)
  if (← bindsGlobal p a) || !(← isModule sub) then
    out := out.push (.dfn p a)
    addRef ctx.mod p a
  return out

partial def attrOf (ctx : Ctx) (x : Atom) (a : String) (fuel : Nat := 6) : M (Array Atom) := do
  match x with
  | .mod p => modAttr ctx p a
  | .ext p => return #[.ext (p ++ "." ++ a)]
  | .dfn m n =>
    if fuel = 0 then return #[]
    match ← globalScope m with
    | some g =>
      let mut out := #[]
      for y in ← envAt g n do out := addNew out (← attrOf ctx y a (fuel - 1))
      return out
    | none => return #[]
  | .klass cid => envAt cid a
  | .inst cid =>
    return (← envAt cid a).map fun | .func f => .meth f cid | y => y
  | _ => return #[]

def resolveImportName (ctx : Ctx) (m : Option String) (level : Nat) : Option String :=
  if level = 0 then m else
  match relBase (ctx.mod.splitOn ".") ctx.isPkg level with
  | none => none
  | some base =>
    let parts := base ++ (m.map (·.splitOn ".")).getD []
    some (".".intercalate parts)

/-- `importlib.import_module(name, package)` (Library Reference, importlib). -/
def importModuleName (name : String) (pkg : Option String) : Option String :=
  let dots := (name.takeWhile (· == '.')).length
  if dots = 0 then some name else
  match pkg with
  | none => none
  | some p =>
    let parts := p.splitOn "."
    if dots - 1 < parts.length then
      let base := parts.take (parts.length - (dots - 1))
      let rest := name.drop dots
      some (".".intercalate (if rest.isEmpty then base else base ++ [rest]))
    else none

mutual

partial def evalE (ctx : Ctx) (e : Expr) : M (Array Atom) := do
  match e with
  | .name n => readName ctx n
  | .attr b a =>
    let mut out := #[]
    for x in ← evalE ctx b do out := addNew out (← attrOf ctx x a)
    return out
  | .call f args kws =>
    let fs ← evalE ctx f
    let as ← args.mapM (evalE ctx)
    let ks ← kws.mapM fun (k, v) => do return (k, ← evalE ctx v)
    callAtoms ctx fs args as ks
  | .str s => return #[.str s]
  | .int n => return #[.int n]
  | .tuple xs =>
    let vs ← xs.mapM (evalE ctx)
    if xs.any (fun | .starred _ => true | _ => false) then
      let flat := (xs.zip vs).map fun
        | (.starred _, v) => elems v
        | (_, v) => v
      return #[.seq false [(union flat).toList]]
    return #[.seq true (vs.map (·.toList))]
  | .walrus n v =>
    let xs ← evalE ctx v
    match walrusTarget (← chainOf ctx.scope) n with
    | some (sid, _) => bindAt sid n xs
    | none => pure ()
    return xs
  | .lambda ps body =>
    let fid ← newScope .func ctx.scope ctx.mod
      ((ps.map (stripStars ·.1)) ++ walrusNames body).eraseDups [] []
    modify fun st => { st with fns := st.fns.insert fid (ps.map (·.1)) }
    for (p, d, _) in ps do
      if let some d := d then bindAt fid (stripStars p) (← evalE ctx d)
    let r ← evalE { ctx with scope := fid, fn := some fid } body
    addReturn fid r
    return #[.func fid]
  | .comp gens es =>
    let cid ← newScope .comp ctx.scope ctx.mod
      (gens.foldr (fun (t, _, _) acc => targetNames t ++ acc) []).eraseDups [] []
    let cctx := { ctx with scope := cid }
    let mut first := true
    for (t, it, ifs) in gens do
      let xs ← evalE (if first then ctx else cctx) it
      first := false
      assignT cctx t (elems (← deref xs))
      for c in ifs do discard <| evalE cctx c
    let ys ← es.mapM (evalE cctx)
    return #[.seq false [(union ys).toList]]
  | .starred b => evalE ctx b
  | .subscript b i =>
    let xs ← deref (← evalE ctx b)
    let is ← deref (← evalE ctx i)
    let mut out := #[]
    for x in xs do
      match x with
      | .ext "sys.modules" =>
        for s in strsOf is do
          if ← isModule s then out := addNew out #[.mod s]
      | .seq true es =>
        for j in is do
          if let .int k := j then
            if 0 ≤ k then out := addNew out (es.getD k.toNat []).toArray
      | .seq false _ =>
        if is.any (fun | .int _ => true | _ => false) then out := addNew out (elems #[x])
      | _ => pure ()
    return out
  | .ifexp t a b =>
    discard <| evalE ctx t
    return addNew (← evalE ctx a) (← evalE ctx b)
  | .or xs => return union (← xs.mapM (evalE ctx))
  | .other xs =>
    for x in xs do discard <| evalE ctx x
    return #[]

/-- Bind parameters of `fid` from a call (§6.3.4); `self` already bound. -/
partial def flow (fid : Nat) (skip : Nat) (as : List (Array Atom)) (args : List Expr)
    (ks : List (Option String × Array Atom)) : M Unit := do
  let ps := (← get).fns.getD fid []
  let positional := (ps.drop skip).takeWhile (fun p => !p.startsWith "*")
  let mut i := 0
  for (a, e) in as.zip args do
    if let .starred _ := e then break
    if h : i < positional.length then bindAt fid positional[i] a
    i := i + 1
  for (k, v) in ks do
    if let some k := k then
      if ps.contains k then bindAt fid k v

partial def callAtoms (ctx : Ctx) (fs : Array Atom) (args : List Expr) (as : List (Array Atom))
    (ks : List (Option String × Array Atom)) : M (Array Atom) := do
  let mut out := #[]
  for f in ← deref fs do
    match f with
    | .func fid =>
      flow fid 0 as args ks
      out := addNew out ((← get).returns.getD fid #[])
    | .meth fid cid =>
      if let some p := ((← get).fns.getD fid []).head? then bindAt fid p #[.inst cid]
      flow fid 1 as args ks
      out := addNew out ((← get).returns.getD fid #[])
    | .klass cid =>
      out := addNew out #[.inst cid]
      for g in ← envAt cid "__init__" do
        if let .func fid := g then
          if let some p := ((← get).fns.getD fid []).head? then bindAt fid p #[.inst cid]
          flow fid 1 as args ks
    | .builtin "getattr" =>
      match as with
      | o :: n :: rest =>
        for name in strsOf (← deref n) do
          for x in ← deref o do out := addNew out (← attrOf ctx x name)
        for d in rest.take 1 do out := addNew out d
      | _ => pure ()
    | .builtin "__import__" =>
      if let some n := as.head? then
        let fromlist := match args.drop 3, ks.find? (·.1 == some "fromlist") with
          | (.tuple []) :: _, _ => false
          | _ :: _, _ => true
          | [], some _ => true
          | [], none => false
        for s in strsOf (← deref n) do
          if ← isModule s then
            markImported s
            out := addNew out #[.mod (if fromlist then s else (s.splitOn ".").headD s)]
    | .ext "importlib.import_module" =>
      if let some n := as.head? then
        let pkgArg := (as.drop 1).head? <|> (ks.find? (·.1 == some "package")).map (·.2)
        let pkgs ← match pkgArg with
          | some p => do pure ((strsOf (← deref p)).map some)
          | none => pure [none]
        for s in strsOf (← deref n) do
          for p in pkgs do
            if let some full := importModuleName s p then
              if ← isModule full then
                markImported full
                out := addNew out #[.mod full]
    | _ => pure ()
  return out

partial def assignT (ctx : Ctx) (t : Expr) (xs : Array Atom) : M Unit := do
  match t with
  | .name n => storeName ctx n xs
  | .tuple ts =>
    let ys ← deref xs
    let hasStar := ts.any fun | .starred _ => true | _ => false
    let mut per : Array (Array Atom) := Array.mkArray ts.length #[]
    for y in ys do
      match y with
      | .seq true es =>
        if !hasStar && es.length == ts.length then
          per := (per.zip es.toArray).map fun (p, e) => addNew p e.toArray
        else per := per.map fun p => addNew p (elems #[y])
      | .seq false _ => per := per.map fun p => addNew p (elems #[y])
      | _ => pure ()
    for (t, p) in ts.zip per.toList do
      match t with
      | .starred b => assignT ctx b #[.seq false [p.toList]]
      | _ => assignT ctx t p
  | .starred b => assignT ctx b #[.seq false [(elems (← deref xs)).toList]]
  | .attr b a =>
    for o in ← deref (← evalE ctx b) do
      match o with
      | .mod p => if let some g ← globalScope p then bindAt g a xs
      | .klass cid | .inst cid => bindAt cid a xs
      | _ => pure ()
  | .subscript b i =>
    discard <| evalE ctx b
    discard <| evalE ctx i
  | _ => discard <| evalE ctx t

partial def matchP (ctx : Ctx) (p : Pat) (xs : Array Atom) : M Unit := do
  match p with
  | .capture n sub =>
    if let some n := n then storeName ctx n xs
    if let some s := sub then matchP ctx s xs
  | .value e => discard <| evalE ctx e
  | .seq ps =>
    let ys ← deref xs
    let hasStar := ps.any fun | .star _ => true | _ => false
    for (k, q) in ps.enum do
      let mut v := #[]
      for y in ys do
        match y with
        | .seq true es =>
          if !hasStar && es.length == ps.length then v := addNew v (es.getD k []).toArray
          else v := addNew v (elems #[y])
        | .seq false _ => v := addNew v (elems #[y])
        | _ => pure ()
      matchP ctx q v
  | .star n => if let some n := n then storeName ctx n #[.seq false [(elems (← deref xs)).toList]]
  | .or ps => for q in ps do matchP ctx q xs
  | .cls e ps =>
    discard <| evalE ctx e
    for q in ps do matchP ctx q #[]
  | .mapping ks ps r =>
    for k in ks do discard <| evalE ctx k
    for q in ps do matchP ctx q #[]
    if let some r := r then storeName ctx r #[]
  | .other => pure ()

partial def fromAttr (ctx : Ctx) (base n : String) : M (Array Atom) := do
  let sub := base ++ "." ++ n
  if ← isModule base then
    let mut out := #[]
    if ← isModule sub then
      markImported sub
      out := out.push (.mod sub)
    if (← bindsGlobal base n) || !(← isModule sub) then
      out := out.push (.dfn base n)
      addRef ctx.mod base n
    return out
  return #[.ext sub]

partial def starImport (ctx : Ctx) (base : String) : M Unit := do
  if !(← isModule base) then return
  let w ← read
  let own := w.moduleLocals.getD base []
  let starred := (← get).starBound.toList.filterMap fun (m, n) => if m == base then some n else none
  let names := match w.allOf.get? base with
    | some (some l) => l
    | some none => own ++ starred
    | none => starNames none (own ++ starred)
  let some g ← globalScope ctx.mod | return
  for n in names.eraseDups do
    if !(← get).starBound.contains (ctx.mod, n) then
      modify fun st => { st with starBound := st.starBound.insert (ctx.mod, n), changed := true }
    bindAt g n #[.dfn base n]
    -- §7.11: a submodule named in a package's `__all__` is imported by `*`
    let sub := base ++ "." ++ n
    if (w.allOf.get? base).isSome && (← isModule sub) then
      markImported sub
      bindAt g n #[.mod sub]

partial def execB (ctx : Ctx) (b : List Stmt) : M Unit := do
  for s in b do execS ctx s

partial def execS (ctx : Ctx) (s : Stmt) : M Unit := do
  match s with
  | .import ns =>
    for (d, a) in ns do
      markImported d
      match a with
      | some a => storeName ctx a #[.mod d]
      | none => let top := (d.splitOn ".").headD d; storeName ctx top #[.mod top]
  | .fromImport m level ns =>
    let some base := resolveImportName ctx m level | return
    if ns.any (·.1 == "*") then starImport ctx base
    else
      for (n, a) in ns do storeName ctx (a.getD n) (← fromAttr ctx base n)
  | .assign ts v =>
    let xs ← evalE ctx v
    for t in ts do assignT ctx t xs
  | .aug t v =>
    discard <| evalE ctx t
    discard <| evalE ctx v
  | .ann t a v =>
    discard <| evalE ctx a
    match v with
    | some v => assignT ctx t (← evalE ctx v)
    | none => match t with
      | .attr b _ => discard <| evalE ctx b
      | .subscript b i => discard <| evalE ctx b; discard <| evalE ctx i
      | _ => pure ()
  | .defn name ps decos ret body tps =>
    let decoVals ← decos.mapM (evalE ctx)
    let outer ← if tps.isEmpty then pure ctx.scope
      else newScope .tparams ctx.scope ctx.mod tps [] []
    let actx := { ctx with scope := outer }
    let b := blockBinders body
    let fid ← newScope .func outer ctx.mod (b.locals (ps.map (stripStars ·.1))) b.globals b.nonlocals
    modify fun st => { st with fns := st.fns.insert fid (ps.map (·.1)) }
    for (p, d, an) in ps do
      if let some an := an then discard <| evalE actx an
      if let some d := d then bindAt fid (stripStars p) (← evalE ctx d)
    if let some r := ret then discard <| evalE actx r
    let mut v := #[Atom.func fid]
    for dv in decoVals do
      v := addNew v (← callAtoms ctx dv [.name name] [#[.func fid]] [])
    storeName ctx name v
    execB { ctx with scope := fid, fn := some fid } body
  | .cls name bases decos body tps =>
    let outer ← if tps.isEmpty then pure ctx.scope
      else newScope .tparams ctx.scope ctx.mod tps [] []
    for x in bases do discard <| evalE { ctx with scope := outer } x
    for x in decos do discard <| evalE ctx x
    let b := blockBinders body
    let cid ← newScope .cls outer ctx.mod (b.locals) b.globals b.nonlocals
    storeName ctx name #[.klass cid]
    execB { ctx with scope := cid, fn := none } body
  | .ret v =>
    if let some v := v then
      let xs ← evalE ctx v
      if let some f := ctx.fn then addReturn f xs
  | .expr e => discard <| evalE ctx e
  | .ifS t x o | .whileS t x o =>
    discard <| evalE ctx t
    execB ctx x
    execB ctx o
  | .forS t i x o =>
    let xs ← evalE ctx i
    assignT ctx t (elems (← deref xs))
    execB ctx x
    execB ctx o
  | .withS items x =>
    for (e, t) in items do
      discard <| evalE ctx e
      if let some t := t then assignT ctx t #[]
    execB ctx x
  | .tryS x hs o f =>
    execB ctx x
    for (t, _, hb) in hs do
      if let some t := t then discard <| evalE ctx t
      execB ctx hb
    execB ctx o
    execB ctx f
  | .global _ | .nonlocal _ => pure ()
  | .del ts =>
    for t in ts do
      match t with
      | .attr b _ => discard <| evalE ctx b
      | .subscript b i => discard <| evalE ctx b; discard <| evalE ctx i
      | _ => pure ()
  | .matchS subj cases =>
    let xs ← evalE ctx subj
    for (p, g, cb) in cases do
      matchP ctx p xs
      if let some g := g then discard <| evalE ctx g
      execB ctx cb
  | .typeAlias name tps v =>
    let sc ← if tps.isEmpty then pure ctx.scope
      else newScope .tparams ctx.scope ctx.mod tps [] []
    discard <| evalE { ctx with scope := sc } v
    storeName ctx name #[]
  | .other es => for e in es do discard <| evalE ctx e

end

/-! ## Whole project -/

/-- Add the namespace packages (directories without `__init__.py`, PEP 420)
that the module names imply. -/
def withNamespaces (ms : List Module) : List Module :=
  let names := ms.map (·.name)
  let missing := (ms.foldr (fun m acc => (prefixes m.name).dropLast ++ acc) []).eraseDups.filter
    (fun p => !names.contains p)
  ms ++ missing.map fun p => { name := p, isPkg := true, file := "", body := [] }

def mkWorld (ms : List Module) : World :=
  let ms := withNamespaces ms
  { modules := HashMap.ofList (ms.map fun m => (m.name, m))
    moduleIds := HashMap.ofList (ms.enum.map fun (i, m) => (m.name, i))
    moduleLocals := HashMap.ofList (ms.map fun m => (m.name, (blockBinders m.body).locals))
    allOf := HashMap.ofList (ms.filterMap fun m => (scanAll m.body).map (m.name, ·)) }

def pass (ms : List Module) : M Unit := do
  let ms := withNamespaces ms
  modify fun st => { st with next := ms.length, changed := false }
  for (i, m) in ms.enum do
    let b := blockBinders m.body
    let info : ScopeInfo := { kind := .module, parent := none, module := m.name,
                              locals := b.locals, globals := [], nonlocals := [] }
    modify fun st => { st with scopes := st.scopes.insert i info }
  for (i, m) in ms.enum do
    execB { mod := m.name, isPkg := m.isPkg, scope := i, fn := none } m.body

/-- References through names a module re-binds by `from m import *`. -/
def closeRefs (w : World) (st : St) : HashSet (String × String × String) := Id.run do
  let mut refs := st.refs
  let mut work := refs.toList
  for _ in [0:100000] do
    match work with
    | [] => break
    | (imp, m, n) :: rest =>
      work := rest
      match w.moduleIds.get? m with
      | none => pure ()
      | some g =>
        for x in st.env.getD (g, n) #[] do
          if let .dfn m2 n2 := x then
            if imp != m2 && !refs.contains (imp, m2, n2) then
              refs := refs.insert (imp, m2, n2)
              work := (imp, m2, n2) :: work
  return refs

def analyze (ms : List Module) (maxPasses : Nat := 40) : List (String × String × String) × Nat :=
  let w := mkWorld ms
  let rec loop : Nat → St → St × Nat
    | 0, st => (st, maxPasses)
    | k + 1, st =>
      let st' := ((pass ms).run w |>.run st).2
      if st'.changed then loop k st' else (st', maxPasses - k)
  let (st, n) := loop maxPasses {}
  ((closeRefs w st).toList, n)

end Binding
