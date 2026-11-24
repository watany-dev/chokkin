/-!
setuptools `extras_require` keys (setuptools "Dependency management",
"Optional dependencies"; `pkg_resources.Distribution._dep_map`): a key is
`extra[:marker]`. The part after the first `:` is a PEP 508 marker on every
requirement of the key, and an empty extra (`":python_version<'3.8'"`) makes
them conditional *install* requirements, not an extra.
-/
namespace MarkerNaming.Setuptools

structure Key where
  /-- `none`: the requirements are install requirements. -/
  extra : Option String
  marker : Option String
  deriving Repr, DecidableEq

def splitKey (key : String) : Key :=
  match key.splitOn ":" with
  | [] | [_] => { extra := some key.trim, marker := none }
  | e :: rest =>
    let marker := (":".intercalate rest).trim
    let e := e.trim
    { extra := if e.isEmpty then none else some e,
      marker := if marker.isEmpty then none else some marker }

end MarkerNaming.Setuptools
