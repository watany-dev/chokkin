/-
PEP 503 (https://peps.python.org/pep-0503/#normalized-names):

    re.sub(r"[-_.]+", "-", name).lower()

The domain is ASCII: PEP 508 names are `[A-Za-z0-9._-]`, and on ASCII
Python's `str.lower` maps exactly `A`–`Z`.
-/

namespace Spec.Pep503

def isSep (c : Char) : Bool := c == '-' || c == '_' || c == '.'

/-- `re.sub(r"[-_.]+", "-", ·)`: the text split into maximal runs of
    separators and of other characters, each separator run replaced by
    one `-`. -/
def collapse (l : List Char) : List Char :=
  (l.splitBy (fun a b => isSep a == isSep b)).flatMap fun run =>
    if run.all isSep then ['-'] else run

def normalize (name : String) : String :=
  String.ofList ((collapse name.toList).map Char.toLower)

def isAscii (s : String) : Bool := s.toList.all fun c => c.toNat < 128

example : normalize "Foo__Bar.-baz" = "foo-bar-baz" := by decide
example : normalize "-a-" = "-a-" := by decide

end Spec.Pep503
