# cargo-mutants 棚卸し（#418 Phase A）

Issue [#418](https://github.com/watany-dev/chokkin/issues/418) の Phase A として、
`src/rules/` と `src/resolver/` に cargo-mutants をかけて MISSED を全件読み、
「テスト不足」と「等価ミュータント」に分類した記録。コード変更は行っていない。

## 実行条件

- cargo-mutants 27.1.0 / rustc 1.96（ruff 系 crate の要求）
- 4 vCPU / 15 GiB のクラウドコンテナ
- 実行コマンド（ディレクトリ単位に分割）:

```sh
CARGO_PROFILE_DEV_DEBUG=0 cargo mutants -f 'src/resolver/**' -j 2 -o target/m-resolver
CARGO_PROFILE_DEV_DEBUG=0 cargo mutants -f 'src/rules/**'    -j 2 -o target/m-rules
```

`CARGO_PROFILE_DEV_DEBUG=0` は、`-j` ごとにできるビルドディレクトリの
サイズと、リンク時間を抑えるために付けた。

## 結果サマリ

| 対象 | ミュータント | caught | missed | unviable | timeout | 所要（`-j 2`） |
|---|---:|---:|---:|---:|---:|---:|
| `src/resolver/` | 173 | 141 | 16 | 16 | 0 | 22 分 |
| `src/rules/` | 413 | 272 | 84 | 57 | 0 | 45 分 |
| 合計 | 586 | 413 | 100 | 73 | 0 | 67 分 |

- viable（caught + missed）に対する kill 率: resolver 90%、rules 76%。
- MISSED 100 件の内訳: **テスト不足 93 件 / 等価ミュータント 7 件**
  （等価の可能性があるものは各表の備考に「等価候補」と書いた）。
- `src/rules/deps/context.rs`（Phase C の kani 対象候補）は全件 caught。

## 所要時間と `-j` / nextest の判断

- 初回ビルド（コピー後のベースライン）: 約 150 秒。
- 1 ミュータントあたり: インクリメンタルビルド 7〜28 秒 + テスト 2〜8 秒。
  **時間の大半はビルド**で、テストは短い。
- **nextest は不要**。テストが短いので、並列化してもほとんど縮まない。
- **`-j 2` が妥当**。4 vCPU では `-j 2` でも各ビルドが CPU を取り合う。
  `-j` を増やすより、`debug=0` でリンクを軽くするほうが効いた。
- 全体で 1 時間以上かかるため、**PR ごとの全件実行はできない**。
  Phase D は当初の計画どおり、PR では `--in-diff`（non-blocking）、
  全件は手動・定期実行（`make mutants`）とする。

## MISSED 一覧と分類

分類: **T** = テスト不足（テストを足せば kill できる）、
**E** = 等価ミュータント（観測可能な振る舞いが変わらない）。
優先度: **P1** = 検出結果（issue の有無・severity・終了コード）が変わる、
**P2** = explain・メッセージ・位置情報などの表示だけが変わる。

### `src/rules/`

#### `chk001.rs`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| 41:13, 41:52, 41:66 | `chk001_severity` の `==`/`&&` 反転 | T | P1 | Library×Maybe → Warning、それ以外 → Error のマトリクスを検証するテストがない |

#### `emit.rs`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| 195:5, 197:49 | `build_summary` → `Default`、`+=` → `*=` | T | **P1** | `IssueSummary` の件数を assert するテストがない。CLI の「Summary: N issues」、JSON の `summary`、baseline store が使っている |
| 96:42 | `explain_issue` の `&&` → `\|\|` | T | P2 | rule が違う issue が先頭にあるケースがない |
| 101:5〜109:62（11 件） | `subject_key_matches` → `true`、各 `==`/`\|\|` 反転 | T | P2 | `--explain` セレクタのテストが issue 1 件だけ。File 以外の subject キーと、複数 issue からの選択が未検証 |
| 127:59 | `candidate_to_issue` の `&&` → `\|\|` | T | P2 | summary と details の片方だけが空のケースがない |
| 170:13, 171:13, 179:13 | `location_from_candidate` の File/Import/ScriptDistribution アーム削除 | T | P2 | origins がないときの subject からの位置フォールバックが未検証 |

#### `filter.rs`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| 38:50 | `passes_rule_filter` の `==` → `!=` | T | P1 | `exclude_rules` の経路にテストがない |

#### `ignore.rs`

subject の種類とルールは 1 対 1 に近い。File は CHK001 だけ、Binary は CHK008 だけ、
Distribution は distribution 系ルールだけ、Symbol は CHK006/007 だけが出す。
この不変条件の下では、ガードを `true` にしても振る舞いは変わらない（E）。

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| 171:40（→`false`）, 221:5（→`false`） | `is_file_rule` を偽にする | T | P1 | CHK001 の config ignore（`[ignore] CHK001 = ["glob"]`）が未検証 |
| 181:40, 184:9, 185:39, 225:5（→`true`） | CHK010 の Import subject の分岐 | T | P1 | CHK010 の path/module glob による ignore が未検証 |
| 163:13 | ScriptDistribution の `&&` → `\|\|` | T | P1 | script 依存に一致しないパターンのケースがない（反転すると常に ignore される） |
| 187:50（→`false`）, 208:5, 217:18 | symbol パターン | T | P1 | `path:symbol` のシンボル名不一致・パス不一致の否定ケースがない |
| 124:9, 125:9, 127:17 | `candidate_line` のアーム削除 | T | P1 | origins を持たない Import と、Manifest origin 付き ScriptDistribution の inline ignore が未検証 |
| 145:5（3 件）, 146:9〜148:9 | `subject_file_path` | T | P1 | origins がない candidate への file-level directive（`# chokkin: ignore-file[...]`）が未検証 |
| 171:40（→`true`）, 221:5（→`true`） | `is_file_rule` を真にする | **E** | — | File subject は CHK001 だけが出す |
| 172:48（→`true`） | Distribution ガード | **E** | — | Distribution subject は distribution 系ルールだけが出す |
| 176:42（→`true`） | Binary ガード | **E** | — | Binary subject は CHK008 だけが出す |
| 187:50（→`true`）, 237:5（→`true`） | `is_symbol_rule` を真にする | **E** | — | Symbol subject は CHK006/007 だけが出す。`symbol_pattern_matches` も Symbol 以外は false を返す |

#### `metadata.rs`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| 8:5（2 件） | `rule_title` → `""` / `"xyzzy"` | T | P2 | SARIF の `shortDescription` を assert していない |

#### `deps/`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| `binary.rs:26:12` | `!reported.insert` の `!` 削除 | T | **P1** | 反転すると CHK008 が 1 件も出なくなるのに検出されない。CHK008 の candidate が出ることを確認するテストがない |
| `missing.rs:71:13, 72:13` | `\|\|` → `&&` | T | P1 | workspace member の import がメンバー側だけで宣言されているケースと、非 strict でルート側だけで宣言されているケースが未検証 |
| `script.rs:69:55` | `&&` → `\|\|` | T | P1 | PEP 723 script 内の first-party import、または distribution 不明の import が未検証 |
| `script.rs:163:43` | `&&` → `\|\|` | T | P1 | optional と platform_guarded の片方だけが立った import が未検証 |
| `script.rs:214:13, 214:17` | marker 条件の反転 | T | P1 | strict モードで marker 付き script 依存を扱うケースがない |
| `unused.rs:92:33` | `build_requires_note` の `==` → `!=` | T | P2 | explain の build-system 注記が未検証 |
| `unused.rs:158:5`（2 件）, 167:34, 179:38 | `top_level_modules_for_distribution` | T | P2 | CHK002 の到達性エビデンス（explain details）が未検証 |
| `unused.rs:242:5` | `is_types_stub` → `true` | **E** | — | 唯一の呼び出し元 `reconcile.rs:147` の直後で `runtime_for_stub` が同じ prefix/suffix 判定をしている。冗長な判定なので、削除して単純化できる |
| `used.rs:43:5, 43:44` | `has_lockfile` | T | P1 | lockfile も transitive edge もないとき、CHK004 を抑止するテストがない |
| `used.rs:103:55, 131:12, 131:38, 131:41, 144:37, 151:16` | `mark_workspace_source_distributions` | T | P1 | uv workspace source の使用判定で、到達不能ファイル・未宣言・非 workspace source・連鎖利用の各分岐が未検証 |
| `used.rs:176:5`（3 件） | `member_path` | T | P1 | 非ルートの workspace member を介した連鎖利用が未検証 |
| `used.rs:218:30` | `index + 1` → `index * 1` | T | P1 | member の `src/` レイアウトからのモジュール名導出が未検証 |

#### `symbols/`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| `exports.rs:85:5`, `graph.rs:175:9` | `is_reexport_used` / `is_referenced` → `false` | T | **P1** | 「使われている re-export は CHK007 にならない」否定ケースがない（反転すると誤検出が出る） |
| `exports.rs:46:54` | `\|\|` → `&&` | T | P1 | 絶対 `from` import、または相対の `import` 文を re-export として扱わないことが未検証 |
| `exports.rs:63:41, 63:84` | `_private` と `__all__` の条件 | T | P1 | `__all__` に載った `_name` の re-export が未検証 |
| `graph.rs:62:34` | `\|\|` → `&&` | T | P1 | 非 public シンボル / `TYPE_CHECKING` 内シンボルの除外が未検証 |
| `graph.rs:69:68` | `in_all` の `==` → `!=` | T | P1 | `__all__` 由来のフラグが結果に効くケースがない |
| `graph.rs:169:52` | `\|=` → `&=` | T | P1 | 外部参照のあとに同一モジュール参照が来る順序のケースがない |
| `analyze.rs:326:5`（2 件） | `symbol_kind_label` | T | P2 | CHK006 メッセージの種別ラベルが未検証 |

### `src/resolver/`

| 位置 | ミュータント | 分類 | 優先度 | 備考 |
|---|---|---|---|---|
| `first_party.rs:41:37` | `is_workspace_import` の `\|\|` → `&&` | T | P1 | member id と basename が異なるケースがない |
| `first_party.rs:53:48` | `==` → `!=` | T | P1 | `config.workspaces` 上書き時の否定ケースがない |
| `first_party.rs:72:13` | `ProjectMetadata.name` の削除 | T | P1 | dist 名と同名の flat layout パッケージが未検証 |
| `maps.rs:103:51` | `namespace_candidates` の `==` → `!=` | T | P1 | 候補 1 件なら Certain、複数なら Maybe という confidence の区別が未検証 |
| `maps.rs:131:5` | `sort_dedup_map_values` → `()` | T | P2 | 重複・未整列の入力がない。現行の入力経路ではすでに整列済みの可能性があり、等価候補 |
| `metadata.rs:23:27` | `name.is_none()` ガード → `true` | T | P1 | description 本文などに 2 つ目の `Name:` 行がある METADATA が未検証 |
| `pytest_path.rs:57:54` | conftest フィルタの `==` → `!=` | T | P1 | `conftest.py` 以外が混ざるケースがない |
| `pytest_path.rs:152:9` | `is_same_or_under` の `\|\|` → `&&` | T | P1 | サブディレクトリのケースがない |
| `resolve.rs:463:25` | `>` → `>=` | T | P1 | 候補 1 件で AmbiguousImport が出ないことを assert していない |
| `resolve.rs:476:33` | `==` → `!=` | T | P1 | override なしのときの confidence が未検証 |
| `venv.rs:124`（3 件） | `top_level.txt` の空行・`_` 始まりの除外 | T | P1 | 該当する入力がない。fixture では RECORD 側と重複しているため、等価候補 |
| `venv.rs:181:28` | `.dist-info/` のスキップ | T | P1 | 未検証 |
| `venv.rs:195:24` | 先頭が `.` のパス（`../../bin/...`）のスキップ | T | P1 | 未検証 |
| `venv.rs:222:44` | `entry_points.txt` のコメント・空行・グループ外の行 | T | P1 | 未検証 |

## 所見

1. **kill 率が低いのは `ignore.rs` / `deps/used.rs` / `symbols/`**。いずれも
   「否定ケース」（ignore しない、使用済みなので報告しない）のテストが薄い。
   Phase B の proptest 参照モデルは、ignore マッチングと
   CHK006/007 の参照判定を最初の対象にするのが効果的。
2. **`build_summary` と CHK008 の出力を見ているテストがない**。
   どちらも P1 で、小さな unit test で kill できるため、Phase B より先に単発で埋めてよい。
3. 等価ミュータント 7 件のうち 6 件は「subject の種類 ↔ ルール」の不変条件から来ている。
   この不変条件は型では表現されていないので、Phase C（kani）か
   `debug_assert!` で明示する候補になる。残る `is_types_stub` の 1 件は冗長な判定で、
   削除すれば等価ミュータント自体がなくなる。
4. `src/rules/deps/context.rs` はすでに全件 caught。Phase C の kani は、
   テストの穴埋めではなく全域の証明として位置付ける。
