import Lake
open Lake DSL

package patterns where

lean_lib Patterns where

@[default_target]
lean_exe oracle where
  root := `Main
