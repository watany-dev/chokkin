---------------------------- MODULE Reachability ----------------------------
(***************************************************************************)
(* Model of `run_reachability_bfs` (src/reachability/bfs.rs) together with *)
(* the edge construction in `add_parsed_imports` (src/graph/edges.rs) and  *)
(* the framework-glob seeding in `analyze_reachability`                    *)
(* (src/reachability/build.rs).                                            *)
(*                                                                         *)
(* Files import *modules*, statically (`parsed.imports`) or dynamically    *)
(* (`parsed.dynamic_imports`).  A module resolves to at most one           *)
(* first-party file, depending on the importer                             *)
(* (`ModuleIndex::resolve_from`): pytest's `sys.path` entries for a test   *)
(* file first (`Pytest`), then the project's module index (`Index`; None   *)
(* for stdlib/third-party), then a module beside a script or in a          *)
(* directory a `sys.path` edit adds (`Fallback`).                          *)
(* A site that imports `pkg.sub.mod` also reaches the files of `pkg` and   *)
(* `pkg.sub` (`module_and_parents`: the module itself, then each parent    *)
(* package outermost first), because Python initialises the parent         *)
(* packages first.  A prefixed dynamic site (`import_module("pkg.sub." +   *)
(* name)`, `dynamic_import_prefixes`) reaches `pkg.sub` and its parents    *)
(* the same way, then every module the index has strictly under `pkg.sub`  *)
(* (`ModuleIndex::under`, which ignores the importer).                     *)
(* A dynamic site that shares its line with a static      *)
(* import of the same module is static (`build_dynamic_sites`); at module *)
(* granularity that is `Dynamic[f] \ Static[f]`, and static sites come     *)
(* first in the adjacency.  A file has one site per module, so the lazy    *)
(* and optional flags of `conditional_lines` are per (file, module).       *)
(*                                                                         *)
(* Each reached file carries a class (`Reach`): 0 Reachable, 1 Eager,      *)
(* 2 Exposed, 3 Certain.  An edge passes on its source's class, capped by  *)
(* `edge_class`: a lazy site (function-local or `TYPE_CHECKING`) gives 0;  *)
(* a site in a test, docs or dev file gives 0 into a file that is not in  *)
(* library mode and at most 2 into a library submodule; an optional site  *)
(* (`try`, `suppress(ImportError)`, platform guard) gives at most 1.  A    *)
(* file reached again at a better class is queued again, and `drain`       *)
(* follows its imports again only when its class beat the one they were   *)
(* last followed at.                                                       *)
(*                                                                         *)
(* Seeds: entry roots (`Root` step; 2 for a root only a library           *)
(* subpackage's public surface makes, else 3), then every module/parent of *)
(* a plugin `module_ref` that resolves from the ref's origin (`PluginRef`  *)
(* step; 2 when the module resolves to the library-mode file the ref sits  *)
(* in, its own decorator, else 3).  After that queue drains,               *)
(* framework-glob files are enqueued at 3 and the queue drains again, so   *)
(* their imports are followed and files that the entry walk already        *)
(* reached keep their trace.  `enqueue_file` records the predecessor only  *)
(* for the first arrival.                                                  *)
(*                                                                         *)
(* Checked properties                                                      *)
(*   ReachSound      : reachable = closure of the seeds over module        *)
(*                     imports, parent packages included                   *)
(*   ClassSound      : eager / exposed / certain each equal their own BFS *)
(*                     from §6 of the spec (edges a test file may not      *)
(*                     follow, lazy and optional sites dropped), so the    *)
(*                     single walk computes all three sets                 *)
(*   SetsNested      : by those definitions,                               *)
(*                     certain <= exposed <= eager <= reachable            *)
(*   ParentReached   : every resolvable parent package of a module that a  *)
(*                     reachable file or plugin ref imports is reachable   *)
(*   ClassPassedOn   : every import site, parent packages included, and    *)
(*                     every plugin ref passes its class on, so each       *)
(*                     derived set keeps ParentReached as well             *)
(*   FrameworkFollowed : framework-glob files are certain, and the files   *)
(*                     they import get the class their sites pass on       *)
(*   FollowedAtMostFour : no file's imports are followed more than once   *)
(*                     per class                                           *)
(*   EntryTraceKept  : framework seeding never overwrites a predecessor    *)
(*                     recorded by the entry walk                          *)
(*   ViaFaithful     : a file reached only through dynamic imports has a   *)
(*                     predecessor step tagged DynamicImport               *)
(*   TraceNonEmpty   : trace_to_file yields >= 1 step for every reachable  *)
(*                     file                                                *)
(*   Terminates      : both drains finish (so the invariants above are not *)
(*                     vacuous)                                            *)
(***************************************************************************)
EXTENDS Naturals, Integers, Sequences, FiniteSets, TLC

CONSTANTS Files,         \* set of first-party files
          Importers,     \* paths a module is resolved from: Files and the
                         \* non-source files a plugin ref sits in
          Modules,       \* set of module names
          Index,         \* Index[m]   : file of module m in the index, or None
          Pytest,        \* Pytest[i][m]: file pytest's sys.path gives i, or None
          Fallback,      \* Fallback[i][m]: file beside i / on a sys.path hint, or None
          Parent,        \* Parent[m]  : enclosing package of m, or None
          Roots,         \* entry roots (subset of Files)
          PublicRoots,   \* roots only a library subpackage's surface makes
          Static,        \* Static[f]  : modules imported statically by f
          Dynamic,       \* Dynamic[f] : modules imported via importlib by f
          Prefix,        \* Prefix[f]  : packages f imports a submodule of by a
                         \*              computed name (`"pkg." + name`)
          Lazy,          \* Lazy[f]    : of those, function-local / TYPE_CHECKING
          Optional,      \* Optional[f]: of those, try / suppress / platform guard
          NonRuntime,    \* files in a test, docs or dev context
          Library,       \* files in library mode (`EntryPlan::mode_for`)
          Submodule,     \* files under a runtime `__init__.py` (`is_submodule`)
          PluginRefs,    \* modules named by plugin module_refs
          RefOrigin,     \* RefOrigin[m]: the importer the ref of m sits in
          Framework,     \* files matched by framework globs
          None           \* model value for "no file / no predecessor"

ASSUME Roots \subseteq Files
ASSUME PublicRoots \subseteq Roots
ASSUME Framework \subseteq Files
ASSUME Files \subseteq Importers
ASSUME PluginRefs \subseteq Modules
ASSUME \A m \in PluginRefs : RefOrigin[m] \in Importers
ASSUME NonRuntime \cup Library \cup Submodule \subseteq Files
ASSUME \A m \in Modules : Index[m] \in Files \cup {None}
ASSUME \A i \in Importers : \A m \in Modules :
           Pytest[i][m] \in Files \cup {None} /\ Fallback[i][m] \in Files \cup {None}
ASSUME \A m \in Modules : Parent[m] \in Modules \cup {None}
ASSUME \A f \in Files : Prefix[f] \subseteq Modules
ASSUME \A f \in Files :
           Lazy[f] \cup Optional[f] \subseteq Static[f] \cup Dynamic[f] \cup Prefix[f]

Reachable == 0
Eager == 1
Exposed == 2
Certain == 3

Min(a, b) == IF a < b THEN a ELSE b

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

(* `ModuleIndex::resolve_from` *)
ResolveFrom(i, m) ==
    IF Pytest[i][m] # None THEN Pytest[i][m]
    ELSE IF Index[m] # None THEN Index[m]
    ELSE Fallback[i][m]

(* Files reached by one site of i importing m, in visiting order. *)
RECURSIVE ResolvedSeq(_, _)
ResolvedSeq(i, s) ==
    IF s = <<>> THEN <<>>
    ELSE (IF ResolveFrom(i, Head(s)) = None THEN <<>> ELSE <<ResolveFrom(i, Head(s))>>)
         \o ResolvedSeq(i, Tail(s))

Targets(i, m) == ResolvedSeq(i, Chain(m))
SeqSet(s) == {s[k] : k \in 1..Len(s)}
TargetSet(i, m) == SeqSet(Targets(i, m))

(* `ModuleIndex::under`: the index's modules strictly inside package p. *)
UnderSeq(p) == LET ms == SetToSeq({m \in Modules : p \in Ancestors(m) /\ Index[m] # None})
               IN [k \in 1..Len(ms) |-> Index[ms[k]]]

(* `build_prefix_imports`: p and its parents, then everything under p. *)
PrefixTargets(f, p) == Targets(f, p) \o UnderSeq(p)

DynamicOnly(f) == Dynamic[f] \ Static[f]

ImportedBy(f) == Static[f] \cup Dynamic[f]

(* Every <<module, file>> pair one file's sites reach; the module is the  *)
(* site's (for a prefixed site, the package), which carries its flags.     *)
SiteTargets(f) ==
    UNION {{<<m, t>> : t \in TargetSet(f, m)} : m \in ImportedBy(f)}
    \cup UNION {{<<p, t>> : t \in SeqSet(PrefixTargets(f, p))} : p \in Prefix[f]}

RefTargets(m) == Targets(RefOrigin[m], m)

PluginTargets == UNION {SeqSet(RefTargets(m)) : m \in PluginRefs}

(* A library module's own decorators register it only once some caller   *)
(* imports it (#802).                                                      *)
SelfRegistered ==
    {m \in PluginRefs : /\ RefOrigin[m] \in Library
                        /\ ResolveFrom(RefOrigin[m], m) = RefOrigin[m]}

(* `edge_class`: what the site of f importing m passes on to t when f is   *)
(* followed at class c.                                                    *)
EdgeClass(f, c, m, t) ==
    IF c = Reachable \/ m \in Lazy[f] THEN Reachable
    ELSE LET c1 == IF f \notin NonRuntime THEN c
                   ELSE IF t \notin Library THEN Reachable
                   ELSE IF t \in Submodule THEN Min(c, Exposed)
                   ELSE c
         IN IF c1 # Reachable /\ m \in Optional[f] THEN Min(c1, Eager) ELSE c1

RootClass(f) == IF f \in PublicRoots THEN Exposed ELSE Certain
RefClass(m) == IF m \in SelfRegistered THEN Exposed ELSE Certain

VARIABLES queue,       \* sequence of files
          reach,       \* function file -> class; its domain is `reachable`
          followed,    \* function file -> class its imports were followed at
          follows,     \* function file -> how often its imports were followed
          pred,        \* partial function file -> [from, step]
          entryPred,   \* history: pred when the entry walk finished
          pc

vars == <<queue, reach, followed, follows, pred, entryPred, pc>>

(* enqueue_file: only the first arrival is recorded; a better class        *)
(* queues the file again.                                                  *)
Enqueue(q, r, p, target, from, step, c) ==
    IF target \in DOMAIN r
    THEN IF c > r[target]
         THEN <<Append(q, target), [r EXCEPT ![target] = c], p>>
         ELSE <<q, r, p>>
    ELSE << Append(q, target),
            r @@ (target :> c),
            p @@ (target :> [from |-> from, step |-> step]) >>

(* Op(v), with v evaluated once.  TLC does not cache LET definitions or    *)
(* operator arguments while it evaluates ENABLED (for WF_vars(Next)), so a *)
(* recursion that passes e[1], e[2], ... of a LET re-evaluates the whole   *)
(* previous level at each use, and checking Terminates took ~7 min (#853). *)
(* The bound variable x is a value, so each level is evaluated once.       *)
Bind(v, Op(_)) == CHOOSE y \in {Op(x) : x \in {v}} : TRUE

(* Seed a list of [file, class] records with no predecessor. *)
RECURSIVE Seed(_, _, _, _, _, _)
Seed(ts, i, step, q, r, p) ==
    IF i > Len(ts) THEN <<q, r, p>>
    ELSE Bind(Enqueue(q, r, p, ts[i].file, None, step, ts[i].class),
              LAMBDA e : Seed(ts, i + 1, step, e[1], e[2], e[3]))

(* enqueue_import over the files of one site *)
RECURSIVE Walk(_, _, _, _, _, _, _, _, _)
Walk(f, c, m, ts, i, via, q, r, p) ==
    IF i > Len(ts) THEN <<q, r, p>>
    ELSE Bind(Enqueue(q, r, p, ts[i], f, via, EdgeClass(f, c, m, ts[i])),
              LAMBDA e : Walk(f, c, m, ts, i + 1, via, e[1], e[2], e[3]))

SiteSeq(f, m, prefixed) == IF prefixed THEN PrefixTargets(f, m) ELSE Targets(f, m)

(* record_file_imports over every site of one kind *)
RECURSIVE Sites(_, _, _, _, _, _, _, _, _)
Sites(f, c, ms, i, via, prefixed, q, r, p) ==
    IF i > Len(ms) THEN <<q, r, p>>
    ELSE Bind(Walk(f, c, ms[i], SiteSeq(f, ms[i], prefixed), 1, via, q, r, p),
              LAMBDA s : Sites(f, c, ms, i + 1, via, prefixed, s[1], s[2], s[3]))

RECURSIVE RefSeeds(_)
RefSeeds(ms) ==
    IF ms = <<>> THEN <<>>
    ELSE [i \in 1..Len(RefTargets(Head(ms))) |->
             [file |-> RefTargets(Head(ms))[i], class |-> RefClass(Head(ms))]]
         \o RefSeeds(Tail(ms))

RootSeeds == LET rs == SetToSeq(Roots)
             IN [i \in 1..Len(rs) |-> [file |-> rs[i], class |-> RootClass(rs[i])]]

FrameworkSeeds == LET fs == SetToSeq(Framework)
                  IN [i \in 1..Len(fs) |-> [file |-> fs[i], class |-> Certain]]

Init ==
    LET s0 == Seed(RootSeeds, 1, "Root", <<>>, [x \in {} |-> x], [x \in {} |-> x])
        s1 == Seed(RefSeeds(SetToSeq(PluginRefs)), 1, "PluginRef", s0[1], s0[2], s0[3])
    IN /\ queue = s1[1]
       /\ reach = s1[2]
       /\ pred = s1[3]
       /\ followed = [x \in {} |-> x]
       /\ follows = [x \in Files |-> 0]
       /\ entryPred = [x \in {} |-> x]
       /\ pc = "entry"

(* `drain`: a file whose imports were already followed at its class is    *)
(* only popped.                                                            *)
Skip ==
    /\ pc \in {"entry", "framework"}
    /\ queue # <<>>
    /\ Head(queue) \in DOMAIN followed
    /\ followed[Head(queue)] >= reach[Head(queue)]
    /\ queue' = Tail(queue)
    /\ UNCHANGED <<reach, followed, follows, pred, entryPred, pc>>

Step ==
    /\ pc \in {"entry", "framework"}
    /\ queue # <<>>
    /\ LET f == Head(queue)
           c == reach[f]
       IN /\ ~(f \in DOMAIN followed /\ followed[f] >= c)
          /\ LET s1 == Sites(f, c, SetToSeq(Static[f]), 1, "Import", FALSE, Tail(queue), reach, pred)
                 s2 == Bind(s1, LAMBDA e : Sites(f, c, SetToSeq(DynamicOnly(f)), 1, "DynamicImport",
                                                FALSE, e[1], e[2], e[3]))
                 s3 == Bind(s2, LAMBDA e : Sites(f, c, SetToSeq(Prefix[f]), 1, "DynamicImport",
                                                TRUE, e[1], e[2], e[3]))
             IN /\ queue' = s3[1]
                /\ reach' = s3[2]
                /\ pred' = s3[3]
          /\ followed' = [x \in DOMAIN followed \cup {f} |->
                             IF x = f THEN c ELSE followed[x]]
          /\ follows' = [follows EXCEPT ![f] = @ + 1]
          /\ UNCHANGED <<entryPred, pc>>

(* The entry walk is drained: enqueue the framework-glob files. *)
SeedFramework ==
    /\ pc = "entry"
    /\ queue = <<>>
    /\ LET s == Seed(FrameworkSeeds, 1, "PluginRef", queue, reach, pred)
       IN /\ queue' = s[1]
          /\ reach' = s[2]
          /\ pred' = s[3]
    /\ entryPred' = pred
    /\ pc' = "framework"
    /\ UNCHANGED <<followed, follows>>

Done ==
    /\ pc = "framework"
    /\ queue = <<>>
    /\ pc' = "done"
    /\ UNCHANGED <<queue, reach, followed, follows, pred, entryPred>>

Next == Skip \/ Step \/ SeedFramework \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

(***************************************************************************)
(* Reference semantics: one BFS per set, as §6 of the spec defines them    *)
(***************************************************************************)

(* Whether the BFS for a set at `level` follows the site of f importing m  *)
(* into t.  Level 0 is the plain reachability BFS.                         *)
Follows(level, f, m, t) ==
    /\ level >= Eager =>
           /\ m \notin Lazy[f]
           /\ f \in NonRuntime => t \in Library
    /\ level >= Exposed => m \notin Optional[f]
    /\ level >= Certain => (f \in NonRuntime => t \notin Submodule)

RECURSIVE ClosureAt(_, _)
ClosureAt(level, S) ==
    LET nxt == S \cup {t \in Files : \E f \in S : \E st \in SiteTargets(f) :
                          st[2] = t /\ Follows(level, f, st[1], t)}
    IN IF nxt = S THEN S ELSE ClosureAt(level, nxt)

Seeds == Roots \cup PluginTargets \cup Framework

(* Seeds that show the package or its host loads the file (#802). *)
CertainSeeds ==
    (Roots \ PublicRoots)
    \cup UNION {SeqSet(RefTargets(m)) : m \in PluginRefs \ SelfRegistered}
    \cup Framework

ExpectedReachable == ClosureAt(Reachable, Seeds)
ExpectedEager == ClosureAt(Eager, Seeds)
ExpectedExposed == ClosureAt(Exposed, Seeds)
ExpectedCertain == ClosureAt(Certain, CertainSeeds)

(* `BfsState::finish` *)
AtLeast(level) == {f \in DOMAIN reach : reach[f] >= level}

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
ReachSound == pc = "done" => DOMAIN reach = ExpectedReachable

ClassSound == pc = "done" =>
    /\ AtLeast(Eager) = ExpectedEager
    /\ AtLeast(Exposed) = ExpectedExposed
    /\ AtLeast(Certain) = ExpectedCertain

SetsNested ==
    /\ ExpectedCertain \subseteq ExpectedExposed
    /\ ExpectedExposed \subseteq ExpectedEager
    /\ ExpectedEager \subseteq ExpectedReachable

(* Python imports every enclosing package of a module first, and, for a  *)
(* prefixed site, the package itself, whatever the computed suffix is.     *)
ParentReached == pc = "done" =>
    /\ \A m \in PluginRefs : \A a \in Ancestors(m) :
           ResolveFrom(RefOrigin[m], a) # None => ResolveFrom(RefOrigin[m], a) \in DOMAIN reach
    /\ \A f \in DOMAIN reach :
           /\ \A m \in ImportedBy(f) : \A a \in Ancestors(m) :
                  ResolveFrom(f, a) # None => ResolveFrom(f, a) \in DOMAIN reach
           /\ \A p \in Prefix[f] : \A a \in {p} \cup Ancestors(p) :
                  ResolveFrom(f, a) # None => ResolveFrom(f, a) \in DOMAIN reach

ClassPassedOn == pc = "done" =>
    /\ \A f \in DOMAIN reach : \A st \in SiteTargets(f) :
           st[2] \in DOMAIN reach /\ reach[st[2]] >= EdgeClass(f, reach[f], st[1], st[2])
    /\ \A m \in PluginRefs : \A t \in SeqSet(RefTargets(m)) :
           t \in DOMAIN reach /\ reach[t] >= RefClass(m)

FrameworkFollowed == pc = "done" =>
    \A f \in Framework :
        /\ f \in DOMAIN reach /\ reach[f] = Certain
        /\ \A st \in SiteTargets(f) :
               st[2] \in DOMAIN reach /\ reach[st[2]] >= EdgeClass(f, Certain, st[1], st[2])

FollowedAtMostFour == \A f \in Files : follows[f] <= 4

EntryTraceKept == pc = "done" =>
    \A f \in DOMAIN entryPred : pred[f] = entryPred[f]

OnlyDynamicPath(t) ==
    /\ t \notin Seeds
    /\ ~\E f \in DOMAIN reach : \E m \in Static[f] : t \in TargetSet(f, m)

ViaFaithful == pc = "done" =>
    \A t \in DOMAIN reach : OnlyDynamicPath(t) => pred[t].step = "DynamicImport"

TraceNonEmpty == pc = "done" =>
    \A t \in DOMAIN reach : TraceLen(t, Cardinality(Files) + 1) >= 1

Terminates == <>(pc = "done")

=============================================================================
