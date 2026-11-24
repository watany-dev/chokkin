import Binding.Spec

open Lean (Json)

/-- Read the IR of a project (`harness/py2ir.py`) and print the references the
spec derives as JSON: `{"refs": [[importer, module, name], ...], "passes": n}`. -/
def main (args : List String) : IO UInt32 := do
  let some path := args.head? | IO.eprintln "usage: oracle <ir.json>"; return 2
  let input ← IO.FS.readFile path
  match Json.parse input >>= Binding.toProject with
  | .error e =>
    IO.eprintln s!"oracle: {e}"
    return 2
  | .ok ms =>
    let (refs, passes) := Binding.analyze ms
    let key := fun ((i, m, n) : String × String × String) => s!"{m}\x00{n}\x00{i}"
    let sorted := refs.toArray.qsort (fun a b => key a < key b)
    let js := sorted.map fun (i, m, n) => Json.arr #[Json.str i, Json.str m, Json.str n]
    IO.println (Json.compress (Json.mkObj [("refs", Json.arr js), ("passes", Json.num passes)]))
    return 0
