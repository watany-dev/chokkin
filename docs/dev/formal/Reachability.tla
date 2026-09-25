---------------------------- MODULE Reachability ----------------------------
(***************************************************************************)
(* Model of `run_reachability_bfs` (src/reachability/bfs.rs) together with *)
(* the edge construction in `add_parsed_imports` (src/graph/edges.rs) and  *)
(* the framework-glob seeding in `analyze_reachability`                    *)
(* (src/reachability/build.rs).                                            *)
(*                                                                         *)
(* Files import *modules*, statically (`parsed.imports`) or dynamically    *)
(* (`parsed.dynamic_imports`).  A module resolves to at most one           *)
(* first-party file (`ModuleIndex::resolve`; None for stdlib/third-party). *)
(* A site that imports `pkg.sub.mod` also reaches the files of `pkg` and   *)
(* `pkg.sub` (`module_and_parents`: the module itself, then each parent    *)
(* package outermost first), because Python initialises the parent         *)
(* packages first.  A dynamic site that shares its line with a static      *)
(* import of the same module is static (`build_dynamic_sites`); at module *)
(* granularity that is `Dynamic[f] \ Static[f]`, and static sites come     *)
(* first in the adjacency.                                                 *)
(*                                                                         *)
(* Seeds: entry roots (`Root` step), then every resolvable module/parent  *)
(* of a plugin `module_ref` (`PluginRef` step).  After that queue drains,  *)
(* framework-glob files are enqueued with their own `PluginRef` step and   *)
(* the queue drains again, so their imports are followed and files that   *)
(* the entry walk already reached keep their trace.                        *)
(*                                                                         *)
(* A walk that resolves to a first-party file records one                  *)
(* `FileReachesFile{from,to,via}` edge per (from, to) pair (first via      *)
(* wins), and `enqueue_file` records the predecessor only for the first    *)
(* arrival.                                                                *)
(*                                                                         *)
(* Checked properties                                                      *)
(*   ReachSound      : reachable = closure of the seeds over module        *)
(*                     imports, parent packages included                   *)
(*   ParentReached   : every resolvable parent package of a module that a  *)
(*                     reachable file or plugin ref imports is reachable   *)
(*   FrameworkFollowed : files imported by framework-glob files are        *)
(*                     reachable                                           *)
(*   EntryTraceKept  : framework seeding never overwrites a predecessor    *)
(*                     recorded by the entry walk                          *)
(*   NoDuplicateEdge : FileReachesFile edges form a set (no duplicates)    *)
(*   ViaFaithful     : a file reached only through dynamic imports has a   *)
(*                     predecessor step tagged DynamicImport               *)
(*   TraceNonEmpty   : trace_to_file yields >= 1 step for every reachable  *)
(*                     file                                                *)
(*   Terminates      : both drains finish (so the invariants above are not *)
(*                     vacuous)                                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS Files,         \* set of first-party files
          Modules,       \* set of module names
          Resolve,       \* Resolve[m] : file of module m, or None
          Parent,        \* Parent[m]  : enclosing package of m, or None
          Roots,         \* entry roots (subset of Files)
          Static,        \* Static[f]  : modules imported statically by f
          Dynamic,       \* Dynamic[f] : modules imported via importlib by f
          PluginRefs,    \* modules named by plugin module_refs
          Framework,     \* files matched by framework globs
          None           \* model value for "no file / no predecessor"

ASSUME Roots \subseteq Files
ASSUME Framework \subseteq Files
ASSUME PluginRefs \subseteq Modules
ASSUME \A m \in Modules : Resolve[m] \in Files \cup {None}
ASSUME \A m \in Modules : Parent[m] \in Modules \cup {None}

RECURSIVE SetToSeq(_)
SetToSeq(S) ==
    IF S = {} THEN <<>>
    ELSE LET x == CHOOSE x \in S : TRUE IN <<x>> \o SetToSeq(S \ {x})

(* Enclosing packages of m, outermost first. *)
RECURSIVE Up(_)
Up(m) == IF Parent[m] = None THEN <<>> ELSE Up(Parent[m]) \o <<Parent[m]>>

(* module_and_parents(m) *)
Chain(m) == <<m>> \o Up(m)

Ancestors(m) == {Up(m)[i] : i \in 1..Len(Up(m))}

(* Files reached by one site importing m, in visiting order. *)
RECURSIVE ResolvedSeq(_)
ResolvedSeq(s) ==
    IF s = <<>> THEN <<>>
    ELSE (IF Resolve[Head(s)] = None THEN <<>> ELSE <<Resolve[Head(s)]>>)
         \o ResolvedSeq(Tail(s))

Targets(m) == ResolvedSeq(Chain(m))
TargetSet(m) == {Targets(m)[i] : i \in 1..Len(Targets(m))}

DynamicOnly(f) == Dynamic[f] \ Static[f]

(* Every file one file reaches through its import sites. *)
Adj(f) == UNION {TargetSet(m) : m \in Static[f] \cup Dynamic[f]}

PluginTargets == UNION {TargetSet(m) : m \in PluginRefs}

VARIABLES queue,       \* sequence of files
          reachable,   \* set of files
          pred,        \* partial function file -> [from, step]
          edges,       \* bag (function to Nat) of FileReachesFile records
          entryPred,   \* history: pred when the entry walk finished
          pc

vars == <<queue, reachable, pred, edges, entryPred, pc>>

EdgeRec(f, t, v) == [from |-> f, to |-> t, via |-> v]

(* reach_edges.entry((from, to)).or_insert(via) *)
AddEdge(bag, e) ==
    IF \E d \in DOMAIN bag : d.from = e.from /\ d.to = e.to THEN bag
    ELSE bag @@ (e :> 1)

(* enqueue_file: only the first arrival is recorded *)
Enqueue(q, r, p, target, from, step) ==
    IF target \in r THEN <<q, r, p>>
    ELSE << Append(q, target),
            r \cup {target},
            p @@ (target :> [from |-> from, step |-> step]) >>

(* Seed a list of files with no predecessor. *)
RECURSIVE Seed(_, _, _, _, _, _)
Seed(ts, i, step, q, r, p) ==
    IF i > Len(ts) THEN <<q, r, p>>
    ELSE LET e == Enqueue(q, r, p, ts[i], None, step)
         IN Seed(ts, i + 1, step, e[1], e[2], e[3])

(* enqueue_resolved_module over the files of one site *)
RECURSIVE Walk(_, _, _, _, _, _, _, _)
Walk(f, ts, i, via, q, r, p, bag) ==
    IF i > Len(ts) THEN <<q, r, p, bag>>
    ELSE LET t == ts[i]
             bag2 == IF t # f THEN AddEdge(bag, EdgeRec(f, t, via)) ELSE bag
             e == Enqueue(q, r, p, t, f, via)
         IN Walk(f, ts, i + 1, via, e[1], e[2], e[3], bag2)

(* enqueue_import over every site of one kind *)
RECURSIVE Sites(_, _, _, _, _, _, _, _)
Sites(f, ms, i, via, q, r, p, bag) ==
    IF i > Len(ms) THEN <<q, r, p, bag>>
    ELSE LET s == Walk(f, Targets(ms[i]), 1, via, q, r, p, bag)
         IN Sites(f, ms, i + 1, via, s[1], s[2], s[3], s[4])

RECURSIVE Concat(_)
Concat(ms) == IF ms = <<>> THEN <<>> ELSE Targets(Head(ms)) \o Concat(Tail(ms))

Init ==
    LET s0 == Seed(SetToSeq(Roots), 1, "Root", <<>>, {}, [x \in {} |-> x])
        s1 == Seed(Concat(SetToSeq(PluginRefs)), 1, "PluginRef", s0[1], s0[2], s0[3])
    IN /\ queue = s1[1]
       /\ reachable = s1[2]
       /\ pred = s1[3]
       /\ edges = [e \in {} |-> 0]
       /\ entryPred = [x \in {} |-> x]
       /\ pc = "entry"

Step ==
    /\ pc \in {"entry", "framework"}
    /\ queue # <<>>
    /\ LET f == Head(queue)
           s1 == Sites(f, SetToSeq(Static[f]), 1, "Import", Tail(queue), reachable, pred, edges)
           s2 == Sites(f, SetToSeq(DynamicOnly(f)), 1, "DynamicImport", s1[1], s1[2], s1[3], s1[4])
       IN /\ queue' = s2[1]
          /\ reachable' = s2[2]
          /\ pred' = s2[3]
          /\ edges' = s2[4]
          /\ UNCHANGED <<entryPred, pc>>

(* The entry walk is drained: enqueue the framework-glob files. *)
SeedFramework ==
    /\ pc = "entry"
    /\ queue = <<>>
    /\ LET s == Seed(SetToSeq(Framework), 1, "PluginRef", queue, reachable, pred)
       IN /\ queue' = s[1]
          /\ reachable' = s[2]
          /\ pred' = s[3]
    /\ entryPred' = pred
    /\ pc' = "framework"
    /\ UNCHANGED edges

Done ==
    /\ pc = "framework"
    /\ queue = <<>>
    /\ pc' = "done"
    /\ UNCHANGED <<queue, reachable, pred, edges, entryPred>>

Next == Step \/ SeedFramework \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Reference semantics                                                     *)
(***************************************************************************)
RECURSIVE Closure(_)
Closure(S) ==
    LET nxt == S \cup UNION {Adj(f) : f \in S}
    IN IF nxt = S THEN S ELSE Closure(nxt)

