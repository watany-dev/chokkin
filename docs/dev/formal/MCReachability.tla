--------------------------- MODULE MCReachability ---------------------------
(* Concrete instance (file = module in parentheses):                       *)
(*   main.py (main, root)   import pkg.sub.c; import django;               *)
(*                          importlib.import_module("b") and, on the same  *)
(*                          line as the static import, "pkg.sub.c"         *)
(*   b.py (b)               importlib.import_module("pkg.a")               *)
(*   pkg/__init__.py (pkg), pkg/sub/__init__.py (pkg.sub),                 *)
(*   pkg/sub/c.py (pkg.sub.c), pkg/a.py (pkg.a), pkg/x.py (pkg.x)          *)
(*   migrations/0001.py (mig, framework glob)   import models              *)
(*   models.py (models)     imported only by the migration                 *)
(* pkg.a is also a framework-glob match, already reached by the entry     *)
(* walk; pkg.x is named only by a plugin module_ref; django is third-party *)
(* (resolves to no file).                                                  *)
EXTENDS Reachability

CONSTANTS fMain, fB, fPkg, fSub, fC, fA, fX, fMig, fModels,
          mMain, mB, mPkg, mSub, mC, mA, mX, mMig, mModels, mDjango

MCFiles == {fMain, fB, fPkg, fSub, fC, fA, fX, fMig, fModels}
MCModules == {mMain, mB, mPkg, mSub, mC, mA, mX, mMig, mModels, mDjango}

MCResolve ==
    (mMain :> fMain) @@ (mB :> fB) @@ (mPkg :> fPkg) @@ (mSub :> fSub)
    @@ (mC :> fC) @@ (mA :> fA) @@ (mX :> fX) @@ (mMig :> fMig)
    @@ (mModels :> fModels) @@ (mDjango :> None)

MCParent ==
    [m \in MCModules |->
        CASE m = mSub -> mPkg
          [] m = mC -> mSub
          [] m \in {mA, mX} -> mPkg
          [] OTHER -> None]

MCRoots == {fMain}
MCPluginRefs == {mX}
MCFramework == {fMig, fA}

MCStatic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mC, mDjango}
          [] f = fMig -> {mModels}
          [] OTHER -> {}]

MCDynamic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mB, mC}
          [] f = fB -> {mA}
          [] OTHER -> {}]

=============================================================================
