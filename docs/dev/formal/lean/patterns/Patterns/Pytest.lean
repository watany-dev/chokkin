import Patterns.Fnmatch
/-
pytest `python_files` (pytest reference, "python_files", and
`_pytest.pathlib.fnmatch_ex`): a pattern without `/` is matched with
`fnmatch` against the file's basename; a pattern with `/` against the
whole absolute path, with `*/` put in front of it unless it is absolute.
-/
namespace Patterns.Pytest

def baseName (p : String) : String := (p.splitOn "/").getLast!

def matchesOne (pat path : String) : Bool :=
  if pat.contains '/' then
    let pat := if pat.startsWith "/" then pat else "*/" ++ pat
    Fnmatch.fnmatchcase pat ("/rootdir/" ++ path)
  else Fnmatch.fnmatchcase pat (baseName path)

def collects (pats : List String) (path : String) : Bool := pats.any (matchesOne · path)

/-- The `*` of a basename pattern never reaches a directory name. -/
example : collects ["check_*.py"] "app/check_x/helper.py" = false := by native_decide
example : collects ["check_*.py"] "app/check_x/check_y.py" = true := by native_decide

end Patterns.Pytest
