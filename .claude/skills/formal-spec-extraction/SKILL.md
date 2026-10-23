---
name: formal-spec-extraction
description: Extract the de-facto spec from implementation code and hunt bugs with Z3 / TLA+ (or plain exhaustive enumeration). Use when asked to formalize existing behavior, find bugs tests cannot reach, check invariants over all inputs or all interleavings, or turn implicit behavior into an explicit spec.
---

# Extract the spec from implementation code and flush out bugs with Z3 / TLA+ — a practical playbook

Treat an existing system's implementation as its "de-facto spec" and formalize it
to surface both "bugs tests cannot reach" and "spec the implementation silently
decides". It assumes the usual situation: there is no spec, the spec is
unreliable, or the spec and the implementation have drifted apart.

Source: <https://gist.github.com/mizchi/db7817e6fc077d567c41cd9d41bb1c53>
(translated from Japanese).

In this repository, `AGENTS.md` still applies: never execute the analyzed
project's code, and models must not import or run it.

---

## 0. Stance: the code is the de-facto spec

Use **what the implementation actually does**, not the spec document, as the
source of truth. Three steps:

1. **Extract**: pull out, separately, "the spec the implementation claims" and
   "the behavior it implicitly decides".
2. **Refute**: model the claims and let a machine attack them to see whether
   they really hold for all inputs / all orderings.
3. **Reconcile**: take each counterexample to someone with domain knowledge and
   ask "is this intended?"
   → if intended, **write it down as spec**; if not, **it is a bug**.

A counterexample is a bug report and, at the same time, **the starting point of
a conversation that aligns understanding**. This is where the method pays off
most.

---

## 1. Choosing between Z3 and TLA+

| | Z3 (SMT) | TLA+ (TLC / model checking) |
|---|---|---|
| Question | **One instant, over all inputs** | **Over time, all orderings and interleavings** |
| Strength | Pure predicates / consistency of config and rules | Stateful protocols / concurrency / sequential state transitions |
| Analogy | A solver that cracks a huge puzzle in one go | A detective who walks every possible scenario |
| model↔code gap | Narrow (if the target is data, it can consume real data directly) | Wide (imperative code is abstracted by hand) |
| Where it lives | Save-time validator / static check in CI | Design-review artifact / regression guard |

**Decision flow:**

- "Does this condition hold **for any input**?" "Can this config **ever be
  served**?" → if the predicates fall in a decidable fragment of
  intervals + sets + equalities, use **Z3**.
- "Is it fine **in any order**?" "What if two arrive at once?" "After a crash?"
  "Does it **eventually** do X?" → state transitions + nondeterminism → **TLA+**.
- If the state space is finite and deterministic, **plain exhaustive
  enumeration (a program looping over the Cartesian product)** is cheapest.
  Before reaching for a tool, ask "is it finite? is it deterministic?"

---

## 2. Bug patterns formal methods flush out (general catalog)

When you extract from implementations, these shapes recur. **Use them as a
checklist.**

### Pure predicates / configuration (suits Z3)
- **Contradictory config (dead config)**: the conjunction of conditions is
  unsatisfiable, so no input ever passes. "Only A ∧ exclude A", "start > end",
  "x>20 ∧ x<18". Detected as UNSAT.
- **Redundancy / subsumption**: one condition implies another, so a configured
  value has no effect. "At most 10 times ∧ at most once" → the tighter one
  dominates. Compute the "effective value" and warn.
- **Coverage holes / dead branches**: some input matches no option, or some
  branch is chosen by no input.
- **Impact of a change**: when a config is edited, symbolically extract "whose
  decision changes" as inputs that flip.
- **Equivalence of representations**: prove old↔new format, or before↔after a
  refactor, mean the same thing for all inputs.

### Concurrency / time (suits TLA+)
- **Read-modify-write races / TOCTOU**: when read, decide, and write are
  separate, concurrent runs both read the stale value and both pass. Cap
  overruns, double processing. Show by contrast that "a conditional (atomic)
  write prevents it".
- **Decision separated from recording**: the decision happens now, recording
  happens later asynchronously. At decision time the record does not exist yet
  → the cap is bypassed.
- **Eventual consistency (staleness)**: the delay before a written value is
  visible to readers alone can break a cap, **even without concurrency**.
- **Atomicity / torn reads**: reading a shared resource (file, snapshot) in an
  intermediate state. Contrast "write to another file and rename (atomic)" with
  "overwrite in place".
- **Counter invariants**: does a counter stay non-negative under duplicate
  delivery and concurrency? Is the guard condition necessary and sufficient?
- **Convergence / runaway (termination)**: does a raise / retry / re-attempt
  loop stop in a bounded number of steps, or is an external condition (e.g. a
  deadline) the only brake (= that condition is load-bearing)?
