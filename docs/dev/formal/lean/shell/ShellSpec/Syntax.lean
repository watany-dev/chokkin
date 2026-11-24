/-!
# POSIX shell syntax (XCU 2.3 token recognition, 2.6 expansions, 2.7 here-documents,
2.9 shell commands, 2.10 grammar) plus the bash extensions that CI scripts use
(`[[ ]]`, `(( ))`, `$'..'`, `function`, arrays, `|&`, `&>`, `<<<`, `<( )`).

Written from the standard, not from `src/plugins/commands.rs`. The output is every
simple command the script can run, at any nesting depth: inside compound commands,
function bodies, command substitutions (also inside double quotes, assignments,
redirection targets and unquoted here-document bodies) and process substitutions.
A word is kept as its parts; a part that depends on run-time state (parameter,
command substitution, arithmetic, unquoted glob or brace expansion) is `dyn`.

Input this module does not recognise is never dropped silently: a syntax error
stops the parse and is reported as `ParseResult.error`, which the oracle turns into
"no expectation" for that input.
-/
namespace ShellSpec

inductive Part where
  | lit (s : String) (quoted : Bool)
  | dyn
  deriving Repr, BEq, Inhabited

structure Word where
  parts : Array Part := #[]
  deriving Repr, BEq, Inhabited

/-- The word's value after quote removal when it has no run-time part. -/
def Word.literal? (w : Word) : Option String :=
  w.parts.foldl (fun acc p => match acc, p with
    | some s, .lit t _ => some (s ++ t)
    | _, _ => none) (some "")

/-- A reserved word or `{`/`}`/`!` is only recognised when unquoted (XCU 2.4). -/
def Word.plain? (w : Word) : Option String :=
  match w.parts.toList with
  | [.lit s false] => some s
  | _ => none

def Word.anyQuoted (w : Word) : Bool :=
  w.parts.any fun p => match p with | .lit _ q => q | .dyn => false

/-- A simple command after its assignment prefix: the words that form argv. -/
structure SimpleCmd where
  words : List Word
  deriving Repr, Inhabited

inductive Tok where
  | word (w : Word)
  | op (s : String)
  | nl
  | arith
  | eof
  deriving Repr, BEq, Inhabited

structure Heredoc where
  delim : String
  stripTabs : Bool
  quoted : Bool
  deriving Inhabited

structure St where
  input : Array Char
  pos : Nat := 0
  peeked : Option (Tok × Nat × Option Bool) := none
  /-- `some stripTabs` after `<<` / `<<-`: the next word is a delimiter. -/
  expectDelim : Option Bool := none
  pending : Array Heredoc := #[]
  out : Array SimpleCmd := #[]
  err : Option String := none
  depth : Nat := 0

abbrev P := StateM St

/-- `NAME=` so far, unquoted: a following `(` starts a bash array value. -/
def isAssignPrefix (ps : Array Part) : Bool :=
  match ps.toList with
  | [.lit s false] => s.endsWith "=" && (s.dropRight 1).length > 0 && (s.dropRight 1).all (fun c => c.isAlphanum || c == '_') && !((s.get 0).isDigit)
  | _ => false

/-- An assignment word (XCU 2.10.2 rule 7): unquoted `NAME=` prefix. -/
def isAssignment (w : Word) : Bool :=
  match w.parts.toList with
  | .lit s false :: _ =>
    match s.splitOn "=" with
    | name :: _ :: _ => name.length > 0 && name.all (fun c => c.isAlphanum || c == '_') && !((name.get 0).isDigit)
    | _ => false
  | _ => false

def isBlank (c : Char) : Bool := c == ' ' || c == '\t'
def isOpChar (c : Char) : Bool := c == ';' || c == '&' || c == '|' || c == '(' || c == ')' || c == '<' || c == '>'
def isNameStart (c : Char) : Bool := c.isAlpha || c == '_'
def isNameChar (c : Char) : Bool := c.isAlphanum || c == '_'

