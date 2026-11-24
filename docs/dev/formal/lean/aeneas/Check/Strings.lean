import Spec.Path
import Spec.Pep503
import Spec.TargetPart
import Std.Data.HashMap

/-!
The copied string functions, run by `extract`'s `dump` binary on every
string over a small alphabet, checked against the specifications. Charon
and Aeneas have no model of `str`, so these functions are compared on a
finite domain instead of being translated.
-/

namespace Check.Strings

/-- `none` when `output` is what the specification allows for `input`,
    else the expected value. -/
def expected (fn input output : String) : Option String :=
  if ["normalize_user_path", "normalize_dir", "normalize_path",
      "normalize_relative_path"].contains fn then
    -- `entry` and `--trace` name a file: neither the root nor, by XBD 4.16,
    -- a pathname with a trailing separator (it must be a directory).
    let names_file := fn == "normalize_user_path"
    let not_a_file := Spec.Path.canonical input == "" ||
      (input.toList.getLast?.map Spec.Path.isSep).getD false ||
      ((input.toList.splitBy (fun a b => !Spec.Path.isSep a && !Spec.Path.isSep b)).getLast?
        == some ['.'])
    if Spec.Path.inDomain input && !(names_file && not_a_file)
        && output != Spec.Path.canonical input then
      some (Spec.Path.canonical input)
    else none
  else if fn == "normalize_distribution_name" || fn == "normalized" then
    let want := Spec.Pep503.normalize input
    if Spec.Pep503.isAscii input && output != want then some want else none
  else if fn == "target_part" then
    if Spec.TargetPart.sound (fun _ => output) input then none
    else some "<no `:`, decodes back>"
  else some s!"<unknown function {fn}>"

structure Tally where
  checked : Nat := 0
  failed : Nat := 0
  shortest : Option (String × String × String) := none

def run (path : System.FilePath) : IO Bool := do
  let text ← IO.FS.readFile path
  let mut tallies : Std.HashMap String Tally := {}
  for line in text.splitOn "\n" do
    match line.splitOn "\t" with
    | [fn, input, output] =>
      let t := tallies.getD fn {}
      let t := { t with checked := t.checked + 1 }
      let t := match expected fn input output with
        | none => t
        | some want =>
          let better := match t.shortest with
            | none => true
            | some (i, _, _) => input.length < i.length
          { t with failed := t.failed + 1,
                   shortest := if better then some (input, output, want) else t.shortest }
      tallies := tallies.insert fn t
    | _ => pure ()
  let mut ok := true
  for (fn, t) in tallies.toList.mergeSort (fun a b => a.1 ≤ b.1) do
    match t.shortest with
    | none => IO.println s!"OK    {fn}: {t.checked} inputs"
    | some (i, o, w) =>
      ok := false
      IO.println s!"FAIL  {fn}: {t.failed}/{t.checked} inputs; shortest {repr i} gives {repr o}, spec {repr w}"
  return ok

end Check.Strings
