import Lexical.Lines

/-!
# Comments versus strings (Language Reference §2.1.3, §2.4; PEP 701)

Which `#` starts a COMMENT token. Only the parts of the tokenizer that decide
that are modelled:

* §2.1.3: "A comment starts with a hash character (`#`) that is not part of a
  string literal, and ends at the end of the physical line."
* §2.4.1: string literals, their prefixes (`r u f b` and the two-letter
  combinations, any case), single and triple quotes, and that a backslash
  always takes the next character with it, raw or not ("a raw literal cannot
  end in a single backslash").
* PEP 701 (3.12): an f-string's replacement field is ordinary code, so it may
  hold strings with any quote, nested f-strings, newlines and comments. `{{` and
  `}}` are literal braces; `:` at bracket depth 0 starts the format spec, which
  may hold nested fields. `\N{...}` in a non-raw f-string is a named escape,
  and a backslash before `{` or `}` leaves the brace to the f-string.
* §2.1.5: a backslash before an end-of-line joins the lines.

Anything the tokenizer would reject (an unterminated string, a lone `}` in an
f-string) is reported as `error`; CPython raises SyntaxError for the file.
-/

namespace Lexical

structure Comment where
  line : Nat
  /-- From the `#` up to, not including, the end of the physical line. -/
  text : List Char
deriving Repr, BEq

inductive Frame where
  /-- Code inside an f-string replacement field, at this bracket depth. -/
  | field (depth : Nat)
  /-- The format spec of a replacement field. -/
  | spec
  /-- A string literal: its quote character, triple-quoted, raw, f-string. -/
  | str (q : Char) (triple raw fstr : Bool)
deriving Repr, BEq

structure St where
  line : Nat := 1
  /-- Innermost first; empty means top-level code. -/
  stack : List Frame := []
  /-- Comments found so far, last first. -/
  comments : List Comment := []
  /-- Line of the first token that is not a comment or whitespace. -/
  firstCode : Option Nat := none
  error : Option (Nat × String) := none
deriving Repr

def isQuote (c : Char) : Bool := c == '"' || c == '\''

def isIdentStart (c : Char) : Bool := c.isAlpha || c == '_' || c.val ≥ 128

def isIdentChar (c : Char) : Bool := isIdentStart c || c.isDigit

/-- §2.4.1 `stringprefix`, compared lowercased. -/
def prefixFlags (p : List Char) : Option (Bool × Bool) :=
  -- (raw, fstring)
  match p.map Char.toLower with
  | [] => some (false, false)
  | ['r'] => some (true, false)
  | ['u'] => some (false, false)
  | ['b'] => some (false, false)
  | ['f'] => some (false, true)
  | ['b', 'r'] | ['r', 'b'] => some (true, false)
  | ['f', 'r'] | ['r', 'f'] => some (true, true)
  | _ => none

/-- One end-of-line unit at the head: CR LF together, or one character. -/
def unitLen : List Char → Nat
  | '\r' :: '\n' :: _ => 2
  | _ => 1

/-- Consume `n` characters, counting the lines they end. -/
def adv (st : St) (n : Nat) (cs : List Char) : St × List Char :=
  ({ st with line := st.line + terminators (cs.take n) }, cs.drop n)

def markCode (st : St) : St :=
  if st.stack.isEmpty && st.firstCode.isNone then { st with firstCode := some st.line } else st

def fail (st : St) (msg : String) : St × List Char := ({ st with error := some (st.line, msg) }, [])

def push (st : St) (f : Frame) : St := { st with stack := f :: st.stack }

def pop (st : St) : St := { st with stack := st.stack.tail }

def setTop (st : St) (f : Frame) : St := { st with stack := f :: st.stack.tail }

/-- Run a step that, by construction, adds no COMMENT token: only `comment`
(a `#` in code) does. -/
def keep (st : St) (r : St × List Char) : St × List Char := ({ r.1 with comments := st.comments }, r.2)

/-- Open a string whose prefix was `raw`/`fstr` at a quote: `cs` starts at it. -/
def openStr (st : St) (raw fstr : Bool) (q : Char) (cs : List Char) : St × List Char :=
  match cs with
  | a :: b :: c :: rest =>
    if a == q && b == q && c == q then (push st (.str q true raw fstr), rest)
    else (push st (.str q false raw fstr), cs.drop 1)
  | _ => (push st (.str q false raw fstr), cs.drop 1)

/-- A step in code on a character other than `#`. Never touches comments. -/
def stepCodeRest (st : St) (depth : Option Nat) (c : Char) (rest cs : List Char) : St × List Char :=
  keep st <|
  if isNL c then adv st (unitLen cs) cs
  else if c == ' ' || c == '\t' || c == '\x0c' then (st, rest)
  else if c == '\\' && (rest.head?.map isNL).getD false then adv st (1 + unitLen rest) cs
  else
    let st := markCode st
    if isQuote c then openStr st false false c cs
    else if isIdentStart c then
      let (ident, tail) := cs.span isIdentChar
      match tail, prefixFlags ident with
      | q :: _, some (raw, fstr) => if isQuote q then openStr st raw fstr q tail else (st, tail)
      | _, _ => (st, tail)
    else if c.isDigit then
      let (_, tail) := cs.span (fun ch => isIdentChar ch || ch == '.')
      (st, tail)
    else match depth with
      | none => (st, rest)
      | some d =>
        if c == '(' || c == '[' || c == '{' then (setTop st (.field (d + 1)), rest)
        else if (c == ')' || c == ']') && d > 0 then (setTop st (.field (d - 1)), rest)
        else if c == '}' then
          if d == 0 then (pop st, rest) else (setTop st (.field (d - 1)), rest)
        else if c == ':' && d == 0 then (setTop st .spec, rest)
        else (st, rest)

/-- The COMMENT token at a `#`: up to the end of the physical line (§2.1.3). -/
def comment (st : St) (rest : List Char) : St × List Char :=
  let body := rest.takeWhile (fun ch => !isNL ch)
  ({ st with comments := { line := st.line, text := '#' :: body } :: st.comments },
   rest.dropWhile (fun ch => !isNL ch))

/-- A step in code (top level, or a replacement field at `depth`). -/
def stepCode (st : St) (depth : Option Nat) (cs : List Char) : St × List Char :=
  match cs with
  | [] => (st, [])
  | c :: rest => if c == '#' then comment st rest else stepCodeRest st depth c rest cs

/-- The closing quote of a string frame, when `cs` starts with it. -/
def closes (q : Char) (triple : Bool) (cs : List Char) : Option (List Char) :=
  if triple then
    match cs with
    | a :: b :: c :: rest => if a == q && b == q && c == q then some rest else none
    | _ => none
  else
    match cs with
    | a :: rest => if a == q then some rest else none
    | _ => none

/-- A backslash in a string body (`cs` starts at it). -/
def stepBackslash (st : St) (raw fstr : Bool) (cs : List Char) : St × List Char :=
  keep st <|
  match cs with
  | _ :: rest =>
    match rest with
    | [] => fail st "unterminated string literal"
    | n :: more =>
      if fstr && (n == '{' || n == '}') then (st, rest)
      else if fstr && !raw && n == 'N' && more.head? == some '{' then
        let (name, tail) := more.span (· != '}')
        if tail.isEmpty then fail st "unterminated \\N{...}"
        else adv st (2 + name.length + 1) cs
      else adv st (1 + unitLen rest) cs
  | [] => (st, [])

/-- A step inside a string literal body. -/
def stepStr (st : St) (q : Char) (triple raw fstr : Bool) (cs : List Char) : St × List Char :=
  keep st <|
  match closes q triple cs with
  | some rest => (pop st, rest)
  | none =>
    match cs with
    | [] => fail st "unterminated string literal"
    | c :: rest =>
      if c == '\\' then stepBackslash st raw fstr cs
      else if isNL c then
        if triple then adv st (unitLen cs) cs else fail st "unterminated string literal"
      else if fstr && c == '{' then
        if rest.head? == some '{' then (st, rest.drop 1) else (push st (.field 0), rest)
      else if fstr && c == '}' then
        if rest.head? == some '}' then (st, rest.drop 1) else fail st "f-string: single '}' is not allowed"
      else (st, rest)

/-- The f-string frame a format spec belongs to (the first `str` below it). -/
def enclosingStr : List Frame → Option (Char × Bool × Bool)
  | [] => none
  | .str q triple raw true :: _ => some (q, triple, raw)
  | _ :: rest => enclosingStr rest

/-- A step inside a format spec. -/
def stepSpec (st : St) (cs : List Char) : St × List Char :=
  keep st <|
  match cs with
  | [] => fail st "f-string: expecting '}'"
  | c :: rest =>
    let (q, triple, raw) := (enclosingStr st.stack).getD ('"', false, false)
    if (closes q triple cs).isSome then fail st "f-string: expecting '}'"
    else if c == '}' then (pop st, rest)
    else if c == '{' then (push st (.field 0), rest)
    else if c == '\\' then stepBackslash st raw true cs
    else if isNL c then
      if triple then adv st (unitLen cs) cs else fail st "f-string: expecting '}'"
    else (st, rest)

def step (st : St) (cs : List Char) : St × List Char :=
  match st.stack with
  | [] => stepCode st none cs
  | .field d :: _ => stepCode st (some d) cs
  | .spec :: _ => stepSpec st cs
  | .str q triple raw fstr :: _ => stepStr st q triple raw fstr cs

/-- Run with fuel; every step on a non-empty input consumes at least one
character, so `cs.length` steps suffice. -/
def run : Nat → St → List Char → St
  | 0, st, _ => st
  | _ + 1, st, [] =>
    if st.stack.isEmpty || st.error.isSome then st
    else { st with error := some (st.line, "unterminated string literal") }
  | fuel + 1, st, cs =>
    if st.error.isSome then st
    else
      let (st', rest) := step st cs
      run fuel st' rest

structure Lexed where
  comments : List Comment
  firstCode : Option Nat
  error : Option (Nat × String)
  lines : Nat
deriving Repr

def lex (s : List Char) : Lexed :=
  let st := run (s.length + 1) {} s
  { comments := st.comments.reverse, firstCode := st.firstCode, error := st.error,
    lines := terminators s + 1 }

end Lexical
