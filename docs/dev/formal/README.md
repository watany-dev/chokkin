# Formal models (docs/dev/formal)

`src/` の主要な判定ロジックを形式手法でモデル化し、仕様 (`docs/dev/spec.ja.md`) や
CPython の意味論との差分を機械的に探索するためのモデル群。Rust コードを実行せず、
各モデルは対象関数の忠実な移植と、期待される性質 (property) の組で構成する。

| モデル | 対象 | 手法 | 性質 |
|---|---|---|---|
| `relative_import_model.py` | `parser/relative.rs`, `sources/layout.rs` | 有限領域の全数探索 vs CPython `_resolve_name` | P1 相対 import 解決の健全性、P2 index に無いファイルからの相対 import を解決しない |
| `import_precedence_model.py` | `resolver/resolve.rs` (`resolve_import_root` の backport → stdlib → first-party の順) | 有限領域の全数探索 vs CPython の `sys.meta_path` と `sys.path` の順 | P1 絶対 import の origin が CPython と一致する。例外は D1 (flat layout の root に置いた stdlib 名の module を、`python main.py` は import するが chokkin は stdlib とみなす) だけ |
| `Reachability.tla` + `MCReachability.{tla,cfg}` | `reachability/bfs.rs`, `reachability/build.rs`, `reachability/module_index.rs`, `sources/layout.rs` (`shadows`), `graph/edges.rs`, `reachability/trace.rs` (importer ごとの module→file 解決 (`resolve_from`: pytest の `sys.path`、index、script の隣や `sys.path` hint の fallback)、親 package、dynamic import、`"pkg." + name` の prefix 付き dynamic import (package と親、配下の全 module)、plugin `module_refs` (自 module を登録する参照は ref の置き場所から導く)、framework glob seed、§6 の eager / exposed / certain: lazy・optional・test context の import、`library subpackage` だけが名指す root を含む) | TLA+ / TLC | ReachSound, ClassSound (各 class 以上の集合が 1 本ずつの BFS と一致), SetsNested, ParentReached (prefix 付き import の package を含む), ClassPassedOn, FrameworkFollowed, FollowedAtMostFour, EntryTraceKept, ViaFaithful, TraceNonEmpty, Terminates |
| `deps_rules_z3.py` | `rules/deps/{missing,misplaced,context}.rs` (§10、workspace member、`--strict`、member と root の lockfile の scope を含む) | Z3 (SMT) | S1 CHK004 は governing な宣言に無いときのみ、S2 dev/type-only の runtime 使用は CHK005 のみ、W1 CHK003/004/005 は排他、W2 strict で member 未宣言なら root に fallback して CHK005、W3 strict で member が runtime 宣言なら何も出ない、W4 usage に合う宣言を持つ member は root によらず何も出ない、L1 lock の edge があれば optional import でも CHK004 / certain (warning)、L2 edge の無い lock entry は CHK004 / likely か optional の CHK003、L3 member の CHK004 / certain は member の宣言からの edge が要る (#649)、L4 lockfile が無ければ CHK004 は出ない |
| `exit_status_z3.py` | `rules/emit.rs`, `rules/filter.rs`, `rules/types.rs` (fingerprint), `baseline/store.rs` | 有限領域の全数探索 + Z3 (SMT) | F1 fingerprint が (rule, member, subject) に対して単射 (`:` を含む path と member を含む)、E1 baseline 適用後の exit status が新しい issue だけに対する emit と一致、E2 baseline が exit 0 を exit 1 に変えない、E3 行だけ違う issue は抑制される、E4 書いた run に当てた baseline は exit 0 |
| `ignore_model.py` | `rules/ignore.rs` (§18) | 有限領域の全数探索 | I1 dependency 系 rule の config ignore は distribution 名 glob（CHK008 は binary 名も可）で、pattern の綴り揺れに依らない、I1b distribution 未解決なら ignore しない、I2 集約 finding (CHK010 の行、CHK005 の import site) は全 site が抑制されるときだけ抑制し、CHK005 は抑制されない site のうち最強のものに立つ |
| `fix_plan_model.py` | `fix/plan.rs` (`plan_move_to_runtime`、`governing_manifest`)、`fix/pyproject.rs` (`move_group_to_runtime`)、`rules/deps/{misplaced,missing,reconcile,used}.rs` (§10 CHK005 と §13、workspace member・`--strict`・lockfile extra 経由の index を含む) | 有限領域の全数探索 (root・member 各 3 件までの宣言 × static 有無 × `--strict` × import site) | F1 適用した move の finding は再解析で消える、F2 move は finding を決めた manifest を書き換える、F3 governing な manifest に pyproject の dev 宣言が 1 件だけで runtime 宣言が無く static なら move する、F4 計画した move はすべて適用できる (同じ entry を 2 度動かさない) |
| `version_guard_model.py` | `parser/version_guard.rs` (`VersionTest::read` / `holds`、`branch_minors`、`minor_runs`)、`parser/condition.rs` (`can` / `fixed` / `excluded`)、`parser/visit.rs` (入れ子の積)、`resolver/stdlib.rs` (`excludes_minors`) (#997) | 有限領域の全数探索 vs Python のタプル比較と `if` / `elif` / `else` の意味論 | V1 `holds` が `sys.version_info` / `[:2]` / `[0]` の両辺・6 演算子で Python の比較と一致し、最後の bit が以降の全 minor を代表する、V2 `can` が健全で opaque leaf の重複が無ければ正確、V3 `excluded` が健全、V4 `never_runs` が `requires-python` の窓 (上限なしを含む) の全 minor・opaque の全値で健全、V5 `minor_runs` が bit 集合を過不足なく表す。version test に束縛した module 変数 (#1272) は対象外 |
| `setup_cfg_requirements_model.py` | `manifest/setup_cfg.rs` (`ini_lines`、`append_value`、`one_line_options`、`setup_cfg_requirement_items`)、`manifest/setuptools_reqs.rs` (#989) | 有限領域の全数探索 vs stdlib `configparser` + setuptools 80.9.0 (`_parse_list_semicolon`、`parse_strings`) | P1 `install_requires` の要件テキストが setuptools と順序まで一致 (D1 末尾 `\` の扱いを除く)、P2 `one_line_options` は configparser の値に改行が無い option と一致 |
| `testpaths_glob_model.py` | `sources/layout.rs` (`expand_testpath`、`matching_children`、`dirs_from`、`testpath_entries`、`unmatched_testpaths`) と globset 0.4 の 1 component 照合 (#998) | 有限領域の全数探索 vs 実ディレクトリ上の stdlib `glob.glob(recursive=True)` (pytest `_decide_args`) | T1 entry が test にする file の集合が pytest と一致、T2 「何も指さない」warning が pytest の glob が空のときに限る (D1 symlink、D2 字義どおりの entry を除く) |

v0.7.10 で入った 3 モデルは `--broken` で意図的に壊した移植を走らせ、性質がそれを反例として捕まえることも確かめる (`make formal` が両方を走らせる)。

## v0.7.10 の台帳

v0.7.10 の変更を `.claude/skills/formal-spec-extraction` の手順で形式化した結果。「実装の主張」はコードのコメント・テスト・CHANGELOG が述べる仕様、「検査」はモデルの結果、「確認事項」は反例のうち意図か不具合かを決める必要があるもの。確認事項はモデルに `Q` 番号で登録してあり、決着したら意図なら `D` に移し、不具合なら実装を直してモデルから外す (外さないと stale として CI が落ちる)。

### 実装の主張と検査結果

| 変更 | 実装の主張 | 検査 | 結果 |
|---|---|---|---|
| #997 `sys.version_info` ガード | 読むのは `sys.version_info` / `[:2]` とリテラルタプルの比較 (と `[0]` / `.major`) だけで、他は opaque。`requires-python` の全 minor で除外される分岐の import は CHK003/004/005 にならない | `version_guard_model.py` V1–V5、`stdlib.rs` `range_follows_the_minors_requires_python_allows` に `excludes_minors` の健全性 (PEP 440 で評価した許可 minor と交わらない) を追加 | 成立。壊した移植 3 種 (`full` の長さを無視、opaque を free 扱い、`max` を含む比較) はすべて反例になる |
| #989 setup.cfg の 1 行の値を `;` で分割 | setuptools の `_parse_list_semicolon` と同じく、1 行の値は `;` で、複数行の値は行で分ける | `setup_cfg_requirements_model.py` P1/P2 (25,732 通り) | 分割規則は成立 (P2)。分割の前段の INI 読みで Q1・Q2 の乖離。#989 以前の読み (常に行で分割) は反例になる |
| #998 testpaths の glob 展開 | pytest の `glob.iglob(entry, recursive=True)` と同じ path を指す。何も指さない entry は warning | `testpaths_glob_model.py` T1/T2 (4,368 entry) | `*` `?` `[...]` と隠し名の規則は成立。`**` と globset の構文差で Q3–Q6 の乖離 |

### 確認事項 (反例)

| # | 反例 | chokkin | setuptools / pytest | 影響 |
|---|---|---|---|---|
| Q1 | `[options]` の直後にインデントした key (`  install_requires = a;b`) | 行を読み飛ばし、`install_requires` が無い | configparser は option として読む | 宣言がすべて消え、その import が CHK010 / 宣言漏れになる (ADR 0005 の「黙って捨てる」に当たる) |
| Q2 | `]` の後に文字がある section header (`[options] ; c`) | header と認めず、以下の option は前の section に入る | `SECTCRE` は前方一致で `options` を開く | Q1 と同じ |
| Q3 | globset が拒む component (閉じない `[`、逆順の範囲 `[z-a]`) | その component は何にも一致せず、entry は字義どおり残る (D2) | `t[1` は字義どおりの名前に一致し、`[z-a]` は何にも一致しない | `t[1/*` のような entry が「何も指さない」warning になり test context が付かない |
| Q4 | `[^a]` | 否定の class | `^` と `a` の class | 一致する名前が逆になる |
| Q5 | `tests/*/**` のように、file に一致した component の後の `**` | `dirs_from` が file を base として残し、その file が test になる | `_glob2` は directory のときだけ base を返す | pytest が集めない file に test context が付く。`dirs_from` で base が directory か確かめれば直る |
| Q6 | 最後の component の `**` | directory だけを返す | file も返す | root 直下でだけ差が出る (`testpaths = **` で root の file が test にならない) |

Q1・Q2 は依存宣言を失う方向なので優先度が高い。Q3–Q6 は `testpaths` に書かれることが稀な形で、Q5 だけが 1 行で直せる不具合。

## 実装に対する検証 (kani / proptest)

上表のモデルは移植に対する検証のため、移植と実装の乖離は検出できない。#418 で一部の
性質を Rust 実装そのものに対して検証するようにした (`make kani`、`make check` には含めない)。
kani は `verification.yml` が週次と、`#[cfg(kani)]` の harness を持つ file を変えた PR で走らせる。
全数列挙と proptest は `make check` の `cargo test` で走る。

| 性質 | 検証 | 場所 |
|---|---|---|
| `exit_status_z3.py` E1 / E2 | kani (`compute_exit_status`、issue 2 件の任意の kept 部分集合・3 通りの `no_exit_code`) | `src/rules/emit.rs` `mod verification` |
| `deps_rules_z3.py` の判定表 (#1108) | 全数列挙 (`#[test]`、usage context 3 × root と member の宣言 bucket × `--strict` × optional import × root lockfile (無し・entry・root / member の宣言からの edge) × member lockfile を実際の宣言と lockfile に組んで `detect_missing_dependencies` と `detect_misplaced_dependencies` を走らせ、モデルの `decide` を Rust に写した参照と rule・severity・confidence を比べる) | `src/rules/deps/missing.rs` `decisions_follow_the_formal_model` |
| `exit_status_z3.py` F1 の前提 (#1108) | proptest (`target_part` の結果が `:` を含まず、元に戻せる) | `src/rules/types.rs` `target_parts_hold_no_colon_and_decode_back` |
| `deps_rules_z3.py` の `matches_usage` 表 (S1/S2/W1 の前提) | kani (`bucket_matches_usage` の全組合せ、`declaration_matches_usage` の自 group + include 元 group) | `src/rules/deps/context.rs` `mod verification` |
| §10 CHK005 の confidence 表 (#860) | kani (同じ distribution の import 2 件の全組合せで `ImportSignals::weigh` / `Weight::absorb` / `settle` を §10 の表から書いた参照モデルと比べる、集約が順序に依らない、origin が finding と同じ強さ、Certain ⇔ certain な file のトップレベル import が opt-in でも host-loaded でもない) | `src/rules/deps/misplaced.rs` `mod verification` |
| §10 CHK009 の重複判定 (#860) | 全数列挙 (`#[test]`、3 context × marker 有無 × specifier × extras の宣言 3 件と、`-r` で 2 context から読まれる行を install の参照モデルと比べる: 判定が対称、context 間の重複を消しても runtime を含む install は変わらない、`removable_duplicates` の行を消してもどの install も変わらない) | `src/rules/deps/duplicate.rs` `duplicates_follow_the_install_model` |
| §12 CHK010 の集約と inline ignore (#860) | 全数列挙 (`#[test]`、行 1 / 2 / なし × guard 有無の site 1〜3 件 × file context × ignore 行の組を参照モデルと比べる: severity は site 単独のうち最も強いもの、anchor は失敗しうる最初の行、inline ignore が効く ⇔ 全 site の行に directive がある) | `src/rules/symbols/analyze.rs` `unresolved_imports_merge_as_the_sites_would_alone` |
| §8 CHK001 の confidence / severity と `exit_status_z3.py` E1 (#860) | 全数列挙 (`#[test]`、project / workspace member (library member と、metapackage root が配りうる app member) の mode × root・member の wheel surface (未宣言・含む・含まない) × root が member を配るか (#1060) × opaque dynamic import の gap × deprecated shim × `--strict` の orphan 1 件を、到達性・surface の再採点・gap・emit・exit status まで通して参照モデルと比べる: 利用者が import しうる library の file は maybe / warning で、`--strict` 以外では exit code を変えない) | `src/reachability/build.rs` `library_orphans_callers_may_import_never_fail_the_run` |
| §8 `mode = "auto"` の解決 (#952) | 全数列挙 (`#[test]`、distribution 名 (なし・あり・`setup.py` の動的な名前) × flat / src layout × package (なし・`__init__.py` なし・あり) × entry point 8 通り × app を示す file 名と置き場所 46 通りを §8 の表から書いた参照モデルと比べる: root の app / library、workspace member の library 判定、明示した `mode` はそのまま) | `src/entry/mode.rs` `mode_follows_the_spec_table` |
| §10 file context と usage context の表 (#954) | 全数列挙 (`#[test]`、root・入れ子を含む workspace member・名前が前方一致するだけの dir の下で、深さ 3 までの dir 列 × file 名を path から書いた参照モデルと比べる: discovery の `FileContext`、`--production` で残る file、未 discovery の file の context、全 `ImportContext` との組の usage context) | `src/rules/deps/context.rs` `usage_context_follows_the_spec_table` |
| §10 import の optional / platform guard / deferred / context (#953) | proptest (`try` / `except` / `else` / `finally`、`with suppress(...)`、`if TYPE_CHECKING` / `sys.platform` / `__name__ == "__main__"`、`ImportError` を raise する package check、def / class の入れ子の中の import を、file context と `__main__.py` / notebook の path ごとに参照モデルと比べる: 文を出ると flag が戻る、fallback の root は `except ImportError:` の中だけ。条件式の組合せは `src/parser/condition.rs` `mod props` (#861)) | `src/parser/visit.rs` `import_flags_follow_the_enclosing_statements` |
| §8 entry root の導出 (#1109) | 全数列挙 (`#[test]`、flat / src layout × root・package 直下・深い package・`scripts/`・`.github/`・`examples/`・`docs/`・`alembic/`・`tests/`・workspace member の各所 17 通り × file 名 13 通り × app / library × `__main__` block 有無 × `--production` を §8 の規則表から書いた参照モデルと比べる。console / gui の entry point (`module:Obj.attr` を含む) と `python -m` (dev / deploy) は、候補 file 7 個の全部分集合で path → module 名の対応から書いた参照モデルと比べる: root の集合、context、symbol、対応しない target の warning) | `src/entry/build.rs` `path_roots_follow_the_spec_table` / `module_targets_follow_the_module_names` |
| §10 CHK002 の workspace source の使用 (#1109) | proptest (member 2〜4 個の root 側の証拠 (`main.py`・到達しない file・`[project.scripts]`・pytest `-p`・test) と member の src / tests の import の任意の組を、最小不動点の参照モデルと比べる: CHK002 が一致、member の宣言順に依らない、証拠や import を足しても CHK002 が増えない) | `tests/prop_workspace_used.rs` |
| §7 stdlib の窓と backport (#1109) | 全数列挙 (`#[test]`、2 節までの `requires-python` 約 1 万通り × target 3.7〜3.15 × 出荷する minor の範囲ごとの代表 root を、PEP 440 を minor・patch ごとに評価する参照モデルと比べる: `contains` / `ships_only_outside` が一致、`always_contains` は許す全 minor が出荷するときだけ。backport は root 4 個 × `requires-python` 5 通り × 宣言 5 通りを表と比べる) | `src/resolver/stdlib.rs` `range_follows_the_minors_requires_python_allows`、`tests/resolver_resolve.rs` `removed_stdlib_modules_follow_the_backport_table` |
| §13 `--fix` の事後収束 (#1109) | proptest (宣言 (なし・runtime・dev・両方) と import の場所 (entry・test・orphan・orphan subpackage) の組、orphan の連鎖、`__init__.py` の有無で、`--fix --allow-remove-files` の後の再解析が新しい finding を出さず、適用した finding を残さない。CHK001 の削除だけでは他の finding が変わらない) | `tests/prop_fix_converge.rs` |
| `Reachability.tla` ClassSound (§6 の 4 集合) | proptest (5 module・test file 1 件の任意の import site (static / dynamic / prefix 付き dynamic、lazy・`TYPE_CHECKING`、`try`・platform guard)、root、library mode / member、plugin 参照、framework seed で、`run_reachability_bfs` の reachable / eager / exposed / certain を class ごとに 1 本ずつ回す BFS の参照モデルと比べる) | `src/reachability/bfs.rs` `derived_sets_match_one_bfs_each` |
| §10 CHK002 の `requires-python` / marker の区間代数 (#1103) | proptest (`PythonRange` の `union` / `intersect` / `complement` / `is_subset_of` が正規形を保ち束の法則 (交換・結合・吸収・ド・モルガン・対合) を満たす。`marker_range` / `requires_python_range` を、各段 0〜4 の `X.Y.Z` を総当たりして PEP 440 で marker / specifier を評価する参照モデルと比べる。モデル外の比較 (`===`、pre / post / dev release、local、epoch、4 段以上、他の marker 変数、`in`) を含む入力は `None` で、`marker_always_holds` は `true` を返さない。深い入れ子を含む任意の入力で panic しない) | `src/manifest/python_range.rs` `mod props` |
| ADR 0005 の gap による findings の弱め方 (#1103) | proptest (任意の finding と gap で、gap なしなら不変、gap は confidence / severity を上げず finding 自体と origin を変えない、gap を足すと emit の issue 数が増えず exit status も悪化しない。`GapKind` の doc から書いた kind → rule の網羅表を参照モデルにして、宣言していない rule と別 member の finding には、語彙で書けるどの file / site / target の gap も届かない) | `src/rules/gaps.rs` `mod props` |
| §12 CHK006 / CHK007 の参照判定 (#1103) | proptest (app mode の 3 module で定義・`__all__` (全部読める 5 形)・module 内の読み取り・`from m import` / `import m; m.name` / package からの import・`__init__.py` の再export、慣例で読まれる `*_pb2.py` / `modular_x.py` を生成し、名前集合で書いた参照モデルと end-to-end で比べる: import を足しても報告は増えず、module の番号と文の順序に依らない) | `tests/prop_symbols.rs` |
| §19 解析キャッシュ (#1103) | proptest (cold 実行の後、manifest・`requirements.txt` とその `-r` 先・import・source・`.gitignore`・`chokkin.toml` を 1 つずつ変えるたびに、キャッシュありの実行が同じ tree の `--no-cache` 実行と JSON 出力・exit code まで一致する) | `tests/prop_cache_warm.rs` |
| `relative_import_model.py` P1 / P2 | proptest (CPython `_resolve_name` を参照モデルとする差分テスト、src / flat layout) | `src/parser/relative.rs` `mod props` |

Z3 / 全数探索モデルは workspace member や baseline の組合せなど kani では重い性質のために残す。

同じ名前の module が同じ directory に package と module で並ぶとき (`foo/__init__.py` と `foo.py`)、CPython の path finder は package を先に試す。`ModuleIndex::build` (doctools の `module_files` も同じ `shadows` を使う)、pytest の `sys.path` (`find_module`)、entry 解決 (`resolve_in`) がそれに従うことは、それぞれ `a_package_shadows_the_module_beside_it`、`a_test_package_wins_over_the_module_beside_it`、`a_package_wins_over_the_module_beside_it` で確かめる。別の directory どうしの重複 (flat と src の併存、member をまたぐ同名 package) は `sys.path` の順で決まり、静的には分からないので、graph で先に来た file を採る。

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
