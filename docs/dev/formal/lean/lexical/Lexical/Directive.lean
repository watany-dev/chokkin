import Lexical.Tokens

/-!
# `# chokkin:` directives (spec §18) over COMMENT tokens

The grammar is spec §18's: `# chokkin: ignore[CODES]` and
`# chokkin: file-ignore[CODES]`, CODES a comma list without spaces, lower case
and a space before `[` not accepted. Whitespace is spec §18's "`chokkin :` is
not accepted" read as ASCII blanks only (the comment-start pattern of #1132
uses `[ \t]`).

What this file adds to spec §18 is the lexical premise every comment-directive
tool (flake8 `noqa`, mypy `type: ignore`, ruff) shares and §2.1.3 of the
Language Reference defines: a directive is a COMMENT token. Text inside a
string literal is not a comment, and a comment never reaches past its
physical line.

Placement (spec §18): `file-ignore` counts only in the comment block before
the first statement; `ignore` counts only on or after the first statement's
line, for its own physical line.
-/

namespace Lexical

def isBlank (c : Char) : Bool := c == ' ' || c == '\t'

def isUpper (c : Char) : Bool := 'A' ≤ c && c ≤ 'Z'

def isCodeChar (c : Char) : Bool := isUpper c || c.isDigit || c == '_'

def expect : List Char → List Char → Option (List Char)
  | [], cs => some cs
  | p :: ps, c :: cs => if p == c then expect ps cs else none
  | _ :: _, [] => none

/-- `[A-Z][A-Z0-9_]*`, the longest run. -/
def code (cs : List Char) : Option (List Char × List Char) :=
  match cs with
  | c :: _ => if isUpper c then some (cs.span isCodeChar) else none
  | [] => none

/-- `CODE(,CODE)*]`; fuel bounds the list by the input length. -/
def codes : Nat → List Char → Option (List (List Char))
  | 0, _ => none
  | fuel + 1, cs => do
    let (c, rest) ← code cs
    match rest with
    | ']' :: _ => some [c]
    | ',' :: more => (c :: ·) <$> codes fuel more
    | _ => none

structure Directive where
  fileLevel : Bool
  codes : List String
deriving Repr, BEq

/-- A directive starting at this `#`: `#[ \t]*chokkin:[ \t]*(file-)?ignore[...]`. -/
def directiveAt (cs : List Char) : Option Directive := do
  let rest ← expect ['#'] cs
  let rest ← expect "chokkin:".toList (rest.dropWhile isBlank)
  let rest := rest.dropWhile isBlank
  let (fileLevel, rest) :=
    match expect "file-".toList rest with
    | some r => (true, r)
    | none => (false, rest)
  let rest ← expect "ignore[".toList rest
  let cs ← codes (rest.length + 1) rest
  pure { fileLevel, codes := cs.map String.mk }

/-- `#[ \t]*chokkin[ \t]*:`: a comment that starts like a directive (#1132). -/
def looksLikeDirective (cs : List Char) : Bool :=
  match expect ['#'] cs with
  | none => false
  | some rest =>
    match expect "chokkin".toList (rest.dropWhile isBlank) with
    | none => false
    | some r => (r.dropWhile isBlank).head? == some ':'

/-- Every suffix of a comment that starts at a `#`. -/
def hashSuffixes : List Char → List (List Char)
  | [] => []
  | c :: rest => (if c == '#' then [c :: rest] else []) ++ hashSuffixes rest

/-- CHK001..CHK010, the codes `RuleId::parse_code` knows (spec §3). -/
def knownCode (c : String) : Bool :=
  ["CHK001", "CHK002", "CHK003", "CHK004", "CHK005", "CHK006", "CHK007", "CHK008",
   "CHK009", "CHK010"].contains c

/-- `CHK` and three digits: the codes kept (spec §18: an unknown one warns). -/
def chkShape (c : String) : Bool :=
  let cs := c.toList
  cs.length == 6 && cs.take 3 == "CHK".toList && (cs.drop 3).all Char.isDigit

inductive Warn where
  | unrecognized | unknownCode | lateFileIgnore | earlyIgnore
deriving Repr, BEq

def Warn.name : Warn → String
  | .unrecognized => "unrecognized"
  | .unknownCode => "unknown-code"
  | .lateFileIgnore => "late-file-ignore"
  | .earlyIgnore => "early-ignore"

structure Effects where
  /-- (physical line, codes) of each inline ignore that counts. -/
  inline : List (Nat × List String) := []
  fileCodes : List String := []
  warnings : List (Nat × Warn) := []
deriving Repr

/-- Spec §18 for one parsed directive in a comment on `line`. `firstStmt` is
the line of the first token that is not a comment; `none` (no statement) puts
every comment in the header. -/
def applyDirective (firstStmt : Option Nat) (line : Nat) (acc : Effects) (d : Directive) : Effects :=
  let acc :=
    if d.codes.any (fun c => !knownCode c) then
      { acc with warnings := acc.warnings ++ [(line, .unknownCode)] }
    else acc
  let kept := d.codes.filter chkShape
  if kept.isEmpty then acc
  else
    let inHeader := match firstStmt with
      | none => true
      | some l => line < l
    if d.fileLevel then
      if inHeader then { acc with fileCodes := acc.fileCodes ++ kept }
      else { acc with warnings := acc.warnings ++ [(line, .lateFileIgnore)] }
    else if inHeader then { acc with warnings := acc.warnings ++ [(line, .earlyIgnore)] }
    else { acc with inline := acc.inline ++ [(line, kept)] }

/-- One `#`-suffix of a comment on `line`. -/
def onSuffix (firstStmt : Option Nat) (line : Nat) (acc : Effects) (sfx : List Char) : Effects :=
  match directiveAt sfx with
  | none =>
    if looksLikeDirective sfx then { acc with warnings := acc.warnings ++ [(line, .unrecognized)] }
    else acc
  | some d => applyDirective firstStmt line acc d

def onComment (firstStmt : Option Nat) (acc : Effects) (cm : Comment) : Effects :=
  (hashSuffixes cm.text).foldl (onSuffix firstStmt cm.line) acc

/-- Spec §18 applied to the COMMENT tokens of a file. -/
def effects (comments : List Comment) (firstStmt : Option Nat) : Effects :=
  comments.foldl (onComment firstStmt) {}

/-- Whether a finding of `code` on physical line `line` is suppressed. -/
def suppressed (e : Effects) (code : String) (line : Nat) : Bool :=
  e.fileCodes.contains code || e.inline.any (fun (l, cs) => l == line && cs.contains code)

end Lexical
