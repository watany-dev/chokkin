import Lake
open Lake DSL

package binding

lean_lib Binding

@[default_target]
lean_exe oracle where
  root := `Main
