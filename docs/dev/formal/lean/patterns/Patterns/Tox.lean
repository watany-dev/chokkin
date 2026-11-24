/-
tox 4 factor conditions (tox documentation, "Generative environment list"
and "Factors and factor-conditional settings").

A line `COND: content` is factor-conditional when the first `:` that is
followed by whitespace or the end of the line closes a valid condition.
The condition is a comma-separated list of alternatives; each alternative
is a `-`-joined list of factors that must all hold; `!` negates a factor;
`{a,b}` expands into one alternative per member (a product when several
groups appear); spaces around members are ignored. A factor is a run of
word characters, `.`, and fnmatch glob characters. When the condition is
not valid the whole line is content.

Numeric ranges (`{38-40}`) are outside this model; the generator avoids
them.
-/
namespace Patterns.Tox

def isSpace (c : Char) : Bool := c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\x0b' || c == '\x0c'

def trim (s : List Char) : List Char :=
  ((s.dropWhile isSpace).reverse.dropWhile isSpace).reverse

def isWord (c : Char) : Bool := c.isAlphanum || c == '_' || c.toNat > 127

/-- tox's `_FACTOR_RE`: empty, or `!?` then one or more of word chars, `.`,
`*`, `?`, `]`, `[` / `[!`. -/
def validFactor (f : List Char) : Bool :=
  let body := match f with
    | '!' :: r => r
    | r => r
  let rec ok : List Char → Bool
    | [] => true
    | '[' :: '!' :: r => ok r
    | c :: r => (isWord c || c == '.' || c == '*' || c == '?' || c == '[' || c == ']') && ok r
  f.isEmpty || (!body.isEmpty && ok body)

/-- Split at top-level commas; text inside `{...}` (no nesting) stays. -/
def splitAlternatives : List Char → List (List Char)
  | cs =>
    let rec go : List Char → Bool → List Char → List (List Char)
      | [], _, cur => [cur.reverse]
      | '{' :: r, false, cur =>
        -- a brace group only counts when it is closed with something inside
        match r.dropWhile (· != '}') with
        | '}' :: _ =>
          if (r.takeWhile (· != '}')).isEmpty then go r false ('{' :: cur) else go r true ('{' :: cur)
        | _ => go r false ('{' :: cur)
      | '}' :: r, true, cur => go r false ('}' :: cur)
      | ',' :: r, false, cur => cur.reverse :: go r false []
      | c :: r, b, cur => go r b (c :: cur)
    go cs false []

/-- Elements of one alternative, as tox splits it at `{...}` groups: each
literal run and each brace member is stripped of surrounding spaces. -/
def elementsF : Nat → List Char → List Char → List (List (List Char))
  | 0, _, cur => [[trim cur.reverse]]
  | _, [], cur => [[trim cur.reverse]]
  | n + 1, '{' :: r, cur =>
    match r.dropWhile (· != '}') with
    | '}' :: after =>
      let inner := r.takeWhile (· != '}')
      if inner.isEmpty then elementsF n r ('{' :: cur)
      else
        let members := (String.mk inner).splitOn "," |>.map (fun m => trim m.toList)
        [trim cur.reverse] :: members :: elementsF n after []
    | _ => elementsF n r ('{' :: cur)
  | n + 1, c :: r, cur => elementsF n r (c :: cur)

def product : List (List (List Char)) → List (List Char)
  | [] => [[]]
  | opts :: rest => opts.flatMap fun o => (product rest).map (o ++ ·)

def expandBraces (cs : List Char) : List (List Char) :=
  product (elementsF (cs.length + 1) cs [])

def splitDash (cs : List Char) : List (List Char) :=
  ((String.mk cs).splitOn "-").map String.toList

/-- Factor groups of a condition, `none` when it is not valid. -/
def groups (cond : List Char) : Option (List (List (String × Bool))) :=
  let alts := (splitAlternatives cond).filter (!·.isEmpty) |>.map trim
  let variants := alts.flatMap fun a => (expandBraces a).map fun v => v
  if variants.all fun v => (splitDash v).all validFactor then
    some (variants.map fun v => (splitDash v).map fun f =>
      match f with
      | '!' :: r => (String.mk r, true)
      | r => (String.mk r, false))
  else none

/-- Index of the first `:` followed by whitespace or the end. -/
def marker : List Char → Nat → Option Nat
  | [], _ => none
  | ':' :: [], i => some i
  | ':' :: c :: r, i => if isSpace c then some i else marker (c :: r) (i + 1)
  | _ :: r, i => marker r (i + 1)

def expandLine (line : String) : Option (List (List (String × Bool))) × String :=
  let cs := line.toList
  match marker cs 0 with
  | none => (none, line)
  | some i =>
    match groups (trim (cs.take i)) with
    | none => (none, line)
    | some gs => (some gs, String.mk (trim (cs.drop (i + 1))))

example : (expandLine "py{38,39}: mock").2 = "mock" := by native_decide
example : (expandLine "py38 , py39: mock").2 = "mock" := by native_decide
example : (expandLine "py38,py39:mock").1 = none := by native_decide
example : (expandLine "https://x").1 = none := by native_decide
example : (expandLine "py3!8: a").1 = none := by native_decide

end Patterns.Tox
