/-
gitignore(5) and git's `wildmatch` with `WM_PATHNAME`.

Written from the gitignore(5) manual (PATTERN FORMAT) and the documented
behaviour of `wildmatch`, not from chokkin:

* a blank line or one starting with `#` is no pattern; trailing spaces are
  dropped unless escaped with `\`; a trailing CR is dropped;
* `!` negates; a trailing `/` matches only directories;
* a pattern with a `/` at the start or in the middle is relative to the
  directory of the `.gitignore`; otherwise it matches the name at any level;
* `*` and `?` and `[...]` never match `/`; `**/` matches zero or more
  directories, a trailing `/**` everything inside, and other runs of `*`
  are a single `*`; `\x` is a literal `x`; braces are literal;
* the manual leaves one case open that `git` settles (`match_pathname`):
  the literal prefix of a pattern with a `/` is compared as text, and the
  rest is matched as if it started the pattern, so `a**/b` acts like
  `a` followed by `**/b`;
* a `[` that is never closed makes the whole pattern match nothing;
* within one file the last matching pattern wins; a deeper `.gitignore`
  wins over a shallower one; a file under an excluded directory is
  excluded whatever a later `!` says ("It is not possible to re-include a
  file if a parent directory of that file is excluded").
-/
namespace Patterns.Gitignore

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
  /-- a run of non-`/` characters -/
  | star
  /-- any run, `/` included (a delimited `**` at the end) -/
  | dstar
  /-- `**/`: nothing, or any run that ends with `/` -/
  | dstarSlash
  | cls (neg : Bool) (items : List Item)
  deriving Repr, DecidableEq

/-- Class members after `[` / `[!` / `[^`, up to the closing `]`; `\`
escapes the next character. Returns the items and the rest after `]`. -/
def classItemsF : Nat → Bool → List Char → Option (List Item × List Char)
  | 0, _, _ => none
  | _, _, [] => none
  | _ + 1, false, ']' :: rest => some ([], rest)
  | n + 1, _, '\\' :: c :: rest =>
    match rest with
    | '-' :: d :: rest' =>
      if d == ']' then
        (classItemsF n false rest).map fun (is, r) => (.one c :: is, r)
      else
        let (d, rest'') := match d, rest' with
          | '\\', e :: r => (e, r)
          | _, _ => (d, rest')
        (classItemsF n false rest'').map fun (is, r) => (.range c d :: is, r)
    | _ => (classItemsF n false rest).map fun (is, r) => (.one c :: is, r)
  | n + 1, _, c :: '-' :: d :: rest =>
    if d == ']' then
      (classItemsF n false ('-' :: d :: rest)).map fun (is, r) => (.one c :: is, r)
    else
      let (d, rest') := match d, rest with
        | '\\', e :: r => (e, r)
        | _, _ => (d, rest)
      (classItemsF n false rest').map fun (is, r) => (.range c d :: is, r)
  | n + 1, _, c :: rest => (classItemsF n false rest).map fun (is, r) => (.one c :: is, r)

/-- Tokens of a wildmatch pattern, `none` when a `[` is never closed or the
pattern ends in a lone `\` (both match nothing). `prevSlash` says whether
the previous character was `/` or the start. -/
def parseF : Nat → Bool → List Char → Option (List Tok)
  | 0, _, _ => some []
  | _, _, [] => some []
  | n + 1, _, '\\' :: c :: rest => (parseF n (c == '/') rest).map (.lit c :: ·)
  | _ + 1, _, ['\\'] => none
  | n + 1, prev, '*' :: '*' :: rest =>
    let rest := rest.dropWhile (· == '*')
    if prev then
      match rest with
      | [] => some [.dstar]
      | '/' :: after => (parseF n true after).map (.dstarSlash :: ·)
      | '\\' :: '/' :: after => (parseF n true after).map (fun t => .dstar :: .lit '/' :: t)
      | _ => (parseF n false rest).map (.star :: ·)
    else (parseF n false rest).map (.star :: ·)
  | n + 1, _, '*' :: rest => (parseF n false rest).map (.star :: ·)
  | n + 1, _, '?' :: rest => (parseF n false rest).map (.any :: ·)
  | n + 1, _, '[' :: rest =>
    let (neg, body) := match rest with
      | '!' :: r => (true, r)
      | '^' :: r => (true, r)
      | r => (false, r)
    match classItemsF (body.length + 1) true body with
    | some (is, after) => (parseF n false after).map (.cls neg is :: ·)
    | none => none
  | n + 1, _, c :: rest => (parseF n (c == '/') rest).map (.lit c :: ·)

def parse (p : List Char) : Option (List Tok) := parseF (p.length + 1) true p

def tokHits (c : Char) : Tok → Bool
  | .lit d => c == d
  | .any => c != '/'
  | .cls neg is => c != '/' && (is.any (·.hits c) != neg)
  | _ => false

def Tok.wt : Tok → Nat
  | .dstarSlash => 3
  | _ => 1

def wt : List Tok → Nat
  | [] => 0
  | t :: p => t.wt + wt p

def mt : List Tok → List Char → Bool
  | [], s => s.isEmpty
  | .star :: p, [] => mt p []
  | .star :: p, c :: s => mt p (c :: s) || (c != '/' && mt (.star :: p) s)
  | .dstar :: p, [] => mt p []
  | .dstar :: p, c :: s => mt p (c :: s) || mt (.dstar :: p) s
  | .dstarSlash :: p, s => mt p s || mt (.dstar :: .lit '/' :: p) s
  | t :: p, c :: s => tokHits c t && mt p s
  | _ :: _, [] => false
termination_by p s => (wt p, s.length)
decreasing_by all_goals simp_wf <;> simp [wt, Tok.wt] <;> omega

def wildmatch (pat text : List Char) : Bool :=
  match parse pat with
  | some toks => mt toks text
  | none => false

structure Pat where
  neg : Bool
  dirOnly : Bool
  /-- no `/` before the trailing one: match the basename at any depth -/
  basename : Bool
  body : List Char
  deriving Repr

/-- git's `trim_trailing_spaces`: drop the trailing run of spaces unless the
last one is escaped. -/
def trimTrailing (cs : List Char) : List Char :=
  let rec go : List Char → Option Nat → Nat → Option Nat
    | [], last, _ => last
    | ' ' :: r, last, i => go r (last <|> some i) (i + 1)
    | '\\' :: [], _, _ => none
    | '\\' :: _ :: r, _, i => go r none (i + 2)
    | _ :: r, _, i => go r none (i + 1)
  match go cs none 0 with
  | some n => cs.take n
  | none => cs

def parseLine (line : List Char) : Option Pat :=
  let line := if line.getLast? == some '\r' then line.dropLast else line
  if line.isEmpty || line.head? == some '#' then none else
  let line := trimTrailing line
  let (neg, line) := match line with
    | '!' :: r => (true, r)
    | r => (false, r)
  let (dirOnly, line) :=
    if line.getLast? == some '/' then (true, line.dropLast) else (false, line)
  let basename := !line.contains '/'
  let body := if line.head? == some '/' then line.tail else line
  some { neg, dirOnly, basename, body }

def lines (s : String) : List (List Char) := (s.splitOn "\n").map String.toList

def parseFile (s : String) : List Pat := (lines s).filterMap parseLine

def baseName (path : List Char) : List Char :=
  match (path.reverse.takeWhile (· != '/')).reverse with
  | n => n

def special (c : Char) : Bool := c == '*' || c == '?' || c == '[' || c == '\\'

/-- git's `match_pathname`: the literal prefix before the first glob
character is compared as text and the rest is matched as a pattern of its
own, so a `**` right after that prefix counts as being at the start
(`a**/b` matches `ab` and `ax/y/b`). -/
def anchoredMatch (body rel : List Char) : Bool :=
  let pre := body.takeWhile (!special ·)
  pre.isPrefixOf rel && wildmatch (body.drop pre.length) (rel.drop pre.length)

/-- Does `p` match `rel` (relative to the `.gitignore` directory)? -/
def Pat.hits (p : Pat) (rel : List Char) (isDir : Bool) : Bool :=
  (!p.dirOnly || isDir) &&
    if p.basename then wildmatch p.body (baseName rel) else anchoredMatch p.body rel

/-- Last matching pattern of a file decides: `some true` excluded,
`some false` re-included, `none` no opinion. -/
def decide (ps : List Pat) (rel : List Char) (isDir : Bool) : Option Bool :=
  ps.foldl (fun acc p => if p.hits rel isDir then some (!p.neg) else acc) none

def strip (dir path : String) : Option String :=
  if dir.isEmpty then some path
  else if (dir ++ "/").isPrefixOf path then some (path.drop (dir.length + 1)) else none

/-- `ignores` maps a directory (root-relative, `""` for the root) to its
`.gitignore` text. A path is excluded by the deepest `.gitignore` above it
that has an opinion. -/
def excludedSelf (ignores : List (String × String)) (path : String) (isDir : Bool) : Bool :=
  let parent := match (path.splitOn "/").dropLast with
    | [] => ""
    | ds => "/".intercalate ds
  let applicable := ignores.filter fun (d, _) =>
    d.isEmpty || d == parent || (d ++ "/").isPrefixOf parent
  -- deepest first
  let sorted := applicable.toArray.qsort (fun a b => a.1.length > b.1.length) |>.toList
  let rec first : List (String × String) → Bool
    | [] => false
    | (d, text) :: rest =>
      match strip d path with
      | none => first rest
      | some rel =>
        match decide (parseFile text) rel.toList isDir with
        | some v => v
        | none => first rest
  first sorted

/-- Proper ancestor directories of a path, shallowest first. -/
def ancestors (path : String) : List String :=
  let parts := (path.splitOn "/").dropLast
  (List.range parts.length).map fun i => "/".intercalate (parts.take (i + 1))

def ignored (ignores : List (String × String)) (path : String) : Bool :=
  (ancestors path).any (fun d => excludedSelf ignores d true) || excludedSelf ignores path false

/-! ## Properties -/

/-- A single `*` never crosses `/`. -/
theorem star_no_slash : ∀ (s : List Char), mt [.star] ('/' :: s) = false := by
  intro s; simp [mt]

/-- `**/` matches the empty prefix: `**/foo` matches `foo` at the top. -/
theorem dstarSlash_empty : ∀ (p : List Tok) (s : List Char), mt p s = true →
    mt (.dstarSlash :: p) s = true := by
  intro p s h; simp [mt, h]

/-- Once an ancestor directory is excluded, no `!` pattern re-includes the
file: the result does not depend on the file's own patterns. -/
theorem parent_excluded (ig : List (String × String)) (path d : String)
    (hd : d ∈ ancestors path) (hx : excludedSelf ig d true = true) :
    ignored ig path = true := by
  unfold ignored
  simp only [Bool.or_eq_true, List.any_eq_true]
  exact Or.inl ⟨d, hd, hx⟩

example : wildmatch "*.py".toList "a/b.py".toList = false := by native_decide
example : wildmatch "**/b.py".toList "b.py".toList = true := by native_decide
example : wildmatch "a/**/b".toList "a/x/y/b".toList = true := by native_decide
example : wildmatch "a/**".toList "a/x/y".toList = true := by native_decide
example : wildmatch "x**y".toList "x/y".toList = false := by native_decide
example : wildmatch "*.{py,txt}".toList "a.py".toList = false := by native_decide
example : wildmatch "[".toList "[".toList = false := by native_decide
example : ignored [("", "a/\n!a/b.py\n")] "a/b.py" = true := by native_decide
example : ignored [("", "*.py\n"), ("a", "!b.py\n")] "a/b.py" = false := by native_decide
example : ignored [("", "foo \n")] "foo" = true := by native_decide
example : ignored [("", "foo\\ \n")] "foo " = true := by native_decide
example : ignored [("", "a**/b\n")] "ab" = true := by native_decide
example : ignored [("", "a**/b\n")] "ax/y/b" = true := by native_decide
example : ignored [("", "a/**b\n")] "a/x/b" = false := by native_decide

end Patterns.Gitignore