def isName (s : String) : Bool :=
  match s.toList with
  | c :: cs => isNameStart c && cs.all isNameChar
  | [] => false

/-- Operators, longest first (XCU 2.10.1 plus bash). -/
def operators : List String :=
  [";;&", "&>>", "<<-", "<<<", ";;", ";&", "&&", "||", "|&", "&>", "<<", ">>", "<&", ">&", "<>", ">|",
   ";", "&", "|", "(", ")", "<", ">"]

def redirOps : List String := ["<", ">", ">>", "<&", ">&", "<>", ">|", "&>", "&>>", "<<", "<<-", "<<<"]

def fail (msg : String) : P Unit := modify fun s => if s.err.isNone then { s with err := some msg } else s

def cur : P (Option Char) := do
  let s ← get
  return s.input[s.pos]?

def ahead (k : Nat) : P (Option Char) := do
  let s ← get
  return s.input[s.pos + k]?

def adv (k : Nat := 1) : P Unit := modify fun s => { s with pos := s.pos + k }

def emit (c : SimpleCmd) : P Unit := modify fun s => { s with out := s.out.push c }

def startsWith (str : String) : P Bool := do
  let s ← get
  let cs := str.toList
  return (List.range cs.length).all fun i => s.input[s.pos + i]? == cs[i]?

/-- Push a literal character onto the word being built. -/
def pushLit (parts : Array Part) (c : Char) (quoted : Bool) : Array Part :=
  match parts.back? with
  | some (.lit s q) => if q == quoted then parts.pop.push (.lit (s.push c) q) else parts.push (.lit c.toString quoted)
  | _ => parts.push (.lit c.toString quoted)

def pushDyn (parts : Array Part) : Array Part := parts.push .dyn

/-- Skip to the `close` that balances the already-consumed `open`, honouring quotes
and backslashes. Used for `$(( ))`, `${ }`, `(( ))` and array assignments. -/
partial def skipBalanced (openC closeC : Char) (depth : Nat) : P Unit := do
  match ← cur with
  | none => fail s!"unterminated {openC}"
  | some c =>
    if c == '\\' then adv 2; skipBalanced openC closeC depth
    else if c == '\'' then
      adv
      let rec sq : P Unit := do
        match ← cur with
        | none => fail "unterminated '"
        | some '\'' => adv
        | some _ => adv; sq
      sq; skipBalanced openC closeC depth
    else if c == '"' then
      adv
      let rec dq : P Unit := do
        match ← cur with
        | none => fail "unterminated \""
        | some '\\' => adv 2; dq
        | some '"' => adv
        | some _ => adv; dq
      dq; skipBalanced openC closeC depth
    else if c == openC then adv; skipBalanced openC closeC (depth + 1)
    else if c == closeC then
      adv
      if depth > 1 then skipBalanced openC closeC (depth - 1)
    else adv; skipBalanced openC closeC depth

def isReserved (t : Tok) (names : List String) : Bool :=
  match t with
  | .word w => match w.plain? with | some s => names.contains s | none => false
  | _ => false

mutual

