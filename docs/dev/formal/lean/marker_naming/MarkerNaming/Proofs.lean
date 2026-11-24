import MarkerNaming.Naming
import MarkerNaming.Pep440
import MarkerNaming.Marker
import MarkerNaming.Dist
import MarkerNaming.Setuptools

/-!
Properties of the specs, proved before they are used as an oracle. A
disagreement with chokkin is filed only when the spec side is backed by
these (and by the standard's text cited in each module).
-/
namespace MarkerNaming.Proofs

/-! ## PEP 503 normalization -/
section Naming
open Naming

/-- `lower` changes only `A`–`Z`. -/
theorem lower_cases (c : Char) : lower c = c ∨ ('a' ≤ lower c ∧ lower c ≤ 'z' ∧ 'A' ≤ c ∧ c ≤ 'Z') := by
  unfold lower; split <;> first | (right; decide) | (left; rfl)

theorem lower_fix (c : Char) (h : 'a' ≤ c ∧ c ≤ 'z') : lower c = c := by
  rcases lower_cases c with h1 | ⟨h1, h2, h3, h4⟩
  · exact h1
  · exfalso
    simp only [Char.le_def] at h h3 h4
    have : (97 : UInt32) ≤ 90 := Nat.le_trans h.1 h4
    contradiction

theorem beq_false_of_val (d e : Char) (h : d.val ≠ e.val) : (d == e) = false := by
  simp only [beq_eq_false_iff_ne]; intro x; exact h (x ▸ rfl)

theorem isSep_lower (c : Char) : isSep (lower c) = isSep c := by
  rcases lower_cases c with h | ⟨h1, h2, h3, h4⟩
  · rw [h]
  · have hl : isSep (lower c) = false := by
      simp only [isSep, Char.le_def] at *
      revert h1 h2; generalize lower c = d; intro h1 h2
      simp only [Bool.or_eq_false_iff]
      refine ⟨⟨?_, ?_⟩, ?_⟩ <;> apply beq_false_of_val <;> intro hv <;> rw [hv] at h1 <;>
        revert h1 <;> decide
    have hc : isSep c = false := by
      simp only [isSep, Char.le_def] at *
      simp only [Bool.or_eq_false_iff]
      refine ⟨⟨?_, ?_⟩, ?_⟩ <;> apply beq_false_of_val <;> intro hv <;> rw [hv] at h3 h4 <;>
        revert h3 h4 <;> decide
    rw [hl, hc]

theorem lower_lower (c : Char) : lower (lower c) = lower c := by
  rcases lower_cases c with h | ⟨h1, h2, _, _⟩
  · rw [h, h]
  · exact lower_fix _ ⟨h1, h2⟩

theorem head_dropWhile (p : Char → Bool) :
    ∀ (l : List Char) (c : Char), (l.dropWhile p).head? = some c → p c = false
  | [], _, h => by simp at h
  | d :: ds, c, h => by
    simp only [List.dropWhile] at h
    split at h
    · exact head_dropWhile p ds c h
    · simp at h; subst h; simp_all

theorem head_norm :
    ∀ (l : List Char), (∀ c, l.head? = some c → isSep c = false) →
      ∀ c, (norm l).head? = some c → isSep c = false
  | [], _, c, h => by simp [norm] at h
  | d :: ds, hl, c, h => by
    have hd : isSep d = false := hl d rfl
    rw [norm] at h
    simp only [hd] at h
    simp at h; subst h; rw [isSep_lower]; exact hd

theorem dropWhile_of_head (p : Char → Bool) (l : List Char)
    (h : ∀ c, l.head? = some c → p c = false) : l.dropWhile p = l := by
  cases l with
  | nil => rfl
  | cons d ds => simp [List.dropWhile, h d rfl]

/-- PEP 503 normalization is idempotent: a normalized name is its own
normal form, so comparing normalized names is an equivalence. -/
theorem norm_idem : ∀ l : List Char, norm (norm l) = norm l
  | [] => by simp [norm]
  | c :: cs => by
    by_cases hc : isSep c = true
    · have hhead := head_norm (cs.dropWhile isSep) (head_dropWhile isSep cs)
      have hx := dropWhile_of_head isSep _ hhead
      have ih := norm_idem (cs.dropWhile isSep)
      rw [norm, if_pos hc, norm, if_pos (by decide), hx, ih]
    · have hc' : isSep c = false := by simpa using hc
      have ih := norm_idem cs
      rw [norm, if_neg hc, norm, isSep_lower, if_neg (by simp [hc']), lower_lower, ih]
termination_by l => l.length
decreasing_by
  all_goals simp_wf
  all_goals first | omega | (have := dropWhile_length_le isSep cs; omega)

theorem normalize_idem (s : String) : normalize (normalize s) = normalize s := by
  simp [normalize, norm_idem]

/-- Same normal form is an equivalence relation (reflexive, symmetric and
transitive by construction), and every name is equivalent to its form. -/
theorem normalize_equiv (s : String) : normalize (normalize s) = normalize s :=
  normalize_idem s

end Naming

/-! ## PEP 440 release ordering -/
section Pep440
open Pep440

theorem cmpRelease_refl : ∀ l : List Nat, cmpRelease l l = .eq
  | [] => by simp [cmpRelease]
  | a :: as => by simp [cmpRelease, cmpRelease_refl as]

/-- Zero padding: `1.0` and `1.0.0` compare equal (PEP 440 "Release
segment"). -/
theorem cmpRelease_pad : ∀ l : List Nat, cmpRelease (l ++ [0]) l = .eq
  | [] => by simp [cmpRelease]
  | a :: as => by simp [cmpRelease, cmpRelease_pad as]

theorem cmpRelease_swap : ∀ l m : List Nat, cmpRelease l m = (cmpRelease m l).swap
  | [], [] => by simp [cmpRelease]; rfl
  | [], b :: bs => by
    simp only [cmpRelease]; split
    · exact cmpRelease_swap [] bs
    · rfl
  | a :: as, [] => by
    simp only [cmpRelease]; split
    · exact cmpRelease_swap as []
    · rfl
  | a :: as, b :: bs => by
    simp only [cmpRelease]
    by_cases h1 : a < b
    · have : ¬ b < a := by omega
      simp [h1, this]; rfl
    · by_cases h2 : b < a
      · simp [h1, h2]; rfl
      · simp [h1, h2]; exact cmpRelease_swap as bs

/-- `!=V` is the complement of `==V` (PEP 440 "Version exclusion"). -/
theorem ne_complement (s : Specifier) (v : Version) (c : String)
    (hne : s.op = .ne) :
    contains s v c = !contains { s with op := .eq } v c := by
  unfold contains; rw [hne]; cases s.wildcard <;> simp <;> rfl

/-- `~=V.N` is `>=V.N, ==V.*` (PEP 440 "Compatible release"). -/
theorem compat_def (s : Specifier) (v : Version) (c : String) (h : s.op = .compat) :
    contains s v c =
      (contains { s with op := .ge } v c &&
        prefixMatch v s.version.release.dropLast) := by
  simp [contains, h]

/-- `requires-python` items only narrow the admitted interpreters. -/
theorem admits_cons (s : Specifier) (rp : List Specifier) (r : List Nat) :
    Marker.admits (s :: rp) r = true → Marker.admits rp r = true := by
  simp [Marker.admits, List.all_cons]

end Pep440

/-! ## setuptools `extras_require` keys -/
section Setuptools
open Setuptools

/-- A key without `:` is an extra with no marker. -/
theorem splitKey_plain (k : String) (h : k.splitOn ":" = [k]) :
    splitKey k = { extra := some k.trim, marker := none } := by
  simp [splitKey, h]

end Setuptools

/-! ## Evaluated examples from the standards' own text

These are checked by evaluation (`native_decide`), not proved: they pin
the spec to the examples the PEPs give. -/

-- PEP 503: "Friendly-Bard", "FRIENDLY-BARD", "friendly.bard", "friendly_bard",
-- "friendly--bard", "FrIeNdLy-._.-bArD" all normalize to "friendly-bard".
example : ["Friendly-Bard", "FRIENDLY-BARD", "friendly.bard", "friendly_bard",
    "friendly--bard", "FrIeNdLy-._.-bArD"].all (Naming.normalize · == "friendly-bard") := by
  native_decide

-- PEP 440: `~= 2.2` matches 2.2 and 2.3 but not 3.0; `== 3.1.*` matches 3.1.0rc1.
example : (match Pep440.parseSpecifier "~=2.2" with
    | .ok s => [("2.2", true), ("2.3", true), ("3.0", false), ("2.1", false)].all fun (v, e) =>
        match Pep440.parseVersion v with
        | some w => Pep440.contains s w v == e
        | none => false
    | _ => false) = true := by native_decide

example : (match Pep440.parseSpecifier "==3.1.*", Pep440.parseVersion "3.1.0rc1" with
    | .ok s, some v => Pep440.contains s v "3.1.0rc1"
    | _, _ => false) = true := by native_decide

-- PEP 440: `<3.1` excludes 3.1.0rc1 (no pre-release of the named version).
example : (match Pep440.parseSpecifier "<3.1", Pep440.parseVersion "3.1.0rc1" with
    | .ok s, some v => !Pep440.contains s v "3.1.0rc1"
    | _, _ => false) = true := by native_decide

-- PEP 508: `python_version` is `X.Y`, so `python_version > "3.9"` fails on
-- 3.9.5 even though `requires-python = ">3.9"` admits it.
example : (match Pep440.parseSpecifierSet ">3.9", Marker.parse "python_version > \"3.9\"" with
    | some rp, some m => Marker.verdict rp m == .notAlways
    | _, _ => false) = true := by native_decide

example : (match Pep440.parseSpecifierSet ">=3.9", Marker.parse "python_version >= '3.8'" with
    | some rp, some m => Marker.verdict rp m == .always
    | _, _ => false) = true := by native_decide

-- Core Metadata: the description after the first empty line is not a field.
example : Dist.field (Dist.headers "Name: a\nVersion: 1\n\nImport-Name: b\n") "import-name" = [] := by
  native_decide

-- PEP 794: `; private` is not part of the name.
example : Dist.importNameValue "foo._impl ; private" = "foo._impl" := by native_decide

-- setuptools: `":python_version<'3.8'"` holds install requirements.
example : Setuptools.splitKey ":python_version<'3.8'" =
    { extra := none, marker := some "python_version<'3.8'" } := by native_decide

end MarkerNaming.Proofs
