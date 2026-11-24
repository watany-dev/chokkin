# patterns: pattern 方言の Lean 4 オラクル

設定の pattern を、その設定を読むツールの方言 (fnmatch、gitignore、Python glob、
tox factor、setuptools find、pytest `python_files`) で解釈する仕様と、その仕様を
oracle として chokkin と突き合わせる harness。結果と issue は `ledger.ja.md`。

- `Patterns/*.lean`: 方言ごとの仕様と証明
- `Main.lean`: `oracle` 実行ファイル (1 行 1 JSON の問い合わせに答える)
- `harness/validate_spec.py`: 仕様を本物の実装 (CPython、git、tox、setuptools、pytest) と照合
- `harness/diff_chokkin.py`: 仕様と `target/release/chokkin` の差分
- `harness/minimize_gitignore.py`: gitignore の不一致の最小化