/-- `$` at the cursor (XCU 2.6.2–2.6.4 and bash `$'..'`). Appends the expansion. -/
partial def dollar (parts : Array Part) (inDq : Bool) : P (Array Part) := do
  adv
  match ← cur with
  | some '(' =>
    if (← ahead 1) == some '(' then
      adv 2; skipBalanced '(' ')' 1
      -- the closing `))`: skipBalanced consumed one `)`, the next must be `)`
      if (← cur) == some ')' then adv else fail "unterminated $(("
      return pushDyn parts
    else
      adv; subshellUntilParen; return pushDyn parts
  | some '{' => adv; skipBalanced '{' '}' 1; return pushDyn parts
  | some '\'' =>
    if inDq then return pushLit parts '$' true
    adv
    let mut ps := parts
    repeat
      match ← cur with
      | none => fail "unterminated $'"; break
      | some '\'' => adv; break
      | some '\\' =>
        match ← ahead 1 with
        | some 'n' => ps := pushLit ps '\n' true
        | some 't' => ps := pushLit ps '\t' true
        | some c => ps := pushLit ps c true
        | none => pure ()
        adv 2
      | some c => ps := pushLit ps c true; adv
    return ps
  | some '"' => if inDq then return pushLit parts '$' true else return parts  -- `$"..."`: the quote is read next
  | some c =>
    if isNameStart c then
      repeat
        match ← cur with
        | some d => if isNameChar d then adv else break
        | none => break
      return pushDyn parts
    else if c.isDigit || "@*#?$!-".contains c then adv; return pushDyn parts
    else return pushLit parts '$' inDq
  | none => return pushLit parts '$' inDq

/-- Backquoted command substitution (XCU 2.6.3): the text up to the closing
backquote, with `\$`, `` \` `` and `\\` unescaped, parsed as a script. -/
partial def backquote : P Unit := do
  adv
  let mut body : String := ""
  repeat
    match ← cur with
    | none => fail "unterminated `"; break
    | some '`' => adv; break
    | some '\\' =>
      match ← ahead 1 with
      | some c => if c == '$' || c == '`' || c == '\\' then body := body.push c else body := (body.push '\\').push c
      | none => pure ()
      adv 2
    | some c => body := body.push c; adv
  subScript body

