import Lockfile
import Lean.Data.Json

/-!
`oracle` reads one scenario per line on stdin and prints one verdict object
per line:

    {"lock": [{"name", "deps": [{"name", "extras"}], "optional": {"<extra>": [...]}}],
     "declared": [{"name", "extras"}], "extrasKnown": bool, "imports": ["<dist>"]}
    → {"closed": bool, "verdicts": {"<dist>": "CHK004/certain" | ...}}

Names are normalized here (PEP 503), so the harness may pass them as the
lockfile spells them.
-/

open Lean Lockfile

def getArr (j : Json) (k : String) : Array Json :=
  (j.getObjValD k).getArr?.toOption.getD #[]

def getStr (j : Json) (k : String) : String :=
  (j.getObjValD k).getStr?.toOption.getD ""

def parseDep (j : Json) : Dep :=
  { name := normalize (getStr j "name"),
    extras := (getArr j "extras").toList.map fun e => normalize (e.getStr?.toOption.getD "") }

def parsePkg (j : Json) : Pkg :=
  let opt := match j.getObjValD "optional" with
    | .obj kvs => kvs.fold (fun acc k v =>
        acc ++ [(normalize k, (v.getArr?.toOption.getD #[]).toList.map parseDep)]) []
    | _ => []
  { name := normalize (getStr j "name"),
    deps := (getArr j "deps").toList.map parseDep,
    optional := opt }

def answer (line : String) : Except String Json := do
  let j ← Json.parse line
  let lock := (getArr j "lock").toList.map parsePkg
  let declared := (getArr j "declared").toList.map parseDep
  let extrasKnown := (j.getObjValD "extrasKnown").getBool?.toOption.getD true
  let imports := (getArr j "imports").toList.map fun e => normalize (e.getStr?.toOption.getD "")
  let closed := isClosed lock (closure lock (seedsOf declared))
  let verdicts := imports.map fun d => (d, Json.str (classify lock declared extrasKnown d).code)
  return Json.mkObj [("closed", Json.bool closed), ("verdicts", Json.mkObj verdicts)]

partial def loop (stdin stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let trimmed := line.trim
  if !trimmed.isEmpty then
    match answer trimmed with
    | .ok j => stdout.putStrLn j.compress
    | .error e => stdout.putStrLn (Json.mkObj [("error", Json.str e)]).compress
    stdout.flush
  loop stdin stdout

def main : IO Unit := do
  loop (← IO.getStdin) (← IO.getStdout)
