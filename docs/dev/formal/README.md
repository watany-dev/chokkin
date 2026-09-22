# Formal models (docs/dev/formal)

`src/` の主要な判定ロジックを形式手法でモデル化し、仕様 (`docs/dev/spec.ja.md`) や
CPython の意味論との差分を機械的に探索するためのモデル群。Rust コードを実行せず、
各モデルは対象関数の忠実な移植と、期待される性質 (property) の組で構成する。

| モデル | 対象 | 手法 | 性質 |
|---|---|---|---|
| `relative_import_model.py` | `parser/relative.rs`, `reachability/module_index.rs` | 有限領域の全数探索 vs CPython `_resolve_name` | P1 相対 import 解決の健全性、P2 `file_module_name` と `path_to_module` の一致 |
| `Reachability.tla` + `MCReachability.{tla,cfg}` | `reachability/bfs.rs`, `reachability/build.rs`, `graph/edges.rs`, `reachability/trace.rs` | TLA+ / TLC | ReachSound, NoDuplicateEdge, ViaFaithful, TraceNonEmpty |
| `deps_rules_z3.py` | `rules/deps/{missing,misplaced,context}.rs` (§10) | Z3 (SMT) | S1 CHK004 は未宣言のときのみ、S2 dev/type-only の runtime 使用は CHK005 のみ |
| `exit_status_z3.py` | `rules/emit.rs`, `baseline/store.rs`, `rules/filter.rs` | Z3 (SMT) | E1 baseline 適用後の exit status が emit と同じ意味論 / E2 baseline が exit 0 を exit 1 に変えない |
| `ignore_model.py` | `rules/ignore.rs` (§18) | 有限領域の全数探索 | I1 dependency 系 rule の config ignore は distribution 名 glob |

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
