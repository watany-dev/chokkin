--------------------------- MODULE MCReachability ---------------------------
(* Concrete instance: main.py (root) imports a.py statically and b.py via  *)
(* importlib.import_module; b.py imports c.py via importlib only.  d.py is *)
(* framework/glob-used and imported by nobody.                             *)
EXTENDS Reachability

CONSTANTS m, a, b, c, d

MCFiles == {m, a, b, c, d}
MCRoots == {m}
MCFramework == {d}
MCStatic == [f \in MCFiles |-> IF f = m THEN {a} ELSE {}]
MCDynamic == [f \in MCFiles |-> IF f = m THEN {b} ELSE IF f = b THEN {c} ELSE {}]

=============================================================================
