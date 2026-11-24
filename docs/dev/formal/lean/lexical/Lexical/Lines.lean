/-!
# Physical lines (Python Language Reference §2.1.2)

"A physical line is a sequence of characters terminated by an end-of-line
sequence. In source files and strings, any of the standard platform line
termination sequences can be used - the Unix form using ASCII LF (linefeed),
the Windows form using the ASCII sequence CR LF (return followed by linefeed),
or the old Macintosh form using the ASCII CR (return) character. All of these
forms can be used equally, regardless of platform."

Nothing else ends a physical line: form feed, VT, NEL (U+0085) and
U+2028/U+2029 are ordinary characters to the tokenizer.

Written from the reference text, not from `src/parser/lines.rs`.
-/

namespace Lexical

/-- The two characters that can take part in an end-of-line sequence. -/
def isNL (c : Char) : Bool := c == '\n' || c == '\r'

/-- Number of end-of-line sequences in `s`; CR LF counts once. -/
def terminators : List Char → Nat
  | [] => 0
  | '\r' :: '\n' :: rest => terminators rest + 1
  | c :: rest => terminators rest + (if isNL c then 1 else 0)

/-- 1-based physical line of the character at index `i`. -/
def lineAt (s : List Char) (i : Nat) : Nat := terminators (s.take i) + 1

/-- Universal-newline translation, as `open()` in text mode does: CR LF and a
lone CR both become LF. The reference says the three forms are equal, so line
numbering must not change under it (`terminators_univ`). -/
def univ : List Char → List Char
  | [] => []
  | '\r' :: '\n' :: rest => '\n' :: univ rest
  | '\r' :: rest => '\n' :: univ rest
  | c :: rest => c :: univ rest

/-- Split into physical lines, without their terminators. A final terminator
does not open an empty last line; an empty input has no line. -/
def physLinesAux : List Char → List Char → List (List Char)
  | [], cur => if cur.isEmpty then [] else [cur.reverse]
  | '\r' :: '\n' :: rest, cur => cur.reverse :: physLinesAux rest []
  | c :: rest, cur =>
    if isNL c then cur.reverse :: physLinesAux rest [] else physLinesAux rest (c :: cur)

def physLines (s : List Char) : List (List Char) := physLinesAux s []

end Lexical
