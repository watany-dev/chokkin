import Lockfile.Spec

/-!
# Properties of the closure specification

* `iterate_sound`: everything the executable closure returns is reachable.
* `seeds_subset_iterate`: the seeds are always in it.
* `complete_of_closed`: if the result is closed under `succ` (checked at run
  time by `isClosed`; the oracle refuses to answer otherwise), it holds every
  reachable node. Together: closed result = exactly `Reach`.
* `reach_mono_seeds` / `reach_mono_lock`: more declarations or more lock
  entries never shrink the closure, so adding lock information can only
  strengthen CHK004 evidence.
* `reach_through_edge_extra`: an edge to `q[x]` reaches what `q[x]` adds.
-/

namespace Lockfile

theorem mem_stepAll_of_mem {L : Lock} {vs : List Node} {n : Node} (h : n ∈ vs) :
    n ∈ stepAll L vs := by
  unfold stepAll; exact List.mem_append.mpr (Or.inl h)

theorem mem_stepAll {L : Lock} {vs : List Node} {m : Node} (h : m ∈ stepAll L vs) :
    m ∈ vs ∨ ∃ n, n ∈ vs ∧ m ∈ succ L n := by
  unfold stepAll at h
  rcases List.mem_append.mp h with h | h
  · exact Or.inl h
  · right
    have h' := (List.mem_filter.mp h).1
    exact List.mem_flatMap.mp h'

theorem iterate_sound (L : Lock) (seeds : List Node) :
    ∀ (k : Nat) (vs : List Node), (∀ n ∈ vs, Reach L seeds n) →
      ∀ n ∈ iterate L k vs, Reach L seeds n := by
  intro k
  induction k with
  | zero => intro vs hvs n hn; exact hvs n hn
  | succ k ih =>
    intro vs hvs n hn
    simp only [iterate] at hn
    split at hn
    · exact hvs n hn
    apply ih (stepAll L vs) _ n hn
    intro m hm
    rcases mem_stepAll hm with h | ⟨p, hp, hsucc⟩
    · exact hvs m h
    · exact Reach.step (hvs p hp) hsucc

theorem closure_sound (L : Lock) (seeds : List Node) (n : Node)
    (h : n ∈ closure L seeds) : Reach L seeds n :=
  iterate_sound L seeds _ seeds (fun _ hs => Reach.seed hs) n h

theorem subset_iterate (L : Lock) :
    ∀ (k : Nat) (vs : List Node), ∀ n ∈ vs, n ∈ iterate L k vs := by
  intro k
  induction k with
  | zero => intro vs n hn; exact hn
  | succ k ih =>
    intro vs n hn
    simp only [iterate]
    split
    · exact hn
    · exact ih _ n (mem_stepAll_of_mem hn)

theorem seeds_subset_closure (L : Lock) (seeds : List Node) :
    ∀ n ∈ seeds, n ∈ closure L seeds :=
  subset_iterate L _ seeds

theorem complete_of_closed (L : Lock) (seeds vs : List Node)
    (hseeds : ∀ n ∈ seeds, n ∈ vs) (hclosed : isClosed L vs = true) :
    ∀ n, Reach L seeds n → n ∈ vs := by
  intro n hr
  induction hr with
  | seed hs => exact hseeds _ hs
  | step _ hm ih =>
    have hall := (List.all_eq_true.mp hclosed) _ ih
    have := (List.all_eq_true.mp hall) _ hm
    exact List.elem_iff.mp this

/-- A closed executable closure is exactly the specification. -/
theorem closure_exact (L : Lock) (seeds : List Node)
    (hclosed : isClosed L (closure L seeds) = true) (n : Node) :
    n ∈ closure L seeds ↔ Reach L seeds n :=
  ⟨closure_sound L seeds n,
   complete_of_closed L seeds _ (seeds_subset_closure L seeds) hclosed n⟩

theorem reach_mono_seeds {L : Lock} {s t : List Node} (hst : ∀ n ∈ s, n ∈ t) :
    ∀ n, Reach L s n → Reach L t n := by
  intro n h
  induction h with
  | seed hs => exact Reach.seed (hst _ hs)
  | step _ hm ih => exact Reach.step ih hm

theorem depsOf_mono {L L' : List Pkg} (hL : ∀ p ∈ L, p ∈ L') (n : Node) :
    ∀ d ∈ depsOf L n, d ∈ depsOf L' n := by
  intro d hd
  match n with
  | (p, none) =>
    simp only [depsOf] at hd ⊢
    obtain ⟨k, hk, hdk⟩ := List.mem_flatMap.mp hd
    have ⟨hkL, hkn⟩ := List.mem_filter.mp hk
    exact List.mem_flatMap.mpr ⟨k, List.mem_filter.mpr ⟨hL k hkL, hkn⟩, hdk⟩
  | (p, some e) =>
    simp only [depsOf] at hd ⊢
    obtain ⟨k, hk, hdk⟩ := List.mem_flatMap.mp hd
    have ⟨hkL, hkn⟩ := List.mem_filter.mp hk
    exact List.mem_flatMap.mpr ⟨k, List.mem_filter.mpr ⟨hL k hkL, hkn⟩, hdk⟩

theorem succ_mono {L L' : Lock} (hL : ∀ p ∈ L, p ∈ L') (n m : Node)
    (h : m ∈ succ L n) : m ∈ succ L' n := by
  unfold succ at h ⊢
  obtain ⟨d, hd, hm⟩ := List.mem_flatMap.mp h
  exact List.mem_flatMap.mpr ⟨d, depsOf_mono hL n d hd, hm⟩

theorem reach_mono_lock {L L' : Lock} {s : List Node} (hL : ∀ p ∈ L, p ∈ L') :
    ∀ n, Reach L s n → Reach L' s n := by
  intro n h
  induction h with
  | seed hs => exact Reach.seed hs
  | step _ hm ih => exact Reach.step ih (succ_mono hL _ _ hm)

/-- Declaring `q[x]` turns on `q` and its extra `x`. -/
theorem depNodes_extra (d : Dep) (e : String) (h : e ∈ d.extras) :
    (d.name, some e) ∈ depNodes d := by
  unfold depNodes
  exact List.mem_cons_of_mem _ (List.mem_map.mpr ⟨e, h, rfl⟩)

/-- A dependency on `q[x]` reaches what `q[x]` depends on: the edge extras
are part of the closure, not just of the declaration (uv's `extra = [...]`). -/
theorem reach_through_edge_extra {L : Lock} {s : List Node} {n : Node} {d : Dep}
    {e : String} {r : Dep}
    (hn : Reach L s n) (hd : d ∈ depsOf L n) (he : e ∈ d.extras)
    (hr : r ∈ depsOf L (d.name, some e)) :
    Reach L s (r.name, none) := by
  have h1 : Reach L s (d.name, some e) :=
    Reach.step hn (List.mem_flatMap.mpr ⟨d, hd, depNodes_extra d e he⟩)
  exact Reach.step h1 (List.mem_flatMap.mpr ⟨r, hr, List.mem_cons_self _ _⟩)

end Lockfile
