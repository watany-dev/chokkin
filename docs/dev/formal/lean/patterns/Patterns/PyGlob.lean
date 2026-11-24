import Patterns.Fnmatch
/-
Python `glob.glob(pattern, recursive=True)` over a fixed tree, relative to
the current directory (Library Reference, `glob` module): the pattern is
split at `/`; a component without `*`, `?` or `[` is looked up literally
(hidden names included); a magic component is matched with
`fnmatch` against the names of a directory, skipping names that start
with `.` unless the component does; `**` alone matches the directory
itself and everything below it (directories only when more components
follow), again skipping hidden names. Braces have no meaning.

This is how pytest expands `testpaths` entries, and how `pathlib`-free
build backends expand path globs.
-/
namespace Patterns.PyGlob

structure Tree where
  files : List String
  dirs : List String

def Tree.isDir (t : Tree) (p : String) : Bool := p.isEmpty || t.dirs.contains p
def Tree.exists (t : Tree) (p : String) : Bool := t.isDir p || t.files.contains p

/-- Entries directly inside `d`. -/
def Tree.children (t : Tree) (d : String) (dirOnly : Bool) : List String :=
  let inside (p : String) : Option String :=
    let rest := if d.isEmpty then some p
      else if (d ++ "/").isPrefixOf p then some (p.drop (d.length + 1)) else none
    rest.bind fun r => if r.isEmpty || r.contains '/' then none else some r
  let ds := t.dirs.filterMap inside
  if dirOnly then ds else ds ++ t.files.filterMap inside

def hasMagic (s : String) : Bool := s.any fun c => c == '*' || c == '?' || c == '['
def hidden (s : String) : Bool := s.startsWith "."

def stripSlashes (s : String) : String :=
  if s.all (· == '/') then s else ⟨(s.toList.reverse.dropWhile (· == '/')).reverse⟩

/-- `os.path.split`. -/
def split (p : String) : String × String :=
  match p.revPosOf '/' with
  | none => ("", p)
  | some i =>
    let head := (p.extract 0 i) ++ "/"
    (stripSlashes head, p.extract (p.next i) p.endPos)

/-- `os.path.join` of two relative parts. -/
def join (a b : String) : String :=
  if a.isEmpty then b else if a.endsWith "/" then a ++ b else a ++ "/" ++ b

def norm (p : String) : String := stripSlashes p

def glob1 (t : Tree) (d pat : String) (dirOnly : Bool) : List String :=
  let names := t.children (norm d) dirOnly
  let names := if hidden pat then names else names.filter (!hidden ·)
  names.filter (Fnmatch.fnmatchcase pat ·)

def glob0 (t : Tree) (d base : String) : List String :=
  if base.isEmpty then (if t.isDir (norm d) && !d.isEmpty then [""] else [])
  else if t.exists (norm (join d base)) then [base] else []

partial def rlist (t : Tree) (d : String) (dirOnly : Bool) : List String :=
  (t.children (norm d) dirOnly).filter (!hidden ·) |>.flatMap fun x =>
    x :: (rlist t (join d x) dirOnly).map (join x ·)

def glob2 (t : Tree) (d : String) (dirOnly : Bool) : List String :=
  (if d.isEmpty || t.isDir (norm d) then [""] else []) ++ rlist t d dirOnly

partial def iglob (t : Tree) (p : String) (dirOnly : Bool) : List String :=
  let (d, base) := split p
  if !hasMagic p then
    if !base.isEmpty then (if t.exists (norm p) then [p] else [])
    else if t.isDir (norm d) && !d.isEmpty then [p] else []
  else if d.isEmpty then
    if base == "**" then glob2 t "" dirOnly else glob1 t "" base dirOnly
  else
    let dirs := if d != p && hasMagic d then iglob t d true else [d]
    dirs.flatMap fun dir =>
      let names :=
        if hasMagic base then
          (if base == "**" then glob2 t dir dirOnly else glob1 t dir base dirOnly)
        else glob0 t dir base
      names.map (join dir ·)

def insertSorted (x : String) : List String → List String
  | [] => [x]
  | y :: ys => if x ≤ y then x :: y :: ys else y :: insertSorted x ys

def glob (files dirs : List String) (pat : String) : List String :=
  let t : Tree := { files, dirs }
  let r := iglob t pat false
  -- `glob.iglob` drops the leading "" a top-level `**` yields
  let r := if pat.startsWith "**" then r.filter (!·.isEmpty) else r
  (r.eraseDups).foldr insertSorted []

example : glob ["a/t/x.py", "b/t/y.py"] ["a", "a/t", "b", "b/t"] "*/t" = ["a/t", "b/t"] := by
  native_decide
example : glob [] ["t{1,2}", "t1"] "t{1,2}" = ["t{1,2}"] := by native_decide
example : glob [] [".h", "a"] "*" = ["a"] := by native_decide

end Patterns.PyGlob