ExpectedReachable == Closure(Roots \cup PluginTargets \cup Framework)

(* trace_to_file: follow pred until from = None; 0 steps when no pred entry *)
RECURSIVE TraceLen(_, _)
TraceLen(f, fuel) ==
    IF fuel = 0 THEN 0
    ELSE IF f \notin DOMAIN pred THEN 0
    ELSE IF pred[f].from = None THEN 1
    ELSE 1 + TraceLen(pred[f].from, fuel - 1)

(***************************************************************************)
(* Invariants (evaluated when pc = "done")                                 *)
(***************************************************************************)
ReachSound == pc = "done" => reachable = ExpectedReachable

ImportedBy(f) == Static[f] \cup Dynamic[f]

ParentReached == pc = "done" =>
    \A m \in PluginRefs \cup UNION {ImportedBy(f) : f \in reachable} :
        \A a \in Ancestors(m) : Resolve[a] # None => Resolve[a] \in reachable

FrameworkFollowed == pc = "done" =>
    \A f \in Framework : Adj(f) \subseteq reachable

EntryTraceKept == pc = "done" =>
    \A f \in DOMAIN entryPred : pred[f] = entryPred[f]

(* One FileReachesFile edge per (from, to) pair, whatever the `via` tag. *)
NoDuplicateEdge == pc = "done" =>
    \A e1, e2 \in DOMAIN edges :
        (e1.from = e2.from /\ e1.to = e2.to) => e1 = e2 /\ edges[e1] = 1

OnlyDynamicPath(t) ==
    /\ t \notin Roots \cup PluginTargets \cup Framework
    /\ ~\E f \in reachable : \E m \in Static[f] : t \in TargetSet(m)
    /\ \E f \in reachable : \E m \in Dynamic[f] : t \in TargetSet(m)

ViaFaithful == pc = "done" =>
    \A t \in reachable : OnlyDynamicPath(t) => pred[t].step = "DynamicImport"

TraceNonEmpty == pc = "done" =>
    \A t \in reachable : TraceLen(t, Cardinality(Files) + 1) >= 1

Terminates == <>(pc = "done")

=============================================================================
