import ShellSpec.Syntax
/-!
# What a simple command runs

Given argv after quote removal (`none` for a word with a run-time part), the
commands the shell and the named wrapper utilities go on to execute, from each
utility's documented option syntax:

* POSIX builtins and reserved forms: `command` (XCU, `-v` / `-V` only look up), `exec`, `eval`, `trap`, `builtin`, `time` (bash keyword, GNU `time` options).
* Wrappers that exec their operand: coreutils `env` (`-u`, `-C`, `-S`, `-i`, `NAME=value`), `nice` (`-n`, legacy `-N`), `nohup`, `timeout` (`-s`, `-k`, then
  DURATION), `stdbuf` (`-i` / `-o` / `-e`), util-linux `setsid`, `ionice`, `taskset`
  (MASK or `-c LIST`), `flock` (`-c` or FILE), procps `watch` (joins its operands
  into a shell command unless `-x`), findutils `xargs` and `find -exec`, `sudo`.
* Shells `sh -c STRING` and friends (options may follow `-c`; the first operand
  is the string), parsed again with `ShellSpec.parse`.
* CPython's command line (`python -h`): option clusters, `-W` / `-X` arguments
  attached or separate, `-c` and `-m` end option parsing, `-` reads stdin, otherwise the first operand is the script. The `py` launcher's `-3.11` first.
* Project runners: `uv run` (`--module` / `-m`, options with values, a `.py`
  operand runs Python), `poetry run`, `pipenv run`, `pdm run`, `hatch run`
  (`env:cmd`), `rye run`, `conda run`, `coverage run` (`-m` / script).

An option a table does not list makes the utility fail before running anything, so it yields no command (the oracle then expects nothing beyond the wrapper).
-/
namespace ShellSpec

inductive Ev where
  | bin (name : String)
  | pyModule (name : String)
  | pyScript (path : String)
  | pyCode
  | unknown
  deriving Repr, BEq, DecidableEq, Inhabited

abbrev Arg := Option String

def basename (s : String) : String := (s.splitOn "/").getLast!

/-- Option syntax of a getopt-style utility that stops at the first operand. -/
structure Opts where
  flags : List Char := []
  withArg : List Char := []
  longFlags : List String := []
  longWithArg : List String := []
  /-- `-N` (all digits) is an option, as `nice -10`. -/
  numeric : Bool := false

/-- Option bits of one cluster: `some needsNext`, or `none` for an unknown option. -/
def optCluster (o : Opts) : List Char → Option Bool
  | [] => some false
  | c :: cs =>
    if o.withArg.contains c then some cs.isEmpty
    else if o.flags.contains c then optCluster o cs
    else none

/-- Remaining operands after options, or `none` when parsing fails or an option is
not literal. Clusters (`-abc`), attached (`-oVAL`) and separate (`-o VAL`) values,
`--long=value`, `--long value` and `--` are handled as in getopt. -/
def skipOpts (o : Opts) : List Arg → Option (List Arg)
  | [] => some []
  | none :: _ => none
  | some a :: rest =>
    match a.data with
    | ['-', '-'] => some rest
    | '-' :: '-' :: body =>
      match (String.mk body).splitOn "=" with
      | [name] =>
        if o.longFlags.contains name then skipOpts o rest
        else if o.longWithArg.contains name then
          match rest with
          | _ :: rest' => skipOpts o rest'
          | [] => none
        else none
      | name :: _ =>
        if o.longWithArg.contains name || o.longFlags.contains name then skipOpts o rest else none
      | [] => none
    | '-' :: c :: cs =>
      if o.numeric && (c :: cs).all Char.isDigit then skipOpts o rest
      else
        match optCluster o (c :: cs) with
        | some true =>
          match rest with
          | _ :: rest' => skipOpts o rest'
          | [] => none
        | some false => skipOpts o rest
        | none => none
    | _ => some (some a :: rest)

