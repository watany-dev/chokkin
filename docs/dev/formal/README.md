# Formal models (docs/dev/formal)

`src/` の主要な判定ロジックを形式手法でモデル化し、仕様 (`docs/dev/spec.ja.md`) や
CPython の意味論との差分を機械的に探索するためのモデル群。Rust コードを実行せず、
各モデルは対象関数の忠実な移植と、期待される性質 (property) の組で構成する。

| モデル | 対象 | 手法 | 性質 |
|---|---|---|---|
| `relative_import_model.py` | `parser/relative.rs`, `sources/layout.rs` | 有限領域の全数探索 vs CPython `_resolve_name` | P1 相対 import 解決の健全性、P2 index に無いファイルからの相対 import を解決しない |
| `Reachability.tla` + `MCReachability.{tla,cfg}` | `reachability/bfs.rs`, `reachability/build.rs`, `graph/edges.rs`, `reachability/trace.rs` (module→file 解決、親 package、dynamic import、plugin `module_refs`、framework glob seed、§6 の eager / exposed / certain: lazy・optional・test context の import、`library subpackage` だけが名指す root、自 module を登録する plugin 参照を含む) | TLA+ / TLC | ReachSound, ClassSound (各 class 以上の集合が 1 本ずつの BFS と一致), SetsNested, ParentReached, ClassPassedOn, FrameworkFollowed, FollowedAtMostFour, EntryTraceKept, ViaFaithful, TraceNonEmpty, Terminates |
| `deps_rules_z3.py` | `rules/deps/{missing,misplaced,context}.rs` (§10、workspace member と `--strict` を含む) | Z3 (SMT) | S1 CHK004 は governing な宣言に無いときのみ、S2 dev/type-only の runtime 使用は CHK005 のみ、W1 CHK003/004/005 は排他、W2 strict で member 未宣言なら root に fallback して CHK005、W3 strict で member が runtime 宣言なら何も出ない |
| `exit_status_z3.py` | `rules/emit.rs`, `baseline/store.rs`, `rules/filter.rs` | Z3 (SMT) | E1 baseline 適用後の exit status が emit と同じ意味論 / E2 baseline が exit 0 を exit 1 に変えない |
| `ignore_model.py` | `rules/ignore.rs` (§18) | 有限領域の全数探索 | I1 dependency 系 rule の config ignore は distribution 名 glob（CHK008 は binary 名も可）で、pattern の綴り揺れに依らない、I1b distribution 未解決なら ignore しない、I2 集約 finding (CHK010 の行、CHK005 の import site) は全 site が抑制されるときだけ抑制し、CHK005 は抑制されない site のうち最強のものに立つ |

## 実装に対する検証 (kani / proptest)

上表のモデルは移植に対する検証のため、移植と実装の乖離は検出できない。#418 で一部の
性質を Rust 実装そのものに対して検証するようにした (`make kani`、`make check` には含めない)。
kani は `verification.yml` が週次と、`#[cfg(kani)]` の harness を持つ file を変えた PR で走らせる。
全数列挙と proptest は `make check` の `cargo test` で走る。