/-- Parse `text` as a separate script and add its commands. -/
partial def subScript (text : String) : P Unit := do
  let s ← get
  if s.depth > 8 then return
  let (_, s') := (parseProgram).run { input := text.toList.toArray, depth := s.depth + 1 }
  modify fun st => { st with out := st.out ++ s'.out, err := if st.err.isNone then s'.err else st.err }

/-- `$(` already consumed: parse a command list up to the matching `)`. -/
partial def subshellUntilParen : P Unit := do
  let saved := (← get).peeked
  modify fun s => { s with peeked := none }
  parseList [] [")"]
  match ← next with
  | .op ")" => pure ()
  | _ => fail "unterminated $("
  modify fun s => { s with peeked := saved }

/-- Double-quoted text after the opening quote (XCU 2.2.3). -/
partial def dquoted (parts : Array Part) (terminated : Bool) : P (Array Part) := do
  let mut ps := parts
  repeat
    match ← cur with
    | none => if terminated then fail "unterminated \"" else pure (); break
    | some '"' => if terminated then adv; break else ps := pushLit ps '"' true; adv
    | some '\\' =>
      match ← ahead 1 with
      | some '\n' => adv 2
      | some c =>
        if c == '$' || c == '`' || c == '"' || c == '\\' then ps := pushLit ps c true
        else ps := pushLit (pushLit ps '\\' true) c true
        adv 2
      | none => ps := pushLit ps '\\' true; adv
    | some '$' => ps ← dollar ps true
    | some '`' => backquote; ps := pushDyn ps
    | some c => ps := pushLit ps c true; adv
  return ps

/-- A word starting at the cursor (XCU 2.3 rules 4–10). -/
partial def readWord : P Word := do
  let mut ps : Array Part := #[]
  repeat
    match ← cur with
    | none => break
    | some c =>
      if isBlank c || c == '\n' then break
      else if c == '(' && isAssignPrefix ps then
        adv; skipBalanced '(' ')' 1; ps := pushDyn ps
      else if (c == '<' || c == '>') && (← ahead 1) == some '(' then
        if ps.isEmpty then adv 2; subshellUntilParen; ps := pushDyn ps else break
      else if isOpChar c then break
      else if c == '\\' then
        match ← ahead 1 with
        | some '\n' => adv 2
        | some d => ps := pushLit ps d true; adv 2
        | none => adv
      else if c == '\'' then
        adv
        let mut any := false
        repeat
          match ← cur with
          | none => fail "unterminated '"; break
          | some '\'' => adv; break
          | some d => ps := pushLit ps d true; any := true; adv
        if !any then ps := ps.push (.lit "" true)
      else if c == '"' then
        adv
        let before := ps.size
        ps ← dquoted ps true
        if ps.size == before then ps := ps.push (.lit "" true)
      else if c == '$' then
        if (← ahead 1) == some '"' then adv else ps ← dollar ps false
      else if c == '`' then backquote; ps := pushDyn ps
      else if c == '*' || c == '?' then ps := pushDyn ps; adv
      else if c == '{' && (← braceExpands) then ps := pushDyn ps; adv
      else ps := pushLit ps c false; adv
  return { parts := ps }

/-- Unquoted `{a,b}` or `{1..3}` (bash brace expansion) from the cursor. -/
partial def braceExpands : P Bool := do
  let s ← get
  let rec go (k : Nat) (depth : Nat) (sawSep : Bool) (prev : Char) (fuel : Nat) : Bool :=
    match fuel with
    | 0 => false
    | fuel + 1 =>
      match s.input[k]? with
      | none => false
      | some c =>
        if isBlank c || c == '\n' || isOpChar c || c == '\'' || c == '"' then false
        else if c == '{' then go (k+1) (depth+1) sawSep c fuel
        else if c == '}' then
          if depth == 1 then sawSep else go (k+1) (depth-1) sawSep c fuel
        else if c == ',' && depth == 1 then go (k+1) depth true c fuel
        else if c == '.' && prev == '.' && depth == 1 then go (k+1) depth true c fuel
        else go (k+1) depth sawSep c fuel
  return go (s.pos + 1) 1 false '{' 4096

/-- Here-document bodies queued on the line just ended (XCU 2.7.4). -/
partial def readHeredocs : P Unit := do
  let hs := (← get).pending
  modify fun s => { s with pending := #[] }
  for h in hs do
    let mut body : String := ""
    repeat
      -- read one line
      let mut line : String := ""
      let mut ended := false
      repeat
        match ← cur with
        | none => ended := true; break
        | some '\n' => adv; break
        | some c => line := line.push c; adv
      let cmp := if h.stripTabs then line.dropWhile (· == '\t') else line
      if cmp == h.delim then break
      body := body ++ cmp ++ "\n"
      if ended then break
    if !h.quoted then
      -- expansions in the body run (2.7.4): parse it as double-quoted text
      let s ← get
      let (_, s') := (dquoted #[] false).run { input := body.toList.toArray, depth := s.depth + 1 }
      modify fun st => { st with out := st.out ++ s'.out }

/-- Next token from the input, ignoring any peeked token. -/
partial def lexToken : P Tok := do
  -- blanks and line continuations
  repeat
    match ← cur with
    | some c =>
      if isBlank c then adv
      else if c == '\\' && (← ahead 1) == some '\n' then adv 2
      else break
    | none => break
  match ← cur with
  | none => return .eof
  | some '#' =>
    repeat
      match ← cur with
      | some '\n' => break
      | none => break
      | some _ => adv
    lexToken
  | some '\n' => adv; readHeredocs; return .nl
  | some c =>
    if c == '(' && (← ahead 1) == some '(' then
      adv 2; skipBalanced '(' ')' 1
      if (← cur) == some ')' then adv else fail "unterminated (("
      return .arith
    -- IO number: digits directly before `<` or `>`
    if c.isDigit then
      let s ← get
      let mut k := s.pos
      while (s.input[k]?.map Char.isDigit).getD false do k := k + 1
      if s.input[k]? == some '<' || s.input[k]? == some '>' then
        if s.input[k+1]? != some '(' then
          set { s with pos := k }
          return ← lexToken
    if (c == '<' || c == '>') && (← ahead 1) == some '(' then
      return .word (← readWord)
    if isOpChar c then
      for o in operators do
        if ← startsWith o then
          adv o.length
          if o == "<<" then modify fun s => { s with expectDelim := some false }
          if o == "<<-" then modify fun s => { s with expectDelim := some true }
          return .op o
      fail "operator"; return .eof
    let w ← readWord
    match (← get).expectDelim with
    | some strip =>
      let delim := w.parts.foldl (fun acc p => match p with | .lit t _ => acc ++ t | .dyn => acc ++ "$") ""
      modify fun s => { s with expectDelim := none, pending := s.pending.push { delim, stripTabs := strip, quoted := w.anyQuoted } }
    | none => pure ()
    return .word w

partial def peek : P Tok := do
  match (← get).peeked with
  | some (t, _, _) => return t
  | none =>
    let t ← lexToken
    modify fun s => { s with peeked := some (t, s.pos, none) }
    return t

partial def next : P Tok := do
  let t ← peek
  modify fun s => { s with peeked := none }
  return t

partial def skipNewlines : P Unit := do
  while (← peek) == .nl do discard next

partial def atStop (stopWords : List String) (stopOps : List String) : P Bool := do
  match ← peek with
  | .eof => return true
  | .op o => return stopOps.contains o || [";;", ";&", ";;&"].contains o
  | t => return isReserved t stopWords

/-- list (XCU 2.9.3): and-or lists separated by `;`, `&` and newlines, until a stop. -/
partial def parseList (stopWords : List String) (stopOps : List String) : P Unit := do
  repeat
    if (← get).err.isSome then break
    skipNewlines
    if ← atStop stopWords stopOps then break
    match ← peek with
    | .op ";" => discard next
    | .op "&" => discard next
    | _ =>
      let before := (← get).pos
      parseAndOr stopWords stopOps
      match ← peek with
      | .op ";" => discard next
      | .op "&" => discard next
      | .nl => discard next
      | _ =>
        if ← atStop stopWords stopOps then break
        if (← get).pos == before then fail "no progress"; break

partial def parseAndOr (stopWords : List String) (stopOps : List String) : P Unit := do
  parsePipeline stopWords stopOps
  repeat
    match ← peek with
    | .op "&&" => discard next; skipNewlines; parsePipeline stopWords stopOps
    | .op "||" => discard next; skipNewlines; parsePipeline stopWords stopOps
    | _ => break

partial def parsePipeline (stopWords : List String) (stopOps : List String) : P Unit := do
  if isReserved (← peek) ["!"] then discard next
  if isReserved (← peek) ["time"] then
    discard next
    if isReserved (← peek) ["-p"] then discard next
  if isReserved (← peek) ["!"] then discard next
  parseCommand stopWords stopOps
  repeat
    match ← peek with
    | .op "|" => discard next; skipNewlines; parseCommand stopWords stopOps
    | .op "|&" => discard next; skipNewlines; parseCommand stopWords stopOps
    | _ => break

partial def expectWord (name : String) : P Unit := do
  skipNewlines
  if isReserved (← peek) [name] then discard next else fail s!"expected {name}"

partial def redirections : P Unit := do
  repeat
    match ← peek with
    | .op o =>
      if redirOps.contains o then
        discard next
        match ← next with
        | .word _ => pure ()
        | _ => fail "redirection target"
      else break
    | _ => break

partial def parseCompound (w : String) : P Unit := do
  match w with
  | "{" => parseList ["}"] []; expectWord "}"
  | "if" =>
    parseList ["then"] []; expectWord "then"
    parseList ["elif", "else", "fi"] []
    repeat
      if isReserved (← peek) ["elif"] then
        discard next; parseList ["then"] []; expectWord "then"; parseList ["elif", "else", "fi"] []
      else break
    if isReserved (← peek) ["else"] then discard next; parseList ["fi"] []
    expectWord "fi"
  | "while" | "until" =>
    parseList ["do"] []; expectWord "do"; parseList ["done"] []; expectWord "done"
  | "for" | "select" =>
    match ← next with
    | .word _ => pure ()
    | .arith => pure ()
    | _ => fail "for name"
    -- optional `in word...` up to `;` or newline
    skipNewlines
    if isReserved (← peek) ["in"] then
      discard next
      repeat
        match ← peek with
        | .word _ => discard next
        | _ => break
    match ← peek with
    | .op ";" => discard next
    | _ => pure ()
    skipNewlines
    if isReserved (← peek) ["{"] then discard next; parseCompound "{"
    else expectWord "do"; parseList ["done"] []; expectWord "done"
  | "case" =>
    discard next  -- subject
    skipNewlines; expectWord "in"
    repeat
      skipNewlines
      if (← get).err.isSome then break
      if isReserved (← peek) ["esac"] then break
      if (← peek) == .op "(" then discard next
      -- patterns: word (| word)* )
      repeat
        match ← next with
        | .word _ =>
          match ← next with
          | .op "|" => pure ()
          | .op ")" => break
          | _ => fail "case pattern"; break
        | _ => fail "case pattern"; break
      parseList ["esac"] []
      match ← peek with
      | .op ";;" | .op ";&" | .op ";;&" => discard next
      | _ => pure ()
    expectWord "esac"
  | "[[" =>
    repeat
      match ← next with
      | .eof => fail "unterminated [["; break
      | .word w => if w.plain? == some "]]" then break
      | _ => pure ()
  | "function" =>
    discard next  -- name
    if (← peek) == .op "(" then
      discard next
      if (← next) != .op ")" then fail "function ()"
    skipNewlines
    parseCommand [] []
  | "coproc" =>
    -- `coproc NAME compound` or `coproc simple`
    parseCommand [] []
  | _ => fail s!"compound {w}"
  redirections

partial def parseCommand (stopWords : List String) (stopOps : List String) : P Unit := do
  match ← peek with
  | .arith => discard next; redirections
  | .op "(" =>
    discard next; parseList [] [")"]
    if (← next) != .op ")" then fail "unterminated ("
    redirections
  | .word w =>
    match w.plain? with
    | some s =>
      if ["{", "if", "while", "until", "for", "select", "case", "[[", "function", "coproc"].contains s then
        discard next; parseCompound s
      else parseSimple stopWords stopOps
    | none => parseSimple stopWords stopOps
  | .op o => if redirOps.contains o then parseSimple stopWords stopOps else fail s!"unexpected {o}"
  | _ => pure ()

/-- simple_command (XCU 2.9.1): assignments and redirections, then words. A first
word followed by `()` is a function definition (2.9.5). -/
partial def parseSimple (_stopWords : List String) (_stopOps : List String) : P Unit := do
  let mut words : Array Word := #[]
  repeat
    match ← peek with
    | .word w =>
      if words.isEmpty && isAssignment w then discard next
      else
        discard next
        if words.isEmpty then
          if (← peek) == .op "(" then
            -- function definition: NAME ( ) compound
            discard next
            if (← next) != .op ")" then fail "function ()"; return
            skipNewlines
            parseCommand [] []
            return
        words := words.push w
    | .op o =>
      if redirOps.contains o then
        discard next
        match ← next with
        | .word _ => pure ()
        | _ => fail "redirection target"
      else break
    | _ => break
  if !words.isEmpty then emit { words := words.toList }

partial def parseProgram : P Unit := do
  parseList [] []
  match ← peek with
  | .eof => pure ()
  | t => fail s!"trailing {repr t}"

end

inductive ParseResult where
  | ok (cmds : Array SimpleCmd)
  | error (msg : String)

def parse (text : String) : ParseResult :=
  let (_, s) := parseProgram.run { input := text.toList.toArray }
  match s.err with
  | some e => .error e
  | none => .ok s.out

end ShellSpec
