/-
Root-relative paths, from POSIX.1-2024 XBD 3.271 (Pathname), 3.170
(Filename) and 4.16 (Pathname Resolution): a pathname is filenames
separated by `/`; "multiple successive <slash> characters are considered
to be the same as one <slash>"; the filename `.` "refers to the directory
in which it appears". Windows (chokkin ships wheels for it, AGENTS.md)
also separates on `\`.

The domain is relative paths without `..`: a `..` segment depends on the
file system (symlinks), and a leading separator makes the path absolute.
-/

namespace Spec.Path

def isSep (c : Char) : Bool := c == '/' || c == '\\'

/-- The filenames a relative pathname walks through, in order. -/
def components (p : String) : List String :=
  ((p.toList.splitBy (fun a b => !isSep a && !isSep b)).map String.ofList).filter
    fun s => !(s.toList.all isSep) && s != "."

/-- The spelling of a discovered file: components joined with `/`. -/
def canonical (p : String) : String := "/".intercalate (components p)

/-- Inside the domain: relative and free of `..`. -/
def inDomain (p : String) : Bool :=
  !(p.toList.head?.map isSep).getD false && !(components p).contains ".."

/-! Facts about the specification. -/

example : canonical "./src//acme/./main.py" = "src/acme/main.py" := by decide
example : canonical ".//src/app.py" = "src/app.py" := by decide
example : canonical "src\\app.py" = "src/app.py" := by decide
example : canonical "./" = "" := by decide

end Spec.Path
