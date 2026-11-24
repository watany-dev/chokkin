/-
Python `fnmatch.fnmatchcase` (Library Reference, `fnmatch` module).

Written from the module documentation and `fnmatch.translate`, not from
chokkin: `*` matches any run of characters (`/` and `.` included), `?` any
one character, `[seq]` / `[!seq]` a character class, and a `[` with no
closing `]` is a literal `[`. There is no escape character and no brace
alternation: `\` and `{` `}` are ordinary characters.

This is the dialect of setuptools `packages.find` `include` / `exclude`
(`fnmatchcase` on dotted names) and of pytest `python_files`.
-/
namespace Patterns.Fnmatch

inductive Item where
  | one (c : Char)
  | range (lo hi : Char)
  deriving Repr, DecidableEq

def Item.hits (c : Char) : Item → Bool
  | .one d => c == d
  | .range lo hi => lo ≤ c && c ≤ hi

inductive Tok where
  | lit (c : Char)
  | any
  | star
  | cls (neg : Bool) (items : List Item)
  deriving Repr, DecidableEq

/-- Items of a class body. A `-` between two members is a range; a range
whose ends are reversed matches nothing (`translate` drops it). -/
def items : List Char → List Item
  | a :: '-' :: b :: rest => .range a b :: items rest
  | a :: rest => .one a :: items rest
  | [] => []

/-- Split off a class body after `[`: `]` right after `[` or `[!` is a
member. `none` when no `]` closes it. -/
def classBody (cs : List Char) : Option (Bool × List Char × List Char) :=
  let (neg, cs) := match cs with
    | '!' :: rest => (true, rest)
    | _ => (false, cs)
  let (first, cs) := match cs with
    | ']' :: rest => ([']'], rest)
    | _ => ([], cs)
  match cs.dropWhile (· != ']') with
  | ']' :: after => some (neg, first ++ cs.takeWhile (· != ']'), after)
  | _ => none

/-- Tokens of a pattern; `fuel` bounds the recursion (the length suffices). -/
def parseF : Nat → List Char → List Tok
  | 0, _ => []
  | _, [] => []
  | n + 1, '*' :: rest => .star :: parseF n rest
  | n + 1, '?' :: rest => .any :: parseF n rest
  | n + 1, '[' :: rest =>
    match classBody rest with
    | some (neg, body, after) => .cls neg (items body) :: parseF n after
    | none => .lit '[' :: parseF n rest
  | n + 1, c :: rest => .lit c :: parseF n rest

def parse (p : String) : List Tok := parseF (p.length + 1) p.toList

def tokHits (c : Char) : Tok → Bool
  | .lit d => c == d
  | .any => true
  | .star => true
  | .cls neg is => is.any (·.hits c) != neg

/-- Whole-string match of a token list. -/
def mt : List Tok → List Char → Bool
  | [], s => s.isEmpty
  | .star :: p, [] => mt p []
  | .star :: p, c :: s => mt p (c :: s) || mt (.star :: p) s
  | t :: p, c :: s => tokHits c t && mt p s
  | _ :: _, [] => false
termination_by p s => (p.length, s.length)

def fnmatchcase (pat name : String) : Bool := mt (parse pat) name.toList

/-! ## Properties of the specification -/

theorem mt_nil_star : ∀ p : List Tok, mt (.star :: p) [] = mt p [] := by
  intro p; simp [mt]

/-- A lone `*` matches every string, `/` and `.` included. -/
theorem star_all : ∀ s : List Char, mt [.star] s = true := by
  intro s
  induction s with
  | nil => simp [mt]
  | cons c s ih => simp [mt, ih]

/-- A literal prefix followed by `*` matches that prefix and anything after
it: the `*` of `check_*.py`-style patterns crosses `/`. -/
theorem lit_prefix_star : ∀ (pre s : List Char),
    mt (pre.map .lit ++ [.star]) (pre ++ s) = true := by
  intro pre s
  induction pre with
  | nil => simpa using star_all s
  | cons c pre ih => simp [mt, tokHits, ih]

/-- A pattern of literals matches exactly itself. -/
theorem lits_exact : ∀ (pre s : List Char), mt (pre.map .lit) s = true ↔ s = pre := by
  intro pre
  induction pre with
  | nil => intro s; cases s <;> simp [mt]
  | cons c pre ih =>
    intro s
    cases s with
    | nil => simp [mt]
    | cons d s =>
      simp [mt, tokHits, ih]

/-- Characters fnmatch gives a meaning to. Braces and `\` are not among them. -/
def isMeta (c : Char) : Bool := c == '*' || c == '?' || c == '['

theorem parseF_plain : ∀ (n : Nat) (cs : List Char), cs.length < n →
    cs.all (fun c => !isMeta c) = true → parseF n cs = cs.map .lit := by
  intro n
  induction n with
  | zero => intro cs h; simp at h
  | succ n ih =>
    intro cs hlen hall
    cases cs with
    | nil => simp [parseF]
    | cons c cs =>
      simp [List.all_cons] at hall
      obtain ⟨hc, hrest⟩ := hall
      have h1 : c ≠ '*' := by intro h; simp [h, isMeta] at hc
      have h2 : c ≠ '?' := by intro h; simp [h, isMeta] at hc
      have h3 : c ≠ '[' := by intro h; simp [h, isMeta] at hc
      have hl : cs.length < n := by simp at hlen; omega
      have := ih cs hl (by simpa using hrest)
      simp [parseF, h1, h2, h3, this]

/-- `{a,b}` and `\x` have no meaning: a pattern without `*`, `?` or `[`
matches only the identical name. -/
theorem plain_is_literal (p s : String) (h : p.toList.all (fun c => !isMeta c) = true) :
    fnmatchcase p s = true ↔ s.toList = p.toList := by
  unfold fnmatchcase parse
  rw [parseF_plain _ _ (by simp [String.length]) h]
  exact lits_exact _ _

example : fnmatchcase "check_*.py" "check_x/helper.py" = true := by native_decide
example : fnmatchcase "check_{a,b}.py" "check_a.py" = false := by native_decide
example : fnmatchcase "check_{a,b}.py" "check_{a,b}.py" = true := by native_decide
example : fnmatchcase "acme.[!t]*" "acme.core" = true := by native_decide
example : fnmatchcase "a[" "a[" = true := by native_decide

end Patterns.Fnmatch
