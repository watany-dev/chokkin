# Formal models (docs/dev/formal)

`src/` の主要な判定ロジックを形式手法でモデル化し、仕様 (`docs/dev/spec.ja.md`) や
CPython の意味論との差分を機械的に探索するためのモデル群。Rust コードを実行せず、
各モデルは対象関数の忠実な移植と、期待される性質 (property) の組で構成する。

| モデル | 対象 | 手法 | 性質 |
|---|---|---|---|
| `relative_import_model.py` | `parser/relative.rs`, `sources/layout.rs` | 有限領域の全数探索 vs CPython `_resolve_name` | P1 相対 import 解決の健全性、P2 index に無いファイルからの相対 import を解決しない |
| `Reachability.tla` + `MCReachability.{tla,cfg}` | `reachability/bfs.rs`, `reachability/build.rs`, `graph/edges.rs`, `reachability/trace.rs` (module→file 解決、親 package、dynamic import、plugin `module_refs`、framework glob seed を含む) | TLA+ / TLC | ReachSound, ParentReached, FrameworkFollowed, EntryTraceKept, NoDuplicateEdge, ViaFaithful, TraceNonEmpty, Terminates |
| `deps_rules_z3.py` | `rules/deps/{missing,misplaced,context}.rs` (§10、workspace member と `--strict` を含む) | Z3 (SMT) | S1 CHK004 は governing な宣言に無いときのみ、S2 dev/type-only の runtime 使用は CHK005 のみ、W1 CHK003/004/005 は排他、W2 strict で member 未宣言なら root に fallback して CHK005、W3 strict で member が runtime 宣言なら何も出ない |
| `exit_status_z3.py` | `rules/emit.rs`, `baseline/store.rs`, `rules/filter.rs` | Z3 (SMT) | E1 baseline 適用後の exit status が emit と同じ意味論 / E2 baseline が exit 0 を exit 1 に変えない |
| `ignore_model.py` | `rules/ignore.rs` (§18) | 有限領域の全数探索 | I1 dependency 系 rule の config ignore は distribution 名 glob（CHK008 は binary 名も可）、I1b distribution 未解決なら ignore しない |

## 実装に対する検証 (kani / proptest)

上表のモデルは移植に対する検証のため、移植と実装の乖離は検出できない。#418 で一部の
性質を Rust 実装そのものに対して検証するようにした (`make kani`、`make check` には含めない)。

| 性質 | 検証 | 場所 |
|---|---|---|
| `exit_status_z3.py` E1 / E2 | kani (`compute_exit_status`、issue 2 件の任意の kept 部分集合・3 通りの `no_exit_code`) | `src/rules/emit.rs` `mod verification` |
| `deps_rules_z3.py` の `matches_usage` 表 (S1/S2/W1 の前提) | kani (`bucket_matches_usage` の全組合せ、`declaration_matches_usage` の自 group + include 元 group) | `src/rules/deps/context.rs` `mod verification` |
| `relative_import_model.py` P1 / P2 | proptest (CPython `_resolve_name` を参照モデルとする差分テスト、src / flat layout) | `src/parser/relative.rs` `mod props` |

Z3 / 全数探索モデルは workspace member や baseline の組合せなど kani では重い性質のために残す。

## 実行方法

```bash
pip install z3-solver
make formal   # Python モデルを全て実行 (個別実行: python3 docs/dev/formal/<name>.py)

# TLA+ (tla2tools.jar は https://github.com/tlaplus/tlaplus/releases から取得)
cd docs/dev/formal
java -cp /path/to/tla2tools.jar tlc2.TLC -deadlock MCReachability
# 個別 invariant を確認する場合は MCReachability.cfg の INVARIANTS 行を絞る
```

各スクリプトは性質が成立すれば exit 0、反例があれば反例を出力して exit 1 で終了する。
TLC は最初に違反した invariant で停止するため、複数の invariant を個別に確認する場合は
`INVARIANTS` を 1 つずつ指定する。

## モデルの更新

対象の Rust 関数を変更した場合は対応するモデルの移植部分も更新し、性質が成立する
(exit 0 / `No error has been found`) ことを確認する。モデルと実装が乖離したままだと
反例が実装の不具合なのかモデルの不具合なのか判断できなくなる。