def envOpts : Opts := { flags := ['i', '0', 'v'], withArg := ['u', 'C', 'P'], longFlags := ["ignore-environment", "null", "debug"], longWithArg := ["unset", "chdir", "default-signal", "ignore-signal", "block-signal"] }
def niceOpts : Opts := { withArg := ['n'], longWithArg := ["adjustment"], numeric := true }
def timeoutOpts : Opts := { flags := ['v'], withArg := ['s', 'k'], longFlags := ["preserve-status", "foreground", "verbose"], longWithArg := ["signal", "kill-after"] }
def stdbufOpts : Opts := { withArg := ['i', 'o', 'e'], longWithArg := ["input", "output", "error"] }
def setsidOpts : Opts := { flags := ['c', 'f', 'w'], longFlags := ["ctty", "fork", "wait"] }
def ioniceOpts : Opts := { flags := ['t'], withArg := ['c', 'n'], longFlags := ["ignore"], longWithArg := ["class", "classdata"] }
def xargsOpts : Opts := { flags := ['0', 'r', 't', 'p', 'x', 'o'], withArg := ['I', 'L', 'n', 'P', 's', 'd', 'E', 'a'], longFlags := ["null", "no-run-if-empty", "verbose", "interactive", "exit", "open-tty"], longWithArg := ["max-lines", "max-args", "max-procs", "max-chars", "delimiter", "arg-file", "replace", "eof", "process-slot-var"] }
def sudoOpts : Opts := { flags := ['E', 'H', 'n', 'P', 'S', 'b', 'k', 'A', 'B'], withArg := ['u', 'g', 'C', 'D', 'h', 'p', 'r', 't', 'T', 'U'], longFlags := ["preserve-env", "set-home", "non-interactive", "stdin", "background", "askpass", "bell"], longWithArg := ["user", "group", "close-from", "chdir", "host", "prompt", "role", "type", "command-timeout", "other-user"] }
def gnuTimeOpts : Opts := { flags := ['p', 'a', 'v', 'q'], withArg := ['f', 'o'], longFlags := ["portability", "append", "verbose", "quiet"], longWithArg := ["format", "output"] }
def watchOpts : Opts := { flags := ['b', 'c', 'e', 'g', 'p', 'r', 't', 'w', 'C'], withArg := ['n', 'q'], longFlags := ["beep", "color", "errexit", "chgexit", "precise", "no-rerun", "no-title", "no-wrap", "no-color", "differences"], longWithArg := ["interval", "equexit"] }

/-- CPython options (3.8–3.14 `python -h`). -/
def pyFlags : List Char := ['b', 'B', 'd', 'E', 'h', 'i', 'I', 'O', 'P', 'q', 'R', 's', 'S', 'u', 'v', 'V', 'x', '?']
def pyWithArg : List Char := ['W', 'X']

/-- CPython's option parser, one step per argument or per character of a cluster.
`cs` is the rest of the cluster being read (`[]` between arguments); `fuel`
bounds the steps (`pythonRuns` passes enough for any input). -/
def pyGo : Nat → List Char → List Arg → List Ev
  | 0, _, _ => []
  | _ + 1, [], [] => []
  | _ + 1, [], none :: _ => [.unknown]
  | f + 1, [], some a :: rest =>
    match a.data with
    | [] => [.pyScript a]
    | ['-'] => [.pyCode]
    | ['-', '-'] =>
      match rest with
      | some p :: _ => [.pyScript p]
      | _ => []
    | '-' :: '-' :: _ =>
      if a == "--check-hash-based-pycs" then
        match rest with
        | _ :: rest' => pyGo f [] rest'
        | [] => []
      else []
    | '-' :: c :: cs => pyGo f (c :: cs) rest
    | _ => [.pyScript a]
  | f + 1, c :: cs, rest =>
    if c == 'c' then [.pyCode]
    else if c == 'm' then
      match cs, rest with
      | [], some m :: _ => [.pyModule m]
      | [], none :: _ => [.unknown]
      | [], [] => []
      | _ :: _, _ => [.pyModule (String.mk cs)]
    else if pyWithArg.contains c then
      match cs, rest with
      | [], _ :: rest' => pyGo f [] rest'
      | [], [] => []
      | _ :: _, _ => pyGo f [] rest
    else if pyFlags.contains c then
      if c == 'h' || c == '?' || c == 'V' then [] else pyGo f cs rest
    else []

def argsSize (l : List Arg) : Nat := l.foldr (fun a n => n + 2 + (a.map String.length).getD 0) 1

/-- What `python ARGS` runs. -/
def pythonRuns (l : List Arg) : List Ev := pyGo (argsSize l) [] l

def isPythonName (n : String) : Bool :=
  let stripped := if n.endsWith ".exe" then n.dropRight 4 else n
  let rest :=
    if stripped.startsWith "python" then some (stripped.drop 6)
    else if stripped.startsWith "pypy" then some (stripped.drop 4)
    else none
  match rest with
  | some r => r.all (fun c => c.isDigit || c == '.')
  | none => false

def shellNames : List String := ["sh", "bash", "dash", "zsh", "ksh", "ash", "mksh"]

