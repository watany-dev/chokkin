import Lake
open Lake DSL

package marker_naming

lean_lib MarkerNaming

@[default_target]
lean_exe oracle where
  root := `Main
  supportInterpreter := false
