import Lexical.Lines
import Lexical.CodecTable

/-!
# Source encoding (PEP 263, Language Reference §2.1.4)

"If a comment in the first or second line of the Python script matches the
regular expression `coding[=:]\s*([-\w.]+)`, this comment is processed as an
encoding declaration [...]. The encoding declaration must appear on a line of
its own. If it is the second line, the first line must also be a comment-only
line." (§2.1.4). "If the first bytes of the file are the UTF-8 byte-order mark
(`b'\xef\xbb\xbf'`), the declared file encoding is UTF-8" and a different
declaration is an error. No declaration: UTF-8.

The declared name goes through `codecs.lookup`: CPython's tokenizer first maps
`utf-8*` and `latin-1*` spellings itself (`get_normal_name`), then
`encodings.search_function` normalizes the name (`normalize_encoding`) and
looks it up in `encodings.aliases`, then as a codec module. The alias table is
`Lexical/CodecTable.lean`, generated from CPython's own `encodings`.

Lines are physical lines (§2.1.2). CPython's tokenizer splits the first two
lines at LF only; the two agree on every input without a lone CR, which is
what the differential feeds both.
-/

namespace Lexical

def isAsciiAlnum (c : Char) : Bool := c.isAlpha || c.isDigit

/-- First two physical lines of the byte string read as Latin-1 (ASCII is all
that matters for the declaration). -/
def firstTwoLines (s : List Char) : List (List Char) := (physLines s).take 2

/-- `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)` on one line, first match. -/
def codingIn (line : List Char) : Option (List Char) :=
  match line.dropWhile (fun c => c == ' ' || c == '\t' || c == '\x0c') with
  | '#' :: rest => search rest.length rest
  | _ => none
where
  search : Nat → List Char → Option (List Char)
    | 0, _ => none
    | fuel + 1, cs =>
      match cs with
      | [] => none
      | _ :: tail =>
        match expect' "coding".toList cs with
        | some (c :: after) =>
          if c == ':' || c == '=' then
            let name := (after.dropWhile (fun ch => ch == ' ' || ch == '\t')).takeWhile
              (fun ch => isAsciiAlnum ch || ch == '-' || ch == '_' || ch == '.')
            if name.isEmpty then search fuel tail else some name
          else search fuel tail
        | _ => search fuel tail
  expect' : List Char → List Char → Option (List Char)
    | [], cs => some cs
    | p :: ps, c :: cs => if p == c then expect' ps cs else none
    | _ :: _, [] => none

/-- A line that is blank or only a comment: `^[ \t\f]*(#.*)?$`. -/
def commentOrBlank (line : List Char) : Bool :=
  match line.dropWhile (fun c => c == ' ' || c == '\t' || c == '\x0c') with
  | [] => true
  | '#' :: _ => true
  | _ => false

/-- The declared name, from the first line or (after a comment-only or blank
first line) the second. -/
def declared (s : List Char) : Option (List Char) :=
  match firstTwoLines s with
  | [] => none
  | [l1] => codingIn l1
  | l1 :: l2 :: _ =>
    match codingIn l1 with
    | some n => some n
    | none => if commentOrBlank l1 then codingIn l2 else none

/-- CPython `get_normal_name`: the first 12 characters, lowercased, `_` as `-`. -/
def normalName (n : List Char) : List Char :=
  let buf := (n.take 12).map (fun c => if c == '_' then '-' else c.toLower)
  let isOrDash (p : String) : Bool :=
    buf == p.toList || buf.take (p.length + 1) == (p ++ "-").toList
  if isOrDash "utf-8" then "utf-8".toList
  else if isOrDash "latin-1" || isOrDash "iso-8859-1" || isOrDash "iso-latin-1" then
    "iso-8859-1".toList
  else n

/-- `encodings.normalize_encoding`: runs of non-alphanumerics other than `.`
become one `_`, none at either end. -/
def normalizeEncoding (n : List Char) : List Char :=
  let rec go : List Char → Bool → List Char → List Char
    | [], _, acc => acc.reverse
    | c :: rest, punct, acc =>
      if isAsciiAlnum c || c == '.' then
        go rest false (c :: (if punct && !acc.isEmpty then '_' :: acc else acc))
      else go rest true acc
  go n false []

/-- `codecs.lookup(name)` as `encodings.search_function` resolves it: the codec
module, or `none` for an unknown encoding. -/
def lookupCodec (n : List Char) : Option String :=
  let norm := normalizeEncoding (n.map Char.toLower)
  let under := norm.map (fun c => if c == '.' then '_' else c)
  let alias := (aliasTable.lookup (String.mk norm)).orElse fun _ => aliasTable.lookup (String.mk under)
  let candidates := (alias.toList) ++ [String.mk under]
  candidates.find? (fun m => textCodecs.contains m)

inductive Decision where
  /-- UTF-8: no declaration, a `utf-8` one, or a BOM. -/
  | utf8 (bom : Bool)
  /-- A declared codec other than the tokenizer's own UTF-8. -/
  | codec (module : String)
  /-- SyntaxError: unknown encoding. -/
  | unknown (name : String)
  /-- SyntaxError: a BOM and a declaration other than `utf-8`. -/
  | bomMismatch (name : String)
deriving Repr, BEq

def bom : List Char := ['\xef', '\xbb', '\xbf']

/-- How CPython decodes a source file whose bytes, read as Latin-1, are `s`. -/
def decide (s : List Char) : Decision :=
  let hasBom := s.take 3 == bom
  let body := if hasBom then s.drop 3 else s
  match declared body with
  | none => .utf8 hasBom
  | some raw =>
    let n := normalName raw
    if hasBom then
      if n == "utf-8".toList then .utf8 true else .bomMismatch (String.mk n)
    else if n == "utf-8".toList then .utf8 false
    else if n == "iso-8859-1".toList then .codec "latin_1"
    else match lookupCodec n with
      | some m => .codec m
      | none => .unknown (String.mk n)

end Lexical