/-- `sh [options] -c STRING [name args...]`: the string, when literal. -/
def shellCommandString : List Arg → Bool → Option (Option String)
  | [], _ => none
  | none :: _, _ => none
  | some a :: rest, sawC =>
    if a == "--" then
      if sawC then match rest with | s :: _ => some s | [] => none else none
    else if a.startsWith "--" then
      if ["--rcfile", "--init-file"].contains a then
        match rest with | _ :: r => shellCommandString r sawC | [] => none
      else shellCommandString rest sawC
    else if (a.startsWith "-" || a.startsWith "+") && a.length > 1 then
      let cs := (a.drop 1).toList
      let hasC := cs.contains 'c'
      let takesArg := cs.contains 'o' || cs.contains 'O'
      if takesArg then
        match rest with
        | _ :: r => shellCommandString r (sawC || hasC)
        | [] => none
      else shellCommandString rest (sawC || hasC)
    else if sawC then some (some a)
    else none

def runnerOptsUv : Opts := { flags := ['q', 'v', 'n', 's', 'U'], withArg := ['p', 'i', 'f', 'P'], longFlags := ["frozen", "locked", "no-sync", "isolated", "no-project", "all-extras", "all-groups", "dev", "no-dev", "no-editable", "active", "exact", "inexact", "no-env-file", "quiet", "verbose", "offline", "native-tls", "no-cache", "script", "gui-script", "all-packages", "no-default-groups", "only-dev", "upgrade", "reinstall", "compile-bytecode"], longWithArg := ["with", "with-editable", "with-requirements", "python", "extra", "group", "only-group", "no-group", "package", "directory", "project", "env-file", "index", "default-index", "index-url", "extra-index-url", "find-links", "config-file", "cache-dir", "color", "refresh-package", "reinstall-package", "upgrade-package", "resolution", "prerelease", "exclude-newer", "link-mode", "no-binary-package", "python-platform"] }

def coverageRunOpts : Opts := { flags := ['a', 'p', 'L'], longFlags := ["append", "parallel-mode", "branch", "pylib", "timid"], longWithArg := ["rcfile", "source", "include", "omit", "concurrency", "context", "data-file", "debug"] }

