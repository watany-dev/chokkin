import Binding.Scope

/-!
Properties of the resolution rules in `Binding/Scope.lean`, proved for every
chain and every name (not a finite sample). The evaluator in `Spec.lean` only
reaches scopes through these functions, so the properties carry over to every
reference the oracle reports.
-/

namespace Binding

theorem moduleOf_kind : ∀ (c : Chain) r, moduleOf c = some r → r.2.kind = .module
  | [], _, h => by simp [moduleOf] at h
  | (i, s) :: rest, r, h => by
    unfold moduleOf at h
    split at h
    · cases h; assumption
    · exact moduleOf_kind rest r h

theorem enclosingFunc_kind : ∀ (c : Chain) n r, enclosingFunc c n = some r → r.2.kind = .func
  | [], _, _, h => by simp [enclosingFunc] at h
  | (i, s) :: rest, n, r, h => by
    unfold enclosingFunc at h
    split at h
    · cases h
    · split at h
      · cases h; rename_i hk; exact hk.1
      · exact enclosingFunc_kind rest n r h

/-- §4.2.2: a free variable never resolves to a class scope. Names bound in a
class block are invisible to the functions, lambdas and comprehensions nested
in it. -/
theorem resolveFree_not_cls : ∀ (c : Chain) n r, resolveFree c n = some r → r.2.kind ≠ .cls
  | [], _, _, h => by simp [resolveFree] at h
  | (i, s) :: rest, n, r, h => by
    unfold resolveFree at h
    split at h
    · cases h; rename_i hk; simp [hk]
    · exact resolveFree_not_cls rest n r h
    · rename_i hm hc
      split at h
      · rw [moduleOf_kind rest r h]; simp
      · split at h
        · cases h; exact hc
        · exact resolveFree_not_cls rest n r h

/-- A load executed in a block that is not a class body reads no class scope:
the only class namespace a load can see is the one of the class body running
it. -/
theorem resolveLoad_not_cls (i : Nat) (s : ScopeInfo) (rest : Chain) (n : String)
    (hs : s.kind ≠ .cls) : ∀ r ∈ resolveLoad ((i, s) :: rest) n, r.2.kind ≠ .cls := by
  intro r hr
  have hmod : ∀ m, moduleOf ((i, s) :: rest) = some m → m.2.kind ≠ .cls := by
    intro m h; rw [moduleOf_kind _ _ h]; simp
  simp only [resolveLoad] at hr
  by_cases h1 : s.kind = .module
  · simp [h1] at hr; subst hr; exact hs
  · simp only [h1, if_false] at hr
    by_cases h2 : n ∈ s.globals
    · simp only [h2, if_true] at hr
      cases h : moduleOf ((i, s) :: rest) with
      | none => simp [h] at hr
      | some m => simp [h] at hr; rw [hr]; exact hmod m h
    · simp only [h2, if_false] at hr
      by_cases h3 : n ∈ s.nonlocals
      · simp only [h3, if_true] at hr
        cases h : enclosingFunc rest n with
        | none => simp [h] at hr
        | some m =>
          simp [h] at hr; subst hr; rw [enclosingFunc_kind _ _ _ h]; simp
      · simp only [h3, if_false] at hr
        by_cases h4 : n ∈ s.locals
        · simp [h4, hs] at hr; subst hr; exact hs
        · simp only [h4, if_false] at hr
          cases h : resolveFree rest n with
          | none => simp [h] at hr
          | some m =>
            simp [h] at hr; subst hr; exact resolveFree_not_cls _ _ _ h

theorem resolveStore_kind (c : Chain) (n : String) (r : Nat × ScopeInfo)
    (h : resolveStore c n = some r) :
    r.2.kind = .module ∨ r.2.kind = .func ∨ some r = c.head? := by
  match c with
  | [] => simp [resolveStore] at h
  | (i, s) :: rest =>
    simp only [resolveStore] at h
    by_cases h1 : n ∈ s.globals
    · simp only [h1, if_true] at h; exact Or.inl (moduleOf_kind _ _ h)
    · simp only [h1, if_false] at h
      by_cases h2 : n ∈ s.nonlocals
      · simp only [h2, if_true] at h; exact Or.inr (Or.inl (enclosingFunc_kind _ _ _ h))
      · simp only [h2, if_false] at h; cases h; simp

/-- PEP 572: an assignment expression never binds in a comprehension scope,
however deeply comprehensions nest. -/
theorem walrusTarget_not_comp : ∀ (c : Chain) n r, walrusTarget c n = some r → r.2.kind ≠ .comp
  | [], _, _, h => by simp [walrusTarget] at h
  | (i, s) :: rest, n, r, h => by
    unfold walrusTarget at h
    split at h
    · exact walrusTarget_not_comp rest n r h
    · rename_i hc
      rcases resolveStore_kind _ _ _ h with hk | hk | hk
      · rw [hk]; simp
      · rw [hk]; simp
      · simp at hk; subst hk; exact hc

/-- §7.11: with a literal `__all__`, `from m import *` binds exactly those names. -/
theorem starNames_all (l ns : List String) : starNames (some l) ns = l := rfl

/-- §7.11: without `__all__`, `from m import *` binds no name starting with `_`. -/
theorem starNames_public (ns : List String) :
    ∀ n ∈ starNames none ns, n.startsWith "_" = false := by
  intro n h
  simp [starNames, List.mem_filter] at h
  simpa using h.2

/-- `from . import x` in a package `__init__` resolves against the package itself. -/
theorem relBase_pkg_one (parts : List String) (h : parts ≠ []) :
    relBase parts true 1 = some parts := by
  have : 0 < parts.length := List.length_pos.mpr h
  simp [relBase, this]

/-- `from . import x` in a plain module resolves against the containing package. -/
theorem relBase_mod_one (parts : List String) (h : 1 < parts.length) :
    relBase parts false 1 = some parts.dropLast := by
  have h0 : 0 < parts.length - 1 := by omega
  have h1 : parts.dropLast.length = parts.length - 1 := List.length_dropLast parts
  simp only [relBase, if_false, Bool.false_eq_true, h1, Nat.sub_self, Nat.sub_zero, h0, if_true]
  rw [← h1, List.take_length]

/-- A relative import never leaves the module's own package path: the base is
always a prefix of the module's dotted name. -/
theorem relBase_prefix (parts : List String) (b : Bool) (k : Nat) (r : List String)
    (h : relBase parts b k = some r) : r <+: parts := by
  simp only [relBase] at h
  cases b <;> simp only [if_false, if_true, Bool.false_eq_true] at h
  · by_cases hk : k - 1 < parts.dropLast.length
    · simp only [hk, if_true, Option.some.injEq] at h
      subst h
      exact (List.take_prefix _ _).trans (List.dropLast_prefix _)
    · simp only [hk, if_false] at h; cases h
  · by_cases hk : k - 1 < parts.length
    · simp only [hk, if_true, Option.some.injEq] at h
      subst h
      exact List.take_prefix _ _
    · simp only [hk, if_false] at h; cases h

end Binding
