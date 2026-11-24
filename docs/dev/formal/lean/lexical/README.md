# O3: Python lexical / line-structure oracle (Lean 4)

Specs written from the external standards, not ported from `src/`:

| File | Standard |
|------|----------|
| `Lexical/Lines.lean` | Language Reference §2.1.2 physical lines (LF, CR LF, CR) |
| `Lexical/Tokens.lean` | §2.1.3 comments, §2.4 string literals, PEP 701 f-strings: which `#` starts a COMMENT token |
| `Lexical/Directive.lean` | spec §18 `# chokkin:` directives, applied to COMMENT tokens only |
| `Lexical/Coding.lean` | PEP 263 / §2.1.4, CPython's `get_normal_name` and `encodings.search_function` |
| `Lexical/CodecTable.lean` | generated from CPython 3.13 `encodings` by `scripts/gen_codecs.py` |
| `Lexical/Script.lean` | PEP 723 reference regex on universal-newline text |

`Lexical/Proofs.lean` proves, for every input (no `sorry`):

1. `terminators_univ`: line numbers do not depend on LF / CR LF / CR.
2. `lex_comments_wf`: every COMMENT token starts with `#` and holds no line terminator.
3. `inline_ignore_is_comment`: an inline ignore counted at line `l` comes from a directive inside a COMMENT token on line `l` (so never from a string, never across a line end).
4. `declared_first_two_lines`: the PEP 263 declaration depends on the first two lines only.

`oracle FILE...` (`lake build`, toolchain in `lean-toolchain`) prints one JSON line per file. The harness under `harness/` checks the spec against CPython (`tokenize`, `compile`, `ast`) first and then compares chokkin's `--reporter json` output with it. Findings and how to rerun them: `ledger.ja.md`.

The specs recurse once per character, so the harness runs the oracle with `ulimit -s unlimited`.
