import Lake
open Lake DSL

package shell_oracle

lean_lib ShellSpec

@[default_target]
lean_exe oracle where
  root := `Main
