import MarkerNaming.Naming

/-!
Which installed distribution provides an import, written from:

- Core Metadata (`METADATA` / `PKG-INFO` are RFC 822 headers: the fields
  end at the first empty line, the rest is the description; field names are
  case-insensitive; a line starting with whitespace continues the field);
- PEP 794 (`Import-Name` / `Import-Namespace`: a dotted name, optionally
  followed by `; private`; an empty `Import-Name` means "provides none");
- the Recording-installed-packages spec (`RECORD` is CSV, paths use `/`,
  relative to site-packages) and legacy `*.egg-info` with
  `installed-files.txt` (relative to the egg-info directory), both of which
  `importlib.metadata` discovers;
- the import system (`importlib` `FileFinder` in one site-packages: a
  directory with `__init__` is a regular package, else a module file
  (`.py`, or an extension module `name.<tag>.so` / `name.<tag>.pyd`), else a
  directory without `__init__` is a PEP 420 namespace portion that loads no
  file).

A distribution is *used* by `import a.b.c` exactly when it owns a file the
import loads: `a`'s, `a.b`'s or `a.b.c`'s module or `__init__`.
-/
namespace MarkerNaming.Dist

/-! ## Core Metadata headers -/

def lowerAscii (s : String) : String := s.map fun c =>
  if 'A' ≤ c ∧ c ≤ 'Z' then Char.ofNat (c.toNat + 32) else c

/-- Header fields as `(lowercased name, value)`, stopping at the first
empty line. -/
def headers (text : String) : List (String × String) := Id.run do
  let mut out : Array (String × String) := #[]
  for line in text.splitOn "\n" do
    let line := line.dropRightWhile (· == '\r')
    if line.isEmpty then break
    if line.front == ' ' || line.front == '\t' then
      if let some (k, v) := out.back? then
        out := out.pop.push (k, v ++ " " ++ line.trim)
      continue
    match line.splitOn ":" with
    | k :: rest@(_ :: _) => out := out.push (lowerAscii k.trim, (":".intercalate rest).trim)
    | _ => pure ()
  return out.toList

def field (hs : List (String × String)) (k : String) : List String :=
  hs.filterMap fun (k', v) => if k' == k then some v else none

/-- PEP 794 value: the dotted name before an optional `; private`. -/
def importNameValue (v : String) : String :=
  match v.splitOn ";" with
  | name :: _ => name.trim
  | [] => v.trim

/-! ## RECORD (CSV) -/

/-- First CSV field of a row: a quoted field may hold commas and `""`. -/
partial def csvFirst (cs : List Char) : String :=
  match cs with
  | '"' :: rest =>
    let rec go : List Char → List Char → List Char
      | '"' :: '"' :: r, acc => go r ('"' :: acc)
      | '"' :: _, acc => acc.reverse
      | c :: r, acc => go r (c :: acc)
      | [], acc => acc.reverse
    String.mk (go rest [])
  | _ => String.mk (cs.takeWhile (· != ','))

/-! ## Files a distribution owns -/

inductive Kind | wheel | editable | egg
  deriving DecidableEq, Repr, Inhabited

structure Dist where
  kind : Kind
  metadata : String
  /-- `RECORD` (wheel), `installed-files.txt` (egg); unused for editable. -/
  files : String := ""
  topLevel : Option String := none
  /-- Distribution name the directory carries, used when `Name` is absent. -/
  dirName : String := ""
  deriving Inhabited

def Dist.name (d : Dist) : String :=
  (field (headers d.metadata) "name").head?.getD d.dirName

/-- Normalize `a/./b`, `a/../b` against an egg-info directory one level
below site-packages. -/
def eggRelative (p : String) : Option String :=
  if p.startsWith "../" then some (p.drop 3) else none

def Dist.paths (d : Dist) : List String :=
  let rows := (d.files.splitOn "\n").map (fun l => csvFirst (l.dropRightWhile (· == '\r')).toList)
    |>.filter (!·.isEmpty)
  match d.kind with
  | .wheel => rows
  | .egg => rows.filterMap eggRelative
  | .editable => []

/-- Import names an editable install declares (PEP 794, else
`top_level.txt`); its files live outside site-packages. -/
def Dist.editableNames (d : Dist) : List String :=
  let hs := headers d.metadata
  let names := (field hs "import-name").map importNameValue |>.filter (!·.isEmpty)
  if !(field hs "import-name").isEmpty then names
  else match d.topLevel with
    | some t => (t.splitOn "\n").map String.trim |>.filter (!·.isEmpty)
    | none => []

/-! ## The import system over one site-packages -/

def isIdent (s : String) : Bool :=
  !s.isEmpty && s.all (fun c => c.isAlphanum || c == '_') && !s.front.isDigit

/-- The module a file *name* (last path component) defines, if any:
`m.py`, or an extension module `m.<tag>.so` / `m.<tag>.pyd` / `m.so`. -/
def moduleOfFile (f : String) : Option String :=
  if f.endsWith ".py" then
    let m := f.dropRight 3
    if isIdent m then some m else none
  else if f.endsWith ".so" || f.endsWith ".pyd" then
    let m := (f.splitOn ".").head!
    if isIdent m then some m else none
  else none

inductive Found | file (path : String) | namespace | missing
  deriving DecidableEq, Repr

/-- `FileFinder` lookup of `name` in directory `dir` ("" = site-packages). -/
def lookup (all : List String) (dir name : String) : Found :=
  let base := if dir.isEmpty then name else dir ++ "/" ++ name
  let initFile := all.find? fun p =>
    p.startsWith (base ++ "/") && (moduleOfFile (p.drop (base.length + 1)) == some "__init__")
  match initFile with
  | some p => .file p
  | none =>
    let modFile := all.find? fun p =>
      let parent := if dir.isEmpty then "" else dir ++ "/"
      p.startsWith parent && !(p.drop parent.length).contains '/' &&
        moduleOfFile (p.drop parent.length) == some name
    match modFile with
    | some p => .file p
    | none => if all.any (·.startsWith (base ++ "/")) then .namespace else .missing

structure Result where
  /-- Normalized names of the distributions the import uses. -/
  used : List String
  resolved : Bool
  deriving Repr

def resolve (dists : List Dist) (module : String) : Result := Id.run do
  let wheelish := dists.filter (·.kind != .editable)
  let all := wheelish.foldl (fun acc d => acc ++ d.paths) []
  let owners := fun (p : String) =>
    wheelish.filter (·.paths.contains p) |>.map (Naming.normalize ·.name)
  -- An editable install puts its own path on `sys.path`.
  let editable := dists.filter (·.kind == .editable) |>.filter fun d =>
    d.editableNames.any fun n => n == module || module.startsWith (n ++ ".")
  if !editable.isEmpty then
    return { used := editable.map (Naming.normalize ·.name), resolved := true }
  let segs := module.splitOn "."
  let mut used : List String := []
  let mut dir := ""
  for seg in segs do
    match lookup all dir seg with
    | .missing => return { used, resolved := false }
    | .namespace => pure ()
    | .file p => used := used ++ (owners p).filter (!used.contains ·)
    dir := if dir.isEmpty then seg else dir ++ "/" ++ seg
  return { used, resolved := true }

end MarkerNaming.Dist
