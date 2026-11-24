import Lexical.Lines
import Lexical.Tokens
import Lexical.Directive
import Lexical.Coding

/-!
# Properties of the specs, proved for every input

These are what make the specs usable as an oracle: a disagreement with
chokkin on an input is a disagreement about one of these, not an accident of
the spec.

1. `terminators_univ`: line numbering does not depend on which of LF, CR LF
   and CR ends a line (§2.1.2 "all of these forms can be used equally").
2. `lex_comments_wf`: every COMMENT token starts with `#` and holds no line
   terminator (§2.1.3 "ends at the end of the physical line").
3. `effects_inline_from_comment`: an inline ignore that counts at line `l` comes
   from a directive that starts at a `#` inside a COMMENT token on line `l`.
   With 2, a directive can neither come from a string literal nor reach across
   a line end.
4. `declared_first_two_lines`: the PEP 263 declaration is a function of the
   first two physical lines only.
-/

namespace Lexical

/-! ## 1. Line terminators -/

theorem terminators_cons_ne (c : Char) (rest : List Char) (hc : c ≠ '\r') :
    terminators (c :: rest) = terminators rest + (if isNL c then 1 else 0) := by
  cases rest <;> simp [terminators, hc]

theorem terminators_cr_ne (rest : List Char) (h : ∀ r, rest ≠ '\n' :: r) :
    terminators ('\r' :: rest) = terminators rest + 1 := by
  cases rest with
  | nil => simp [terminators, isNL]
  | cons d tl =>
    have hd : d ≠ '\n' := fun e => h tl (by simp [e])
    simp [terminators, hd, isNL]

