import ShellSpec
import Lean.Data.Json
open ShellSpec Lean

def evJson : Ev → Json
  | .bin n => Json.arr #["bin", n]
  | .pyModule m => Json.arr #["module", m]
  | .pyScript p => Json.arr #["script", p]
  | .pyCode => Json.arr #["code"]
  | .unknown => Json.arr #["unknown"]

/-- One JSON string (a script) per input line; one JSON value per output line:
the list of events, or `null` when the script does not parse. -/
partial def loop (stdin stdout : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let out :=
    match Json.parse line with
    | .ok (.str text) =>
      match scriptRuns text with
      | some evs => Json.arr (evs.map evJson).toArray
      | none => Json.null
    | _ => Json.null
  stdout.putStrLn out.compress
  stdout.flush
  loop stdin stdout

def main : IO Unit := do
  loop (← IO.getStdin) (← IO.getStdout)
