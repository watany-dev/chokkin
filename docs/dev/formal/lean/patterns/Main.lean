import Lean.Data.Json
import Patterns

open Lean Patterns

/-! `oracle`: one JSON query per stdin line, one JSON answer per stdout line.

* `{"op":"fnmatch","pat":P,"name":N}` → `true`/`false`
* `{"op":"gitignore","ignores":{dir:text},"paths":[P]}` → the paths not ignored
* `{"op":"pyglob","pat":P,"files":[F],"dirs":[D]}` → sorted matches
* `{"op":"tox","line":L}` → `{"factors":[[[name,neg]]]|null,"content":C}`
* `{"op":"find","dirs":[D],"inits":[D],"where":W,"include":[P],"exclude":[P],"namespaces":B}`
  → sorted dotted packages
* `{"op":"pytest_file","patterns":[P],"path":F}` → `true`/`false`
-/

def strs (j : Json) (k : String) : List String :=
  match j.getObjValAs? (Array String) k with
  | .ok a => a.toList
  | .error _ => []

def str (j : Json) (k : String) : String := (j.getObjValAs? String k).toOption.getD ""

def answer (j : Json) : Json :=
  match str j "op" with
  | "fnmatch" => toJson (Fnmatch.fnmatchcase (str j "pat") (str j "name"))
  | "gitignore" =>
    let ignores : List (String × String) :=
      match j.getObjVal? "ignores" with
      | .ok (.obj kvs) => kvs.fold (fun acc k v => (k, v.getStr?.toOption.getD "") :: acc) []
      | _ => []
    toJson ((strs j "paths").filter fun p => !Gitignore.ignored ignores p)
  | "pyglob" => toJson (PyGlob.glob (strs j "files") (strs j "dirs") (str j "pat"))
  | "tox" =>
    let (fs, content) := Tox.expandLine (str j "line")
    Json.mkObj [("factors", match fs with
        | none => Json.null
        | some gs => toJson (gs.map fun g => g.map fun (n, neg) => (n, neg))),
      ("content", toJson content)]
  | "find" =>
    let ns := (j.getObjValAs? Bool "namespaces").toOption.getD true
    toJson (Setuptools.find (strs j "dirs") (strs j "inits") (str j "where")
      (strs j "include") (strs j "exclude") ns)
  | "pytest_file" => toJson (Pytest.collects (strs j "patterns") (str j "path"))
  | op => Json.mkObj [("error", toJson s!"unknown op {op}")]

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let out := match Json.parse line.trimRight with
    | .ok j => answer j
    | .error e => Json.mkObj [("error", toJson e)]
  stdout.putStrLn out.compress
  stdout.flush
  loop stdin stdout

def main : IO Unit := do
  loop (← IO.getStdin) (← IO.getStdout)
