import Lake
open Lake DSL

package lockfile

lean_lib Lockfile

@[default_target]
lean_exe oracle where
  root := `Main
