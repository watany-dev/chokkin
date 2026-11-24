import Lexical.Lines

/-!
# PEP 723 inline script metadata blocks

The PEP's reference regex, applied to the script text as Python reads it
(`open()` in text mode, so universal newlines: `Lexical.univ`):

    (?m)^# /// (?P<type>[a-zA-Z0-9-]+)$\s(?P<content>(^#(| .*)$\s)+)^# ///$

With MULTILINE, `^`/`$` sit at line boundaries and `.` is anything but LF, so
the regex is a statement about lines: an opening line `# /// TYPE`, one or more
content lines (`#` alone or `# ` + anything), and a closing `# ///` that the
greedy `+` puts at the last `# ///` of the run of content-shaped lines (the
closing line is itself content-shaped), with at least one content line before
it. `re.finditer` resumes after the closing line.
-/

namespace Lexical

def isAsciiAlnum' (c : Char) : Bool := c.isAlpha || c.isDigit

def openingType (l : List Char) : Option (List Char) :=
  let p := "# /// ".toList
  if l.take p.length == p then
    let ty := l.drop p.length
    if !ty.isEmpty && ty.all (fun c => isAsciiAlnum' c || c == '-') then some ty else none
  else none

def contentLine (l : List Char) : Bool := l == ['#'] || l.take 2 == ['#', ' ']

def closingLine (l : List Char) : Bool := l == "# ///".toList

structure Block where
  /-- 1-based physical lines of the opening and closing lines. -/
  opening : Nat
  closing : Nat
  type : String
deriving Repr

/-- Index (into `run`) of the last closing line that has a content line before it. -/
def lastClosing (run : List (List Char)) : Option Nat :=
  ((List.range run.length).filter (fun i => i ≥ 1 && closingLine (run.getD i []))).getLast?

def blocksFrom : Nat → Nat → List (List Char) → List Block
  | 0, _, _ => []
  | _, _, [] => []
  | fuel + 1, n, l :: rest =>
    match openingType l with
    | none => blocksFrom fuel (n + 1) rest
    | some ty =>
      let run := rest.takeWhile contentLine
      match lastClosing run with
      | none => blocksFrom fuel (n + 1) rest
      | some i =>
        { opening := n, closing := n + 1 + i, type := String.mk ty } ::
          blocksFrom fuel (n + 2 + i) (rest.drop (i + 1))

/-- PEP 723 blocks of a script, by physical line. -/
def scriptBlocks (s : List Char) : List Block :=
  let ls := physLines (univ s)
  blocksFrom (ls.length + 1) 1 ls

end Lexical
