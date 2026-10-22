--------------------------- MODULE MCReachability ---------------------------
(* Concrete instance (file = module in parentheses):                       *)
(*   main.py (main, root)   import pkg.sub.c; import django;               *)
(*                          importlib.import_module("b") and, on the same  *)
(*                          line as the static import, "pkg.sub.c";        *)
(*                          a function-local `import lazy`; import tool    *)
(*   b.py (b)               importlib.import_module("pkg.a");              *)
(*                          importlib.import_module("plugins." + name)     *)
(*   scripts/tool.py (tool) beside main's script directory only            *)
(*                          (`Fallback` from main.py)                      *)
(*   plugins/__init__.py (plugins), plugins/one.py (plugins.one)           *)
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
(*   deep.py (deep)         import helpers (resolves to nothing from here) *)
(*   tests/test_a.py (test, root, test context)                            *)
(*                          import pkg.y; import pkg; import z;            *)
(*                          import helpers                                 *)
(*   z.py (z)               imported only by the test                      *)
(*   tests/helpers.py (helpers) on pytest's sys.path for the test only     *)
(* pkg is a library; pkg.a is also a framework-glob match, already reached *)
(* by the entry walk; pkg.x is named only by its own decorator's plugin    *)
(* module_ref (its own file, so self-registered); django is third-party   *)
(* (resolves to no file).  helpers is reached only from the test, so it   *)
(* stays Reachable although deep.py, which is certain, names it too.       *)
(* plugins/__init__.py is reached only as the package of b's prefixed     *)
(* import.  lazy.py is first reached through main's lazy import and        *)
(* upgraded to certain once the framework walk reaches models.py, so      *)
(* deep.py is upgraded through a second follow of lazy.py.                 *)
EXTENDS Reachability

CONSTANTS fMain, fB, fPkg, fSub, fC, fA, fX, fOpt, fY, fPub, fW,
          fMig, fModels, fLazy, fDeep, fTest, fZ, fHelpers, fTool, fPlugins, fOne,
          mMain, mB, mPkg, mSub, mC, mA, mX, mOpt, mY, mPub, mW,
          mMig, mModels, mLazy, mDeep, mTest, mZ, mDjango, mHelpers, mTool,
          mPlugins, mOne

MCFiles == {fMain, fB, fPkg, fSub, fC, fA, fX, fOpt, fY, fPub, fW,
            fMig, fModels, fLazy, fDeep, fTest, fZ, fHelpers, fTool, fPlugins, fOne}
MCModules == {mMain, mB, mPkg, mSub, mC, mA, mX, mOpt, mY, mPub, mW,
              mMig, mModels, mLazy, mDeep, mTest, mZ, mDjango, mHelpers, mTool,
              mPlugins, mOne}

MCIndex ==
    (mMain :> fMain) @@ (mB :> fB) @@ (mPkg :> fPkg) @@ (mSub :> fSub)
    @@ (mC :> fC) @@ (mA :> fA) @@ (mX :> fX) @@ (mOpt :> fOpt)
    @@ (mY :> fY) @@ (mPub :> fPub) @@ (mW :> fW) @@ (mMig :> fMig)
    @@ (mModels :> fModels) @@ (mLazy :> fLazy) @@ (mDeep :> fDeep)
    @@ (mTest :> fTest) @@ (mZ :> fZ) @@ (mDjango :> None)
    @@ (mHelpers :> None) @@ (mTool :> None)
    @@ (mPlugins :> fPlugins) @@ (mOne :> fOne)

MCPytest ==
    [i \in MCFiles |-> [m \in MCModules |->
        IF i = fTest /\ m = mHelpers THEN fHelpers ELSE None]]

MCFallback ==
    [i \in MCFiles |-> [m \in MCModules |->
        IF i = fMain /\ m = mTool THEN fTool ELSE None]]

MCParent ==
    [m \in MCModules |->
        CASE m = mSub -> mPkg
          [] m = mC -> mSub
          [] m \in {mA, mX, mOpt, mY, mPub, mW} -> mPkg
          [] m = mOne -> mPlugins
          [] OTHER -> None]

MCRoots == {fMain, fPub, fTest}
MCPublicRoots == {fPub}
MCPluginRefs == {mX}
MCRefOrigin == [m \in MCPluginRefs |-> fX]
MCFramework == {fMig, fA}

MCStatic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mC, mDjango, mLazy, mTool}
          [] f = fC -> {mOpt}
          [] f = fPub -> {mW}
          [] f = fMig -> {mModels}
          [] f = fModels -> {mLazy}
          [] f = fLazy -> {mDeep}
          [] f = fDeep -> {mHelpers}
          [] f = fTest -> {mY, mPkg, mZ, mHelpers}
          [] OTHER -> {}]

MCDynamic ==
    [f \in MCFiles |->
        CASE f = fMain -> {mB, mC}
          [] f = fB -> {mA}
          [] OTHER -> {}]

MCPrefix ==
    [f \in MCFiles |-> IF f = fB THEN {mPlugins} ELSE {}]

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
        CASE f \in {fZ, fHelpers} -> Reachable
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
