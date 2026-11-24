import Lean.Data.Json
import MarkerNaming

/-!
Differential oracle: one JSON object per stdin line, one answer per stdout
line. Kinds: `marker` (requires-python + marker → verdict), `venv`
(distributions + imports → which distributions each import uses), `key`
(setuptools `extras_require` key), `normalize` (PEP 503 name).
-/
open Lean MarkerNaming

def str (j : Json) (k : String) : String := (j.getObjValAs? String k).toOption.getD ""

def distOf (j : Json) : Dist.Dist :=
  { kind := match str j "kind" with
      | "editable" => .editable
      | "egg" => .egg
      | _ => .wheel
    metadata := str j "metadata"
    files := str j "files"
    topLevel := (j.getObjValAs? String "top_level").toOption
    dirName := str j "dir_name" }

def answer (j : Json) : Json :=
  match str j "kind" with
  | "marker" =>
    match Pep440.parseSpecifierSet (str j "requires_python"), Marker.parse (str j "marker") with
    | some rp, some m =>
      let v := match Marker.verdict rp m with
        | .always => "always" | .notAlways => "not_always" | .unknown => "unknown"
      Json.mkObj [("verdict", v)]
    | none, _ => Json.mkObj [("verdict", "unknown"), ("why", "requires-python outside the model")]
    | _, none => Json.mkObj [("verdict", "unknown"), ("why", "marker does not parse")]
  | "venv" =>
    let dists := ((j.getObjValAs? (Array Json) "dists").toOption.getD #[]).toList.map distOf
    let imports := (j.getObjValAs? (Array String) "imports").toOption.getD #[]
    Json.arr <| imports.map fun m =>
      let r := Dist.resolve dists m
      Json.mkObj [("module", m), ("used", toJson r.used), ("resolved", r.resolved)]
  | "key" =>
    let k := Setuptools.splitKey (str j "key")
    Json.mkObj [("extra", toJson k.extra), ("marker", toJson k.marker)]
  | "normalize" => Json.mkObj [("name", Naming.normalize (str j "name"))]
  | other => Json.mkObj [("error", s!"unknown kind {other}")]

partial def loop (stdin : IO.FS.Stream) (stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let out := match Json.parse line with
    | .ok j => answer j
    | .error e => Json.mkObj [("error", e)]
  stdout.putStrLn out.compress
  stdout.flush
  loop stdin stdout

def main : IO Unit := do loop (← IO.getStdin) (← IO.getStdout)
