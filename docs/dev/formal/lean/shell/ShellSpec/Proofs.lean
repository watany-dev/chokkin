import ShellSpec.Exec
/-!
# Properties of the specification

The theorems hold for every input and every amount of fuel (not a finite
domain). They pin the parts of the CPython and getopt option syntax the oracle's
expectations rest on; the regression examples check the syntax layer against
the root causes in `ledger.ja.md`, so a parser change that loses one fails the build.
-/
namespace ShellSpec

/-- `-c` ends CPython's option parsing: nothing after it is a module or script. -/
theorem python_c_ends_options (f : Nat) (rest : List Arg) :
    pyGo (f + 2) [] (some "-c" :: rest) = [.pyCode] := by
  simp [pyGo]

/-- `-m MOD` runs MOD whatever follows. -/
theorem python_m_runs_module (f : Nat) (m : String) (rest : List Arg) :
    pyGo (f + 2) [] (some "-m" :: some m :: rest) = [.pyModule m] := by
  simp [pyGo]

/-- A flag without a value is transparent: `python -u X` runs what `python X` runs. -/
theorem python_u_transparent (f : Nat) (rest : List Arg) :
    pyGo (f + 3) [] (some "-u" :: rest) = pyGo (f + 1) [] rest := by
  simp [pyGo, pyFlags, pyWithArg]

/-- A clustered `-um MOD` runs MOD, as `-u -m MOD` does. -/
theorem python_um_runs_module (f : Nat) (m : String) (rest : List Arg) :
    pyGo (f + 3) [] (some "-um" :: some m :: rest) = [.pyModule m] := by
  simp [pyGo, pyFlags, pyWithArg]

/-- `-W VALUE` consumes its value, whatever the value looks like. -/
theorem python_W_consumes_value (f : Nat) (v : String) (rest : List Arg) :
    pyGo (f + 2) [] (some "-W" :: some v :: rest) = pyGo f [] rest := by
  simp [pyGo, pyWithArg]

/-- `--check-hash-based-pycs MODE` consumes its value. -/
theorem python_check_hash_consumes_value (f : Nat) (v : Arg) (rest : List Arg) :
    pyGo (f + 1) [] (some "--check-hash-based-pycs" :: v :: rest) = pyGo f [] rest := by
  simp [pyGo]

/-- `--` ends option parsing in every getopt-style table. -/
theorem skipOpts_double_dash (o : Opts) (rest : List Arg) : skipOpts o (some "--" :: rest) = some rest := by
  simp [skipOpts]

/-! Regression examples: one per syntax-level root cause (ledger R1–R5). -/
example : scriptRuns "x=$(python -m pkg.tool)" = some [.bin "python", .pyModule "pkg.tool"] := by native_decide
example : scriptRuns "function run { black .; }" = some [.bin "black"] := by native_decide
example : scriptRuns ">log black ." = some [.bin "black"] := by native_decide
example : scriptRuns "echo \"usage:\nblack .\"" = some [.bin "echo"] := by native_decide
example : scriptRuns "command -v black" = some [.bin "command"] := by native_decide

end ShellSpec