theorem terminators_univ (s : List Char) : terminators (univ s) = terminators s := by
  induction s using univ.induct with
  | case1 => rfl
  | case2 rest ih => simp [univ, terminators, ih, isNL]
  | case3 rest h ih =>
    have h' : ∀ r, rest ≠ '\n' :: r := fun r e => h r e
    rw [terminators_cr_ne rest h']
    have : univ ('\r' :: rest) = '\n' :: univ rest := by
      cases rest with
      | nil => rfl
      | cons d tl =>
        have hd : d ≠ '\n' := fun e => h tl (by simp [e])
        simp [univ, hd]
    rw [this, terminators_cons_ne _ _ (by decide), ih]
    simp [isNL]
  | case4 c rest h1 h2 ih =>
    have : univ (c :: rest) = c :: univ rest := by
      cases rest <;> simp [univ, h2]
    rw [this, terminators_cons_ne _ _ h2, terminators_cons_ne _ _ h2, ih]

theorem univ_no_cr (s : List Char) : '\r' ∉ univ s := by
  induction s using univ.induct with
  | case1 => simp [univ]
  | case2 rest ih => simpa [univ] using ih
  | case3 rest h ih =>
    cases rest with
    | nil => simp [univ]
    | cons d tl =>
      have hd : d ≠ '\n' := fun e => h tl (by simp [e])
      simpa [univ, hd] using ih
  | case4 c rest h1 h2 ih =>
    have : univ (c :: rest) = c :: univ rest := by
      cases rest <;> simp [univ, h2]
    rw [this]; simp only [List.mem_cons, not_or]; exact ⟨fun e => h2 e.symm, ih⟩

/-! ## 2. COMMENT tokens -/

/-- What every COMMENT token satisfies. -/
def CommentWF (c : Comment) : Prop := c.text.head? = some '#' ∧ ∀ ch ∈ c.text, isNL ch = false

def StWF (st : St) : Prop := ∀ c ∈ st.comments, CommentWF c

theorem stWF_of_comments {st st' : St} (h : st'.comments = st.comments) (hw : StWF st) : StWF st' := by
  intro c hc; rw [h] at hc; exact hw c hc

theorem adv_comments (st : St) (n : Nat) (cs : List Char) : (adv st n cs).1.comments = st.comments := rfl

theorem stepBackslash_comments (st : St) (raw fstr : Bool) (cs : List Char) :
    (stepBackslash st raw fstr cs).1.comments = st.comments := rfl

theorem stepStr_comments (st : St) (q : Char) (t r f : Bool) (cs : List Char) :
    (stepStr st q t r f cs).1.comments = st.comments := rfl

theorem stepSpec_comments (st : St) (cs : List Char) :
    (stepSpec st cs).1.comments = st.comments := rfl

theorem openStr_comments (st : St) (raw fstr : Bool) (q : Char) (cs : List Char) :
    (openStr st raw fstr q cs).1.comments = st.comments := by
  unfold openStr
  repeat' (first | split | simp [push] | rfl)

theorem mem_takeWhile {p : Char → Bool} : ∀ {l : List Char} {x : Char},
    x ∈ l.takeWhile p → p x = true
  | [], _, h => by simp at h
  | a :: l, x, h => by
    unfold List.takeWhile at h
    split at h
    · simp only [List.mem_cons] at h
      rcases h with e | e
      · subst e; assumption
      · exact mem_takeWhile e
    · simp at h

theorem markCode_comments (st : St) : (markCode st).comments = st.comments := by
  unfold markCode; split <;> rfl

theorem stepCodeRest_comments (st : St) (d : Option Nat) (c : Char) (rest cs : List Char) :
    (stepCodeRest st d c rest cs).1.comments = st.comments := rfl

theorem stepCode_wf (st : St) (d : Option Nat) (cs : List Char) (hw : StWF st) :
    StWF (stepCode st d cs).1 := by
  unfold stepCode
  split
  · exact hw
  · rename_i c rest
    split
    · intro cm hm
      simp only [comment, List.mem_cons] at hm
      rcases hm with h | h
      · subst h
        refine ⟨rfl, ?_⟩
        intro ch hch
        simp only [List.mem_cons] at hch
        rcases hch with e | e
        · subst e; rfl
        · simpa using mem_takeWhile e
      · exact hw cm h
    · exact stWF_of_comments (stepCodeRest_comments st d c rest _) hw

theorem step_wf (st : St) (cs : List Char) (hw : StWF st) : StWF (step st cs).1 := by
  unfold step
  split
  · exact stepCode_wf st none cs hw
  · exact stepCode_wf st _ cs hw
  · exact stWF_of_comments (stepSpec_comments st cs) hw
  · exact stWF_of_comments (stepStr_comments st _ _ _ _ cs) hw

theorem run_wf : ∀ (fuel : Nat) (st : St) (cs : List Char), StWF st → StWF (run fuel st cs)
  | 0, st, _, hw => by simpa [run] using hw
  | fuel + 1, st, [], hw => by
    unfold run; split
    · exact hw
    · exact stWF_of_comments rfl hw
  | fuel + 1, st, c :: cs, hw => by
    unfold run
    split
    · exact hw
    · exact run_wf fuel _ _ (step_wf st (c :: cs) hw)

theorem lex_comments_wf (s : List Char) : ∀ c ∈ (lex s).comments, CommentWF c := by
  intro c hc
  simp only [lex, List.mem_reverse] at hc
  exact run_wf (s.length + 1) {} s (by intro c h; simp at h) c hc

/-! ## 3. Inline ignores come from COMMENT tokens -/

theorem mem_hashSuffixes {t sfx : List Char} (h : sfx ∈ hashSuffixes t) :
    sfx.head? = some '#' ∧ ∃ pre, pre ++ sfx = t := by
  induction t with
  | nil => simp [hashSuffixes] at h
  | cons c rest ih =>
    simp only [hashSuffixes, List.mem_append] at h
    rcases h with h | h
    · split at h
      · simp at h; subst h; rename_i hc; simp_all
      · simp at h
    · obtain ⟨hh, pre, hp⟩ := ih h
      exact ⟨hh, c :: pre, by simp [hp]⟩

theorem applyDirective_inline (fs : Option Nat) (l : Nat) (acc : Effects) (d : Directive) :
    ∀ e ∈ (applyDirective fs l acc d).inline, e ∈ acc.inline ∨ (e.1 = l ∧ d.fileLevel = false) := by
  intro e he
  unfold applyDirective at he
  simp only at he
  have base : ∀ x ∈ (if (d.codes.any fun c => !knownCode c) = true then
      { acc with warnings := acc.warnings ++ [(l, Warn.unknownCode)] } else acc).inline,
      x ∈ acc.inline := by
    intro x h; split at h <;> exact h
  generalize (if (d.codes.any fun c => !knownCode c) = true then
      { acc with warnings := acc.warnings ++ [(l, Warn.unknownCode)] } else acc) = b at he base
  repeat' split at he
  all_goals first
    | exact Or.inl (base e he)
    | (simp only [List.mem_append, List.mem_singleton] at he
       rcases he with h | h
       · exact Or.inl (base e h)
       · subst h; exact Or.inr ⟨rfl, by simp_all⟩)

theorem onSuffix_inline (fs : Option Nat) (l : Nat) (acc : Effects) (sfx : List Char) :
    ∀ e ∈ (onSuffix fs l acc sfx).inline, e ∈ acc.inline ∨
      (e.1 = l ∧ ∃ d, directiveAt sfx = some d ∧ d.fileLevel = false) := by
  intro e he
  unfold onSuffix at he
  split at he
  · left; split at he <;> exact he
  · rename_i d hd
    rcases applyDirective_inline fs l acc d e he with h | ⟨h1, h2⟩
    · exact Or.inl h
    · exact Or.inr ⟨h1, d, hd, h2⟩

/-- An inline entry at line `l` is witnessed by a comment on line `l` and a
directive parsed from a `#`-suffix of its text. -/
def Witnessed (comments : List Comment) (l : Nat) : Prop :=
  ∃ c ∈ comments, c.line = l ∧ ∃ sfx ∈ hashSuffixes c.text, ∃ d, directiveAt sfx = some d ∧ d.fileLevel = false

theorem onComment_inline (fs : Option Nat) (comments : List Comment) (cm : Comment)
    (hcm : cm ∈ comments) :
    ∀ (sfxs : List (List Char)) (acc : Effects), (∀ s ∈ sfxs, s ∈ hashSuffixes cm.text) →
      (∀ e ∈ acc.inline, Witnessed comments e.1) →
      ∀ e ∈ (sfxs.foldl (onSuffix fs cm.line) acc).inline, Witnessed comments e.1 := by
  intro sfxs
  induction sfxs with
  | nil => intro acc _ h; simpa using h
  | cons s rest ih =>
    intro acc hs hacc
    simp only [List.foldl_cons]
    apply ih
    · intro x hx; exact hs x (by simp [hx])
    · intro e he
      rcases onSuffix_inline fs cm.line acc s e he with h | ⟨h1, d, hd, hfl⟩
      · exact hacc e h
      · exact ⟨cm, hcm, h1.symm, s, hs s (by simp), d, hd, hfl⟩

theorem effects_inline_from_comment (comments : List Comment) (fs : Option Nat) :
    ∀ e ∈ (effects comments fs).inline, Witnessed comments e.1 := by
  unfold effects
  suffices h : ∀ (cms : List Comment) (acc : Effects), (∀ c ∈ cms, c ∈ comments) →
      (∀ e ∈ acc.inline, Witnessed comments e.1) →
      ∀ e ∈ (cms.foldl (onComment fs) acc).inline, Witnessed comments e.1 from
    h comments {} (fun _ h => h) (by simp)
  intro cms
  induction cms with
  | nil => intro acc _ h; simpa using h
  | cons cm rest ih =>
    intro acc hin hacc
    simp only [List.foldl_cons]
    apply ih _ (fun c h => hin c (by simp [h]))
    exact onComment_inline fs comments cm (hin cm (by simp)) _ acc (fun _ h => h) hacc

/-- The headline: an inline ignore that counts on line `l` was written in a
COMMENT token on line `l`, and the directive's text lies inside that token, so
it holds no line terminator. -/
theorem inline_ignore_is_comment (s : List Char) :
    ∀ e ∈ (effects (lex s).comments (lex s).firstCode).inline,
      ∃ c ∈ (lex s).comments, c.line = e.1 ∧
        ∃ sfx ∈ hashSuffixes c.text, (∀ ch ∈ sfx, isNL ch = false) ∧
          ∃ d, directiveAt sfx = some d := by
  intro e he
  obtain ⟨c, hc, hl, sfx, hs, d, hd, _⟩ := effects_inline_from_comment _ _ e he
  refine ⟨c, hc, hl, sfx, hs, ?_, d, hd⟩
  obtain ⟨_, pre, hpre⟩ := mem_hashSuffixes hs
  intro ch hch
  exact (lex_comments_wf s c hc).2 ch (by rw [← hpre]; simp [hch])

/-! ## 4. PEP 263 reads the first two lines only -/

theorem physLinesAux_line (l : List Char) (hl : ∀ ch ∈ l, isNL ch = false)
    (r cur : List Char) :
    physLinesAux (l ++ '\n' :: r) cur = (cur.reverse ++ l) :: physLinesAux r [] := by
  induction l generalizing cur with
  | nil => simp [physLinesAux, isNL]
  | cons c rest ih =>
    have hc : isNL c = false := hl c (by simp)
    have hr : ∀ ch ∈ rest, isNL ch = false := fun ch h => hl ch (by simp [h])
    have hcr : c ≠ '\r' := by intro e; subst e; simp [isNL] at hc
    simp only [List.cons_append]
    rw [show physLinesAux (c :: (rest ++ '\n' :: r)) cur
        = physLinesAux (rest ++ '\n' :: r) (c :: cur) by
      cases rest with
      | nil => simp [physLinesAux, hc, hcr]
      | cons d tl => simp [physLinesAux, hc, hcr]]
    rw [ih hr]
    simp

theorem declared_first_two_lines (l1 l2 r r' : List Char)
    (h1 : ∀ ch ∈ l1, isNL ch = false) (h2 : ∀ ch ∈ l2, isNL ch = false) :
    declared (l1 ++ '\n' :: (l2 ++ '\n' :: r)) = declared (l1 ++ '\n' :: (l2 ++ '\n' :: r')) := by
  have e : ∀ x, firstTwoLines (l1 ++ '\n' :: (l2 ++ '\n' :: x)) = [l1, l2] := by
    intro x
    simp only [firstTwoLines, physLines]
    rw [physLinesAux_line l1 h1, physLinesAux_line l2 h2]
    simp
  simp only [declared, e]

end Lexical
