import Patterns.Fnmatch
/-
setuptools `find_packages` / `find_namespace_packages` (setuptools
documentation, "Package Discovery and Namespace Packages", and
`[tool.setuptools.packages.find]`): walk the directories below `where`;
a directory whose name holds `.` is skipped with everything below it; the
regular finder also skips (with its subtree) a directory without
`__init__.py`; a package is reported when its dotted name matches an
`include` pattern and no `exclude` pattern, with `fnmatchcase`; `ez_setup`
and `*__pycache__` are always excluded. Subpackages are visited even when
the parent is excluded, unless `exclude` literally lists `pkg*` or `pkg.*`.
-/
namespace Patterns.Setuptools

def under (base d : String) : Option String :=
  if base.isEmpty then some d
  else if (base ++ "/").isPrefixOf d then some (d.drop (base.length + 1)) else none

def childDirs (dirs : List String) (d : String) : List String :=
  dirs.filter fun c =>
    match under d c with
    | some r => !r.isEmpty && !r.contains '/'
    | none => false

def baseName (d : String) : String := (d.splitOn "/").getLast!

partial def walk (dirs inits : List String) (where_ : String) (inc exc : List String)
    (ns : Bool) (d : String) : List String :=
  (childDirs dirs d).flatMap fun c =>
    if (baseName c).contains '.' || (!ns && !inits.contains c) then []
    else
      let rel := (under where_ c).getD c
      let pkg := rel.replace "/" "."
      let here := if inc.any (Fnmatch.fnmatchcase · pkg) && !exc.any (Fnmatch.fnmatchcase · pkg)
        then [pkg] else []
      if exc.contains (pkg ++ "*") || exc.contains (pkg ++ ".*") then here
      else here ++ walk dirs inits where_ inc exc ns c

def insertSorted (x : String) : List String → List String
  | [] => [x]
  | y :: ys => if x ≤ y then x :: y :: ys else y :: insertSorted x ys

def find (dirs inits : List String) (where_ : String) (inc exc : List String) (ns : Bool) :
    List String :=
  let exc := ["ez_setup", "*__pycache__"] ++ exc
  (walk dirs inits where_ inc exc ns where_).foldr insertSorted []

/-- `include = ["acme"]` does not take `acme.sub`: patterns name packages,
not trees. -/
example : find ["acme", "acme/sub"] ["acme", "acme/sub"] "" ["acme"] [] false = ["acme"] := by
  native_decide
example : find ["acme", "acme/internal"] [] "" ["acme*"] ["acme.{internal}*"] true
    = ["acme", "acme.internal"] := by native_decide

end Patterns.Setuptools