/-- Commands a simple command runs. `reparse` turns a shell string into its simple
commands; `fuel` bounds wrapper and string nesting. -/
def runs (reparse : String → Option (List (List Arg))) : Nat → List Arg → List Ev
  | 0, _ => [.unknown]
  | _, [] => []
  | _, none :: _ => [.unknown]
  | fuel + 1, some h :: args =>
    let n := basename h
    let again := runs reparse fuel
    let script (s : Option String) : List Ev :=
      match s with
      | none => [.unknown]
      | some text =>
        match reparse text with
        | some cmds => cmds.foldr (fun c acc => again c ++ acc) []
        | none => [.unknown]
    let after (o : Opts) : List Ev :=
      match skipOpts o args with
      | some rest => again rest
      | none => []
    let rest : List Ev :=
      if n == "command" then
        if args.any (fun a => a == some "-v" || a == some "-V" || a == some "-pv" || a == some "-vp") then []
        else after { flags := ['p'] }
      else if n == "exec" then after { flags := ['c', 'l'], withArg := ['a'] }
      else if n == "builtin" || n == "type" || n == "hash" || n == "which" || n == "whereis" then []
      else if n == "nohup" then after {}
      else if n == "env" then
        match args with
        | some "-S" :: s :: tail => script (s.map (fun t => " ".intercalate (t :: tail.filterMap id)))
        | _ =>
          match skipOpts envOpts args with
          | some rest => again (rest.dropWhile (fun a => match a with
              | some w => (w.splitOn "=").length > 1 && isName ((w.splitOn "=").head!)
              | none => false))
          | none => []
      else if n == "nice" then after niceOpts
      else if n == "timeout" then
        match skipOpts timeoutOpts args with
        | some (_ :: rest) => again rest
        | _ => []
      else if n == "stdbuf" then after stdbufOpts
      else if n == "setsid" then after setsidOpts
      else if n == "ionice" then after ioniceOpts
      else if n == "taskset" then
        match skipOpts { flags := ['a', 'c', 'p'], longFlags := ["all-tasks", "cpu-list", "pid"] } args with
        | some (_ :: rest) => again rest
        | _ => []
      else if n == "flock" then
        match skipOpts { flags := ['s', 'e', 'x', 'n', 'u', 'o', 'F'], withArg := ['w', 'E'] } args with
        | some (_ :: some "-c" :: s :: _) => script s
        | some (_ :: rest) => again rest
        | _ => []
      else if n == "time" then after gnuTimeOpts
      else if n == "xargs" then
        match skipOpts xargsOpts args with
        | some [] => [.bin "echo"]
        | some rest => again rest
        | none => []
      else if n == "sudo" || n == "doas" then after sudoOpts
      else if n == "watch" then
        match skipOpts watchOpts args with
        | some rest =>
          if args.contains (some "-x") || args.contains (some "--exec") then again rest
          else if rest.all Option.isSome then script (some (" ".intercalate (rest.filterMap id))) else [.unknown]
        | none => []
      else if n == "eval" then
        if args.all Option.isSome then script (some (" ".intercalate (args.filterMap id))) else [.unknown]
      else if n == "trap" then
        match args with
        | some s :: _ :: _ => if s == "-" || s.startsWith "-" || s.isEmpty then [] else script (some s)
        | _ => []
      else if n == "find" then
        let rec findExec : List Arg → List Ev
          | [] => []
          | some a :: tail =>
            if a == "-exec" || a == "-execdir" || a == "-ok" || a == "-okdir" then
              let cmd := tail.takeWhile (fun x => x != some ";" && x != some "+")
              again cmd ++ findExec (tail.drop cmd.length)
            else findExec tail
          | none :: tail => findExec tail
        termination_by l => l.length
        decreasing_by all_goals simp_wf; all_goals (first | omega | (simp only [List.length_drop]; omega) | (have := List.length_drop (n := cmd.length) (l := tail); omega))
        findExec args
      else if shellNames.contains n then
        match shellCommandString args false with
        | some s => script s
        | none => []
      else if isPythonName n then
        match args with
        | some "-m" :: some "coverage" :: some "run" :: tail => .pyModule "coverage" :: coverageRuns tail
        | _ => pythonRuns args
      else if n == "py" then
        match args with
        | some v :: tail => if v.startsWith "-3" || v.startsWith "-V:" || v == "-2" then pythonRuns tail else pythonRuns args
        | _ => pythonRuns args
      else if n == "coverage" then
        match args with
        | some "run" :: tail => coverageRuns tail
        | _ => []
      else if n == "uv" then
        match args with
        | some "run" :: tail =>
          let isModule := (tail.takeWhile (fun a => match a with
            | some w => w.startsWith "-"
            | none => false)).any (fun a => a == some "-m" || a == some "--module")
          match skipOpts { runnerOptsUv with flags := 'm' :: runnerOptsUv.flags, longFlags := "module" :: runnerOptsUv.longFlags } tail with
          | some (some m :: _) =>
            if isModule then [.pyModule m]
            else if m.endsWith ".py" then [.pyScript m]
            else again (skipOpts { runnerOptsUv with flags := 'm' :: runnerOptsUv.flags } tail).get!
          | some (none :: _) => [.unknown]
          | _ => []
        | _ => []
      else if n == "poetry" || n == "pipenv" || n == "rye" then
        match args with
        | some "run" :: tail => again tail
        | _ => []
      else if n == "pdm" then
        match args with
        | some "run" :: tail =>
          match skipOpts { flags := ['s', 'g', 'r', 'l', 'j', 'v', 'q'], withArg := ['p'], longFlags := ["site-packages", "global", "recreate", "list", "json", "verbose", "quiet"], longWithArg := ["project", "venv"] } tail with
          | some rest => again rest
          | none => []
        | _ => []
      else if n == "hatch" then
        match args with
        | some "run" :: some c :: tail =>
          let parts := c.splitOn ":"
          let cmd := if parts.length > 1 then ":".intercalate parts.tail else c
          again (some cmd :: tail)
        | _ => []
      else if n == "conda" || n == "mamba" || n == "micromamba" then
        match args with
        | some "run" :: tail =>
          match skipOpts { flags := ['v', 'q'], withArg := ['n', 'p'], longFlags := ["no-capture-output", "live-stream"], longWithArg := ["name", "prefix", "cwd"] } tail with
          | some rest => again rest
          | none => []
        | _ => []
      else []
    .bin n :: rest
where
  coverageRuns (tail : List Arg) : List Ev :=
    match skipOpts { coverageRunOpts with flags := 'm' :: coverageRunOpts.flags, longFlags := "module" :: coverageRunOpts.longFlags } tail with
    | some (some x :: _) =>
      if (tail.takeWhile (fun a => a != some x)).any (fun a => a == some "-m" || a == some "--module") then [.pyModule x] else [.pyScript x]
    | some (none :: _) => [.unknown]
    | _ => []

def wordsOf (c : SimpleCmd) : List Arg := c.words.map Word.literal?

def reparseStr (text : String) : Option (List (List Arg)) :=
  match parse text with
  | .ok cmds => some (cmds.toList.map wordsOf)
  | .error _ => none

/-- Everything a script runs. `none` when it does not parse. -/
def scriptRuns (text : String) : Option (List Ev) :=
  match parse text with
  | .error _ => none
  | .ok cmds => some (cmds.toList.foldr (fun c acc => runs reparseStr 16 (wordsOf c) ++ acc) [])

end ShellSpec
