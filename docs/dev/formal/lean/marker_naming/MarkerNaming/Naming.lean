/-!
Distribution / extra name normalization, from PEP 503 ("Normalized
Names": `re.sub(r"[-_.]+", "-", name).lower()`) and PEP 685 (extras use the
same rule). Defined on character lists so the properties below can be
proved by induction.
-/
namespace MarkerNaming.Naming

def isSep (c : Char) : Bool := c == '-' || c == '_' || c == '.'

/-- ASCII lowercase (project names are ASCII per PEP 508). -/
def lower : Char → Char
  | 'A' => 'a'
  | 'B' => 'b'
  | 'C' => 'c'
  | 'D' => 'd'
  | 'E' => 'e'
  | 'F' => 'f'
  | 'G' => 'g'
  | 'H' => 'h'
  | 'I' => 'i'
  | 'J' => 'j'
  | 'K' => 'k'
  | 'L' => 'l'
  | 'M' => 'm'
  | 'N' => 'n'
  | 'O' => 'o'
  | 'P' => 'p'
  | 'Q' => 'q'
  | 'R' => 'r'
  | 'S' => 's'
  | 'T' => 't'
  | 'U' => 'u'
  | 'V' => 'v'
  | 'W' => 'w'
  | 'X' => 'x'
  | 'Y' => 'y'
  | 'Z' => 'z'
  | c => c

theorem dropWhile_length_le (p : Char → Bool) : ∀ l : List Char, (l.dropWhile p).length ≤ l.length
  | [] => by simp
  | c :: cs => by
    simp only [List.dropWhile]
    split
    · have := dropWhile_length_le p cs; simp; omega
    · simp

def norm : List Char → List Char
  | [] => []
  | c :: cs =>
    if isSep c then '-' :: norm (cs.dropWhile isSep)
    else lower c :: norm cs
termination_by l => l.length
decreasing_by
  all_goals simp_wf
  all_goals
    first
    | omega
    | (have h := dropWhile_length_le isSep cs; omega)

def normalize (s : String) : String := String.mk (norm s.toList)

end MarkerNaming.Naming
