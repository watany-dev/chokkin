--------------------------- MODULE MCReachability ---------------------------
(* Concrete instance (file = module in parentheses):                       *)
(*   main.py (main, root)   import pkg.sub.c; import django;               *)
(*                          importlib.import_module("b") and, on the same  *)
(*                          line as the static import, "pkg.sub.c";        *)
(*                          a function-local `import lazy`                 *)
(*   b.py (b)               importlib.import_module("pkg.a")               *)
(*   pkg/__init__.py (pkg), pkg/sub/__init__.py (pkg.sub),                 *)
(*   pkg/sub/c.py (pkg.sub.c)   `try: import pkg.opt`                      *)
(*   pkg/a.py (pkg.a), pkg/x.py (pkg.x), pkg/opt.py (pkg.opt),             *)
(*   pkg/y.py (pkg.y)                                                      *)
(*   pkg/pub/__init__.py (pkg.pub, root through `library subpackage`)      *)
(*                          import pkg.w                                   *)
(*   pkg/w.py (pkg.w)                                                      *)
(*   migrations/0001.py (mig, framework glob)   import models              *)
(*   models.py (models)     import lazy                                    *)
(*   lazy.py (lazy)         import deep                                    *)
(*   deep.py (deep)                                                        *)
(*   tests/test_a.py (test, root, test context)                            *)
(*                          import pkg.y; import pkg; import z             *)
(*   z.py (z)               imported only by the test                      *)
(* pkg is a library; pkg.a is also a framework-glob match, already reached *)
(* by the entry walk; pkg.x is named only by its own decorator's plugin    *)
(* module_ref; django is third-party (resolves to no file).  lazy.py is   *)
(* first reached through main's lazy import and upgraded to certain once  *)
(* the framework walk reaches models.py, so deep.py is upgraded through a *)
(* second follow of lazy.py.                                               *)
EXTENDS Reachability

CONSTANTS fMain, fB, fPkg, fSub, fC, fA, fX, fOpt, fY, fPub, fW,
          fMig, fModels, fLazy, fDeep, fTest, fZ,
          mMain, mB, mPkg, mSub, mC, mA, mX, mOpt, mY, mPub, mW,
          mMig, mModels, mLazy, mDeep, mTest, mZ, mDjango

MCFiles == {fMain, fB, fPkg, fSub, fC, fA, fX, fOpt, fY, fPub, fW,
            fMig, fModels, fLazy, fDeep, fTest, fZ}
MCModules == {mMain, mB, mPkg, mSub, mC, mA, mX, mOpt, mY, mPub, mW,
              mMig, mModels, mLazy, mDeep, mTest, mZ, mDjango}

MCResolve ==
    (mMain :> fMain) @@ (mB :> fB) @@ (mPkg :> fPkg) @@ (mSub :> fSub)
    @@ (mC :> fC) @@ (mA :> fA) @@ (mX :> fX) @@ (mOpt :> fOpt)
    @@ (mY :> fY) @@ (mPub :> fPub) @@ (mW :> fW) @@ (mMig :> fMig)
    @@ (mModels :> fModels) @@ (mLazy :> fLazy) @@ (mDeep :> fDeep)
    @@ (mTest :> fTest) @@ (mZ :> fZ) @@ (mDjango :> None)

MCParent ==
    [m \in MCModules |->
        CASE m = mSub -> mPkg
          [] m = mC -> mSub
          [] m \in {mA, mX, mOpt, mY, mPub, mW} -> mPkg
          [] OTHER -> None]

MCRoots == {fMain, fPub, fTest}
MCPublicRoots == {fPub}
MCPluginRefs == {mX}
MCSelfRegistered == {mX}
MCFramework == {fMig, fA}

MCStatic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mC, mDjango, mLazy}
          [] f = fC -> {mOpt}
          [] f = fPub -> {mW}
          [] f = fMig -> {mModels}
          [] f = fModels -> {mLazy}
          [] f = fLazy -> {mDeep}
          [] f = fTest -> {mY, mPkg, mZ}
          [] OTHER -> {}]

MCDynamic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mB, mC}
          [] f = fB -> {mA}
          [] OTHER -> {}]

MCLazy ==
    [f \in MCFiles |-> IF f = fMain THEN {mLazy} ELSE {}]

MCOptional ==
    [f \in MCFiles |-> IF f = fC THEN {mOpt} ELSE {}]

MCNonRuntime == {fTest}
MCLibrary == {fPkg, fSub, fC, fA, fX, fOpt, fY, fPub, fW}
MCSubmodule == MCLibrary \ {fPkg}

(* The classes the walk must end with, written out by hand so the instance *)
(* exercises every cap: Expected* must give these too.                     *)
MCExpectedClass ==
    [f \in MCFiles |->
        CASE f \in {fZ} -> Reachable
          [] f \in {fOpt} -> Eager
          [] f \in {fX, fY, fPub, fW} -> Exposed
          [] OTHER -> Certain]

MCClassesAsWritten == pc = "done" =>
    /\ DOMAIN reach = MCFiles
    /\ \A f \in MCFiles : reach[f] = MCExpectedClass[f]

(* lazy.py and deep.py are followed twice: first at Reachable, then at    *)
(* Certain.                                                                *)
MCUpgradeFollowed == pc = "done" => follows[fLazy] = 2 /\ follows[fDeep] = 2

=============================================================================