- **Propagation / eventual consistency**: is a committed change always
  eventually applied? Is an update lost across crash + restart?
  (edge-triggered can lose it / level-triggered catches up)
- **Idempotency**: does a retry double-count? If IDs are "random per send",
  duplicates get recorded.

### Boundaries / trust (either tool)
- **Inconsistent fail-open / fail-close**: when information is unavailable, one
  mechanism blocks everything while another lets everything through.
  Enumerating "how does it behave on error" per mechanism exposes the
  inconsistency.
- **Errors folded into a boolean × negation**: an implementation that folds
  "no match" and "error" into the same `false` turns **errors into matches**
  once negation (NOT/inverse) is applied. The more broken the input, the more it
  passes. The most commonly missed one.
- **Meaning of the empty set**: empty AND = true (an always-match backdoor),
  empty OR = false, zero elements = true — inconsistent across layers.
- **Trust boundaries and spoofing**: a path where input the user can set freely
  (query params, headers) is adopted as a "trusted value" without validation. If
  unvalidated adoption sits at a high-priority position, it can be spoofed.
- **Cross-boundary representation contracts**: when two systems share the same
  bit string / enum / layout, mismatches like one being hourly and the other
  15-minute, or one MSB-first and the other LSB-first. Bijectivity of expansion
  and conversion.
- **Client-side-only validation**: only the UI/frontend rejects; the
  server/API lets it through → hitting the API directly creates invalid state.
- **Missing preconditions**: panic on negative sizes or empty arrays. The guard
  exists only at call sites.

---

## 3. Building a model from code — what to look at (extraction viewpoints)

When reading the implementation, **always note "declared spec" and "implicit
behavior" separately**.

### Where declared spec lives (places intent is written)
- **Comments**, especially ones that explain "why". `// everything is OK when
  ...`, `// check it doesn't overflow` are declarations of spec. Comments like
  "the original code did this" or "not sure why" are signs of a **fossil** —
  confirm whether it is spec or inertia.
- **Test names and assertions**: a test name like `NG if count equal the cap`
  is the boundary spec itself. The sequence of expected values is spec too.
- **Guard clauses, panics, error returns**: they declare preconditions and
  fail modes. Whether `if n < 0 { ... }` exists or not.
- **Enum definitions, constant tables, schema constraints**: the possible
  values and the value↔meaning mapping.

### Implicitly decided behavior (written nowhere, but fixed by the implementation)
- **Defaults and handling of empty/nil**: does "unset" mean "allow all" or
  "deny all"? The truth value of an empty collection.
- **Branches on error**: on missing / parse failure / timeout, does it fall to
  true, false, or an exception?
- **Order dependence and short-circuiting**: does the result change with
  application order (are there side effects)? Does short-circuiting affect the
  result or only the reason code?
- **Where a value is "decided"**: fall-through priority, which of several
  sources wins, where the trust decision is made.
- **Cross-boundary conversions**: serialization / bit layout / units /
  time zones / first day of week.
- **Who controls the input**: values set by infra vs values the user can set
  freely (trust boundary).

### Useful questions (throw them mechanically at every predicate and mechanism)
1. What happens when it is empty/unset?
2. On error/fetch failure, is it true, false, or an exception? (fail-open/close)
3. How does the answer above change once negation (NOT/exclude/inverse) is applied?
4. Which way does the boundary go? (`>` or `>=`, are endpoints included?)
5. What if two arrive at once? Does one's result arrive in time for the other?
6. Who decides this value? Can an untrusted party replace it?
7. What stops this loop/raise? Is the stopping condition essential or incidental?
8. Do the two systems agree on this data? (layout, enum, units)

---

## 4. What to write down as spec — how to write the model

### Carve out the decision function as pure
Strip the plumbing from the implementation and write a **pure function
`predict(state)`** from observable input → result. This becomes the
mathematical expression of "the spec itself". Do not bring plumbing (I/O,
frameworks, DB) into the model.

### Declare it positively, deterministically, in domain terms
Separate negative guards like "does not return 5xx when ..." from **behavioral
spec** like "doing this yields that". Write behavioral spec in positive,
deterministic, domain vocabulary. "Close the box" (hide the implementation's
internal vocabulary so it reads in domain words only) and it works as a spec.

### Declare invariants with names
`NeverOverCap == served <= cap`, `NoTornRead == every snapshot read is a single
version`. **Name the properties you want to prove.** Once named, a review can
talk about "does it satisfy this property?"

### Write counterexample-first: assert the expectation, let the tool refute it
Assert as an invariant "delivery never exceeds the initial target", and TLC
produces a counterexample that breaks it → the **implication of the
implementation**, "target does not bound the total", becomes visible. Proof
(holds) and counterexample (does not hold) are two sides of the same tool.
Set up each invariant knowing **whether you want to prove it holds or want a
counterexample**.