| 性質 | 検証 | 場所 |
|---|---|---|
| `exit_status_z3.py` E1 / E2 | kani (`compute_exit_status`、issue 2 件の任意の kept 部分集合・3 通りの `no_exit_code`) | `src/rules/emit.rs` `mod verification` |
| `deps_rules_z3.py` の `matches_usage` 表 (S1/S2/W1 の前提) | kani (`bucket_matches_usage` の全組合せ、`declaration_matches_usage` の自 group + include 元 group) | `src/rules/deps/context.rs` `mod verification` |
| §10 CHK005 の confidence 表 (#860) | kani (同じ distribution の import 2 件の全組合せで `ImportSignals::weigh` / `Weight::absorb` / `settle` を §10 の表から書いた参照モデルと比べる、集約が順序に依らない、origin が finding と同じ強さ、Certain ⇔ certain な file のトップレベル import が opt-in でも host-loaded でもない) | `src/rules/deps/misplaced.rs` `mod verification` |
| §10 CHK009 の重複判定 (#860) | 全数列挙 (`#[test]`、3 context × marker 有無 × specifier × extras の宣言 3 件と、`-r` で 2 context から読まれる行を install の参照モデルと比べる: 判定が対称、context 間の重複を消しても runtime を含む install は変わらない、`removable_duplicates` の行を消してもどの install も変わらない) | `src/rules/deps/duplicate.rs` `duplicates_follow_the_install_model` |
| §12 CHK010 の集約と inline ignore (#860) | 全数列挙 (`#[test]`、行 1 / 2 / なし × guard 有無の site 1〜3 件 × file context × ignore 行の組を参照モデルと比べる: severity は site 単独のうち最も強いもの、anchor は失敗しうる最初の行、inline ignore が効く ⇔ 全 site の行に directive がある) | `src/rules/symbols/analyze.rs` `unresolved_imports_merge_as_the_sites_would_alone` |
| §8 CHK001 の confidence / severity と `exit_status_z3.py` E1 (#860) | 全数列挙 (`#[test]`、project / workspace member の mode × root・member の wheel surface (未宣言・含む・含まない) × opaque dynamic import の gap × deprecated shim × `--strict` の orphan 1 件を、到達性・surface の再採点・gap・emit・exit status まで通して参照モデルと比べる: 利用者が import しうる library の file は maybe / warning で、`--strict` 以外では exit code を変えない) | `src/reachability/build.rs` `library_orphans_callers_may_import_never_fail_the_run` |
| §8 `mode = "auto"` の解決 (#952) | 全数列挙 (`#[test]`、distribution 名 (なし・あり・`setup.py` の動的な名前) × flat / src layout × package (なし・`__init__.py` なし・あり) × entry point 8 通り × app を示す file 名と置き場所 46 通りを §8 の表から書いた参照モデルと比べる: root の app / library、workspace member の library 判定、明示した `mode` はそのまま) | `src/entry/mode.rs` `mode_follows_the_spec_table` |
| §10 file context と usage context の表 (#954) | 全数列挙 (`#[test]`、root・入れ子を含む workspace member・名前が前方一致するだけの dir の下で、深さ 3 までの dir 列 × file 名を path から書いた参照モデルと比べる: discovery の `FileContext`、`--production` で残る file、未 discovery の file の context、全 `ImportContext` との組の usage context) | `src/rules/deps/context.rs` `usage_context_follows_the_spec_table` |
| §10 import の optional / platform guard / deferred / context (#953) | proptest (`try` / `except` / `else` / `finally`、`with suppress(...)`、`if TYPE_CHECKING` / `sys.platform` / `__name__ == "__main__"`、`ImportError` を raise する package check、def / class の入れ子の中の import を、file context と `__main__.py` / notebook の path ごとに参照モデルと比べる: 文を出ると flag が戻る、fallback の root は `except ImportError:` の中だけ。条件式の組合せは `src/parser/condition.rs` `mod props` (#861)) | `src/parser/visit.rs` `import_flags_follow_the_enclosing_statements` |
| `Reachability.tla` ClassSound (§6 の 4 集合) | proptest (5 module・test file 1 件の任意の import site (static / dynamic、lazy・`TYPE_CHECKING`、`try`・platform guard)、root、library mode / member、plugin 参照、framework seed で、`run_reachability_bfs` の reachable / eager / exposed / certain を class ごとに 1 本ずつ回す BFS の参照モデルと比べる) | `src/reachability/bfs.rs` `derived_sets_match_one_bfs_each` |
| `relative_import_model.py` P1 / P2 | proptest (CPython `_resolve_name` を参照モデルとする差分テスト、src / flat layout) | `src/parser/relative.rs` `mod props` |

Z3 / 全数探索モデルは workspace member や baseline の組合せなど kani では重い性質のために残す。

## 実行方法

```bash
pip install z3-solver
make formal   # Python モデルを全て実行 (個別実行: python3 docs/dev/formal/<name>.py)

# TLA+ (tla2tools.jar は CI と同じ v1.7.4 を
# https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar から取得)
make tlc TLA2TOOLS=/path/to/tla2tools.jar   # 数秒
# 個別 invariant を確認する場合は MCReachability.cfg の INVARIANTS 行を絞る
```

各スクリプトは性質が成立すれば exit 0、反例があれば反例を出力して exit 1 で終了する。
TLC は最初に違反した invariant で停止するため、複数の invariant を個別に確認する場合は
`INVARIANTS` を 1 つずつ指定する。

`Reachability.tla` で状態 (queue など) を引き回す再帰演算子を書くときは、前の段の結果を
`LET` で受けて `e[1]`, `e[2]` と渡さず、`Bind` で 1 度だけ評価した値を渡す。TLC は
`Terminates` の公平性 (`WF_vars(Next)`) のための ENABLED 評価では `LET` も演算子引数も
キャッシュしないので、参照のたびに前の段を評価し直し、所要時間が段数に対して指数的に
増える (#853 では 12 状態で約 7 分かかった)。

## CI とモデルの更新

`.github/workflows/formal.yml` が `make formal` と `make tlc` を実行する。`pull_request`
の paths は `docs/dev/formal/**` と上表の「対象」列の Rust ファイルで、加えて週次 (月曜) と
`workflow_dispatch` でも走る。モデルの編集や z3-solver / TLC の更新で性質が崩れれば
job が失敗する。

モデルは Rust を読まない移植なので、Rust の変更だけで job が失敗することはない。代わりに
対象ファイルを変えた PR で対応するモデルが変わっていなければ、job は「どのモデルの移植を
見直すか」を warning 注釈で出す。対象関数の意味を変えた PR では、その移植部分も同じ PR で
更新し、性質が成立する (exit 0 / `No error has been found`) ことを確かめる。モデルと実装が
乖離したままだと、反例が実装の不具合なのかモデルの不具合なのか判断できなくなる。

対象ファイルを増減したら、上表と `formal.yml` の paths・warning の対応表を一緒に直す。
