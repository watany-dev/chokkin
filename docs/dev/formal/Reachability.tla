---------------------------- MODULE Reachability ----------------------------
(***************************************************************************)
(* Model of `run_reachability_bfs` (src/reachability/bfs.rs) together with *)
(* the edge construction in `add_parsed_imports` (src/graph/edges.rs).     *)
(*                                                                         *)
(* Files import modules either statically (`parsed.imports`) or            *)
(* dynamically (`parsed.dynamic_imports`).  `add_parsed_imports` pushes a  *)
(* `FileImportsModule` edge for both kinds, carrying a `dynamic` flag, and *)
(* `record_file_imports` walks that adjacency exactly once, tagging each   *)
(* site `Import` or `DynamicImport` from the flag.  Edge order puts a      *)
(* file's static sites before its dynamic ones.                            *)
(*                                                                         *)
(* A walk that resolves to a first-party file pushes one                   *)
(* `FileReachesFile{from,to,via}` edge per (from, to) pair, and            *)
(* `enqueue_file` records the predecessor only for the first arrival.      *)
(*                                                                         *)
(* Checked properties                                                      *)
(*   ReachSound      : reachable = transitive closure from roots           *)
(*   NoDuplicateEdge : FileReachesFile edges form a set (no duplicates)    *)
(*   ViaFaithful     : a file reached only through a dynamic import has a  *)
(*                     predecessor step tagged DynamicImport               *)
(*   TraceNonEmpty   : trace_to_file yields >= 1 step for every file in    *)
(*                     the final reachable set (bfs ∪ framework_used)      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS Files,         \* set of first-party files
          Roots,         \* entry roots (subset of Files)
          Static,        \* Static[f]  : files imported statically by f
          Dynamic,       \* Dynamic[f] : files imported via importlib by f
          Framework,     \* files marked used by glob/framework plugin
          None           \* model value for "no predecessor"

ASSUME Roots \subseteq Files
ASSUME Framework \subseteq Files

RECURSIVE SetToSeq(_)
SetToSeq(S) ==
    IF S = {} THEN <<>>
    ELSE LET x == CHOOSE x \in S : TRUE IN <<x>> \o SetToSeq(S \ {x})

(* FileImportsModule adjacency as built by add_parsed_imports: both kinds, *)
(* static sites first, each dynamic-only site tagged `dynamic`.            *)
Adj(f) == Static[f] \cup Dynamic[f]
DynamicOnly(f) == Dynamic[f] \ Static[f]

VARIABLES queue,       \* sequence of files
          reachable,   \* set of files
          pred,        \* partial function file -> [from, step]
          edges,       \* bag (function to Nat) of FileReachesFile records
          pc

vars == <<queue, reachable, pred, edges, pc>>

EdgeRec(f, t, v) == [from |-> f, to |-> t, via |-> v]

(* push_edge guarded by the (from, to) dedupe set in BfsState. *)
AddEdge(bag, e) ==
    IF \E d \in DOMAIN bag : d.from = e.from /\ d.to = e.to THEN bag
    ELSE bag @@ (e :> 1)

(* enqueue_file: only the first arrival is recorded *)
Enqueue(q, r, p, target, from, step) ==
    IF target \in r THEN <<q, r, p>>
    ELSE << Append(q, target),
            r \cup {target},
            p @@ (target :> [from |-> from, step |-> step]) >>

(* enqueue_resolved_module over a list of targets: edge push, then enqueue *)
RECURSIVE Walk(_, _, _, _, _, _, _, _)
Walk(f, ts, i, via, q, r, p, bag) ==
    IF i > Len(ts) THEN <<q, r, p, bag>>
    ELSE LET t == ts[i]
             bag2 == IF t # f THEN AddEdge(bag, EdgeRec(f, t, via)) ELSE bag
             e == Enqueue(q, r, p, t, f, via)
         IN Walk(f, ts, i + 1, via, e[1], e[2], e[3], bag2)

Init ==
    /\ queue = SetToSeq(Roots)
    /\ reachable = Roots
    /\ pred = [r \in Roots |-> [from |-> None, step |-> "Root"]]
    /\ edges = [e \in {} |-> 0]
    /\ pc = "loop"

Step ==
    /\ pc = "loop"
    /\ queue # <<>>
    /\ LET f == Head(queue)
           s1 == Walk(f, SetToSeq(Static[f]), 1, "Import", Tail(queue), reachable, pred, edges)
           s2 == Walk(f, SetToSeq(DynamicOnly(f)), 1, "DynamicImport", s1[1], s1[2], s1[3], s1[4])
       IN /\ queue' = s2[1]
          /\ reachable' = s2[2]
          /\ pred' = s2[3]
          /\ edges' = s2[4]
          /\ pc' = "loop"

(* apply_framework_globs records a PluginRef predecessor for every glob   *)
(* match, so a file only the globs reach still has a trace step.  `@@`     *)
(* keeps the BFS entry when the file was already reached by an import.    *)
Done ==
    /\ pc = "loop"
    /\ queue = <<>>
    /\ pred' = pred @@ [f \in Framework |-> [from |-> None, step |-> "PluginRef"]]
    /\ pc' = "done"
    /\ UNCHANGED <<queue, reachable, edges>>

Next == Step \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Reference semantics                                                     *)
(***************************************************************************)
RECURSIVE Closure(_)
Closure(S) ==
    LET nxt == S \cup UNION {Adj(f) : f \in S}
    IN IF nxt = S THEN S ELSE Closure(nxt)

ExpectedReachable == Closure(Roots)

(* Final reachable set = bfs ∪ framework_used (src/reachability/build.rs) *)
FinalReachable == reachable \cup Framework

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

(* One FileReachesFile edge per (from, to) pair, whatever the `via` tag. *)
NoDuplicateEdge == pc = "done" =>
    \A e1, e2 \in DOMAIN edges :
        (e1.from = e2.from /\ e1.to = e2.to) => e1 = e2 /\ edges[e1] = 1

OnlyDynamicPath(t) ==
    /\ t \notin Roots
    /\ ~\E f \in reachable : t \in Static[f]
    /\ \E f \in reachable : t \in Dynamic[f]

ViaFaithful == pc = "done" =>
    \A t \in reachable : OnlyDynamicPath(t) => pred[t].step = "DynamicImport"

TraceNonEmpty == pc = "done" =>
    \A t \in FinalReachable : TraceLen(t, Cardinality(Files) + 1) >= 1

=============================================================================
