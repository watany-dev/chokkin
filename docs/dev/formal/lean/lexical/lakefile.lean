import Lake
open Lake DSL

package lexical

@[default_target]
lean_lib Lexical

@[default_target]
lean_exe oracle where
  root := `Main