### Write equivalence classes and boundaries as enumerable sets
Even if all inputs are infinite, the **equivalence classes and boundaries**
where the decision changes are finite. Defining them as enumerated sets
generalizes hand-written tests into "generated from the model". Always include
the boundaries (cap-1, cap, cap+1).

### Track "contracts" and "counterexamples" separately
- **A counterexample appears** → bug or spec mismatch → issue / clarifying question
- **It is proven to hold** → **lock it as a contract (regression guard)** → it
  goes red if either side changes in the future

With both, you get a ledger of "what is guaranteed / where the holes are".

---

## 5. Keeping verification "load-bearing"

- **Make it self-checking**: give each model a run mode that asserts "the
  verdict matches the expected one (VIOLATE / OK)". Make it a regression guard
  that runs in CI, not a one-off demo.
- **Broken-variant tests**: prepare deliberately broken implementations (guard
  removed, inversion skipped) and confirm the check **catches them in red**.
  Skip this and you fall into "always green = verifies nothing". It proves the
  check is load-bearing.
- **Pin dependencies declaratively**: declare the solver, model checker, and
  runtime in one file (e.g. a nix flake / lockfile) so "anyone gets the same
  result without depending on the network or the local environment". Have
  local and CI hit the same harness (one execution path).
- **Emit both human-readable output and a machine-readable exit code.** It
  serves the demo and CI at once.

---

## 6. Closing the model ↔ code gap

Formal methods verify **the model you wrote**, not the code itself. Handling
the gap:

- **If the target is data (config, rules), the gap is narrow**: the model can
  read real data directly, so writing "a conversion layer that feeds the actual
  config into the validator" makes the proven object nearly identical to
  production.
- **If the target is imperative / concurrent code, the gap is wide**: you
  abstract by hand, so drift remains. The value is "showing the design is sound
  before / independently of the implementation".
- **Close the gap with trace-checking**: capture the real system's operation
  log (the actual event sequence) and replay it to check "is this observed
  sequence a legal behavior of the model?" You can **settle with real data**
  "which spec, naive or atomic, does the implementation refine?" This is the
  strongest move for connecting the abstract model to real behavior.

---

## 7. When to stop (ROI and anti-patterns)

Do not formalize everything. Stop by watching **marginal utility**.

**Good fits**: properties that must hold for all inputs / all orderings, many
boundaries and edges, concurrency, cross-boundary contracts, things where "a
config mistake becomes an incident", trust boundaries.

**Poor fits / when to stop**:
- **Probabilistic properties** (expectations, distributions) → not a fit for
  Z3/TLC (only rounding error in integer arithmetic is doable with Z3).
- **Control-theoretic properties** (e.g. PID convergence) → faithfully modeling
  gains/errors is heavy and does not pay off.
- **Re-verifying an optimization algorithm already brute-forced in unit tests**
  → that is the domain of tests, not specs.
- Mass-producing **merely confirmatory propositions** after the big ones are
  out → only makes CI heavier.
- Once the main invariants, dangerous holes, and cross-boundary contracts are
  covered, **working through the accumulated counterexamples and clarifying
  questions with the domain has higher leverage than new formalization**.

**Anti-patterns**:
- Forcing a concurrent model onto a serial dependency.
- Building the model from a summary or memory without reading the
  implementation (getting one conversion table wrong yields a false proof).
- Treating "green" as OK without broken variants (green that verifies nothing).
- Verification that depends on the network or manual setup (not reproducible).

---

## 8. One full loop (summary)

```
Pick a target area (use the bug-pattern catalog in §2 to guess where bugs are likely)
  ↓
Read the implementation: extract declared spec / implicit behavior separately (throw the §3 questions mechanically)
  ↓
Write the decision function `predict` and invariants (§4). Decide: prove it, or find a counterexample?
  ↓
Run Z3 (all inputs) or TLA+ (all orderings) or exhaustive enumeration (§1)
  ↓
Counterexample → ask the domain "is this intended?" with the witness (§0 reconcile)
Holds          → lock it as a contract (regression guard) (§4, §5)
  ↓
Put it in CI with self-checks + broken variants + declarative dependencies (§5)
  ↓
If the model↔code gap matters, check against real logs with trace-checking (§6)
  ↓
Stop by marginal utility (§7). Leave a ledger (implementation claims / check results / clarifying questions)
```

**The final deliverable is not "a pile of proofs" but a ledger of
"implementation claims / machine-check results / clarifying questions for the
domain".** Use formal methods not only to surface bugs but as **a shared
language for aligning spec and understanding**.
