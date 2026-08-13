# Phase 3.x Step 0: CHK003 計測・分類 + 全ルール label coverage 棚卸し

- 状態: **完了**
- 親: [`phase-3x-v0.4-reliability-contract.md`](./phase-3x-v0.4-reliability-contract.md) §3 (Step 0) / issue #85 WS1
- 日付: 2026-08-11
- 対応リリース: v0.4.0 (PR 1–2 相当)

## 1. 目的

v0.4 の後続作業 (A1–A3 の CHK003 FP 是正、#85 WS2 の real-world precision 測定) は
すべて「現状の数値」を入力にする。本 Step はコード挙動を変えずに計測基盤を拡張し、
次の 2 つを同時に解決する:

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | (a) CHK003 が計測・分類されておらず v0.4 A 系の baseline がない。(b) label 分類が CHK002/CHK003 のみで、CHK001 / CHK004–010 の precision が定量的裏付けゼロ (#85 WS1 の指摘) |
| 成果物 | 全 CHK ルール対応の findings 分類、per-rule coverage 表、CHK003 root-cause bucket 分類、CHK003 recall sentinel、`docs/dev/v0.3-stocktake-coverage.md` |
| 期間目安 | 3–5 日 |
| 非目標 | 検出ロジックの変更、CHK003 の gate 昇格、FP 是正そのもの (A1–A3 で実施)、recall 測定 (#85 WS4)、issue #51 の性能改善 |

### Exit Criteria

| 項目 | 合格条件 |
| --- | --- |
| 全ルール分類 | `target/oss-metrics/findings.tsv` が CHK001–CHK010 全 finding を含み、`report.md` に per-rule coverage 表が出る |
| 既存 gate 非破壊 | `make oss-metrics ARGS=--gate` の合否条件 (CHK002 FP / crash / speed / CHK002 recall) が変わらず合格する |
| label 移行の無損失 | 移行後も CHK002 unknown が 0、既存 recall `tp` label が全件 findings に一致する |
| CHK003 分類 | CHK003 unknown を 0 にする。500 件超の場合は上位 95% coverage または上位 200 件の多い方を分類し、残りを `deferred` として明示する (親プラン §3 の規約) |
| CHK003 sentinel | in-repo missing-dependency fixture が 1 件以上 `oss-recall.manifest` に載り、`tp` label が recall gate で検証される |
| 棚卸し報告 | `docs/dev/v0.3-stocktake-coverage.md` に per-rule coverage 実測値と blind spot (CHK001, CHK004–010) を記録する |
| 既存 CI | `make check` が合格する |

## 2. 現状と前提

### 2.1 事実 (v0.3.0 時点)

- `scripts/oss-metrics.sh` は `findings.tsv` に **CHK002/CHK003 のみ**を出力し
  (`measure_one` 内の jq filter)、照合キーは `(slug, code, distribution)`。
- `scripts/oss-fixtures.labels.tsv` (180 行) は
  `slug / code / distribution / verdict / note` の 5 列。verdict は `fp` / `tp` のみ。
- gate は CHK002 FP rate < 5%・crash 0・medium cold run ≤ 2s・CHK002 recall
  (全 `tp` label が findings に出現) の 4 条件。CHK003 は informational。
- JSON reporter は v0.3 で全 issue に安定キー `target`
  (`issue_stable_target`, `src/rules/types.rs:297`) と `fingerprint` を出力済み。
  `target` は subject 種別ごとに path / distribution 名 / `path:symbol` /
  `path:module` を返し、workspace member があれば `member:` を前置する。
- recall sentinel は `scripts/oss-recall.manifest` の 2 fixture
  (`unused_boto3`, `optional_try_import`) で、いずれも CHK002 用。CHK003 用はない。

### 2.2 設計制約

- **Rust コードは変更しない。** 必要な情報 (`target`) は既に JSON に載っており、
  本 Step は `scripts/` と `tests/fixtures/` と `docs/` のみを触る。
  したがって JSON schema / baseline / exit code / CLI flag は無変更で、
  親プラン「非破壊」条件を構造的に満たす。
- sentinel fixture は解析対象の Python ファイルだが、chokkin は静的解析のみで
  fixture コードを実行しない (spec §2 の制約を維持)。
- ネットワークは `make oss-clones` (20 project clone) のみ。metrics 集計と
  in-repo sentinel 検証はネットワークなしで完結する。

## 3. 設計

### 3.1 findings 照合キーを `target` に統一

`measure_one` の jq filter を全 issue に広げ、照合キーを
`(slug, code, target)` に変更する。

```
# findings.tsv (新)
slug	code	target	verdict	bucket	confidence	message
```

- jq: `.issues[]? | [.code, .target, (.confidence // "?"), (.message // "")] | @tsv`
- dependency 系 (CHK002/003/004) の `target` は distribution 名そのものなので、
  既存 label の `distribution` 列は原則そのまま `target` 列として読める。
- 例外は workspace member 付き issue (`member:dist` 形式)。OSS validation set は
  単一 root project 主体だが、移行検証 (§3.5) で機械的に確認する。

### 3.2 labels.tsv v2 フォーマット

```
# slug	code	target	verdict	bucket	note
```

| 列 | 内容 |
| --- | --- |
| `target` | JSON `target` field と厳密一致 (旧 `distribution` 列の一般化) |
| `verdict` | `fp` / `tp` / `deferred`。未記載 finding は従来どおり `unknown` |
| `bucket` | root-cause bucket。CHK003 の `fp` では必須、それ以外は `-` 可 |

bucket の語彙は親プラン §3.3 の 6 種
(`map-gap` / `workspace-boundary` / `optional-import` / `dev-context` /
`transitive-policy` / `metadata-gap`) に固定し、ヘッダコメントに列挙する。
既存 CHK002 行の note は Phase 1.5 の是正理由をすでに含むため書き換えず、
bucket 列に `-` を入れる機械移行とする。

### 3.3 verdict `deferred` の集計

- `deferred` は CHK003 の件数超過時のみ使う分類で、report 上 `unknown` とは
  別カウントする (親プラン §3.2)。
- **gate は変更しない**: gate 評価 (CHK002) では `deferred` を unclassified 扱い
  とし、`deferred` で CHK002 gate をすり抜けられないようにする。
  CHK003 は従来どおり gate 対象外。

### 3.4 report.md への per-rule coverage 表 (#85 WS1)

`report.md` に次の 2 表を追加する:

```
## Per-rule label coverage        ← WS1 の中核成果物
| Rule | Reported | tp | fp | deferred | unknown | Coverage % |
(CHK001–CHK010 全行。Reported=0 の rule も行を出し、corpus 上の
 「検出ゼロ = precision も recall も未検証」という blind spot を可視化する)

## CHK003 root-cause buckets
| Bucket | Count | 代表例 (slug/target) |
```

Coverage % = (tp + fp) / reported。`deferred` は bucket 済みでも ground
truth ではないため分子に含めない。集計は awk で行い、
既存の `fp_count` 系ヘルパーを code パラメタ化して再利用する。

### 3.5 label 移行の無損失検証

1. 既存 180 行を v2 列順に機械変換する (`distribution`→`target`、bucket 列挿入)。
2. 変換後に metrics を再実行し、(a) CHK002 unknown が 0 のまま、
   (b) recall `tp` label が全件一致 (`tp_missed == 0`)、を確認する。
   この 2 条件が「1 行も照合を失っていない」ことの機械的証明になる。
   recall 検証は in-repo sentinel のみで完結するためネットワーク不要、
   (a) の確認は clone 済み環境で行い結果を棚卸し報告に記録する。

### 3.6 CHK003 recall sentinel

`tests/fixtures/deps/missing_dependency/` を新設する:

- `pyproject.toml` は最小限の依存のみ宣言し、`src/` のモジュールが
  **宣言していない** third-party distribution (例: `requests`) を import する。
- `scripts/oss-recall.manifest` に `missing_dependency` 行を追加。
- labels.tsv に `missing_dependency / CHK003 / requests / tp / - / sentinel` を追加。
- 既存 recall 集計は「全 `tp` label が findings に出現すること」を code を問わず
  検証するため、**recall gate のロジック変更なしで** CHK003 の過剰抑制ガードが
  有効になる。これが v0.4 A2/A3 (FP 是正) の安全網になる。

### 3.7 計測実行と分類作業

スクリプト整備後に実測する:

```bash
make oss-clones                    # 20 project (network)
make oss-metrics ARGS=--gate       # 既存 gate が green のままであることを確認
```

- `findings.tsv` の CHK003 を 1 件ずつ調査し、verdict + bucket を labels.tsv に
  追記する。判定規約は親プラン §3.2 (`tp` = 未宣言 third-party import、
  `fp` = 宣言済み / first-party / optional / dev 等)。
- 件数が 500 件を超えた場合は distribution 単位の頻度順に上位 95% coverage
  または上位 200 件の多い方まで分類し、残りへ `deferred` を付ける。
- CHK001 / CHK004–010 の finding は本 Step では **unknown のまま記録する**
  (分類は #85 WS2 以降のスコープ)。coverage 表で blind spot として数値化する
  ことが WS1 の成果物であり、ゼロ埋めの偽 label は作らない。

### 3.8 棚卸し報告 `docs/dev/v0.3-stocktake-coverage.md`

以下を記録する (#85 WS1 DoD):

1. per-rule coverage 表の実測値 (report.md から転記)
2. CHK003 の `reported / tp / fp / unknown / deferred` と bucket 分布
   (v0.4 release validation の Step 0 baseline としてそのまま参照される)
3. blind spot の明文化: 「CHK001, CHK004–010 は corpus 上の precision 裏付けゼロ」
   と、corpus 上 Reported=0 の rule (= recall も未検証) の一覧
4. #85 WS2 以降への引き継ぎ事項 (件数規模、bucket 上位、深掘り候補 slug)

## 4. PR 分割

| PR | 内容 | 依存 | 検証 |
| --- | --- | --- | --- |
| 1 | `oss-metrics.sh` 全ルール化 + `target` キー + `deferred` + coverage 表、labels.tsv v2 機械移行、ヘッダ文書更新 | なし | `make check`; clone なし実行で sentinel 照合を確認; clone 済み環境で `--gate` green + CHK002 unknown 0 |
| 2 | CHK003 sentinel fixture + recall manifest + `tp` label | 1 | `make oss-metrics` で sentinel の CHK003 検出と recall 一致を確認; `make check` |
| 3 | 実測 + CHK003 分類 (labels.tsv 追記) + `docs/dev/v0.3-stocktake-coverage.md` + `plans/README.md` 索引更新 | 1, 2 | `make oss-clones`; `make oss-metrics ARGS=--gate`; `make check` |

PR 1 と 2 はコード変更が `scripts/` + `tests/fixtures/` に閉じ、PR 3 は
labels/docs のみ。どの段階でも Rust 差分ゼロのため cargo-semver-checks への
影響はない。

## 5. リスク管理

| リスク | 対応 |
| --- | --- |
| label 移行で照合キーがずれ、既存分類が unknown 化する | §3.5 の二重検証 (CHK002 unknown 0 + recall 全件一致) を PR 1 のマージ条件にする |
| workspace member 付き `target` (`member:dist`) が旧 label と不一致 | 移行時に findings.tsv 側の実キーを grep し、prefix 付き finding があれば label 側をそちらに合わせる。単一 root の corpus では発生しない見込み |
| CHK003 が大量で全件分類が非現実的 | 親プラン §8 の規約どおり `deferred` に逃がし、件数と代表例を棚卸し報告に残す |
| sentinel の import 先 distribution が将来 bundled map 拡充で first-party 誤解される | sentinel には map に確実に載る著名 distribution (requests 等) を使い、fixture の manifest に宣言しないことで CHK003 判定を安定させる |
| coverage 表で Reported=0 の rule を「問題なし」と誤読される | 表の脚注と棚卸し報告に「Reported=0 は precision/recall とも未検証の意味」と明記する (#85 WS4 への導線) |
| 分類作業の判定ブレ | 判定規約を親プラン §3.2 に一本化し、note に検証方法 (manifest 行 / import 箇所) を必ず書く |

## 6. update-plan 検証サマリ

### 6.1 採点

| カテゴリ | 点 | 評価 |
| --- | ---: | --- |
| モジュール / struct 設計 | 19/20 | Rust 無変更を明示し、`issue_stable_target` の既存契約を再利用。変更を scripts/fixtures/docs に閉じ込めた |
| 静的解析制約 | 20/20 | fixture コード非実行、network は clone のみ、sentinel 検証はオフラインで完結 |
| ルール / ポリシー | 18/20 | gate 非変更・`deferred` の gate すり抜け防止・CHK003 判定規約の親プラン一本化を固定 |
| エラー処理 | 18/20 | 移行無損失の機械的検証 (unknown 0 + recall 一致) を合格条件化。exit code / schema 無変更 |
| テスト容易性 | 19/20 | sentinel による recall 回帰ガード、PR ごとの検証コマンド、clone なし検証経路を明記 |

総合: **94/100**。

### 6.2 整合性チェック

| 対象 | 結果 |
| --- | --- |
| 親プラン §3 (Step 0) | OK。分類規約・bucket 語彙・deferred 規約・500 件規約を継承 |
| 親プラン §7 PR 1–2 | OK。本プランの PR 1–3 が親の PR 1–2 を具体化 (WS1 分を追加) |
| issue #85 WS1 DoD | OK。per-rule tp/fp/unknown 分類 + coverage 表 + 成果物 `v0.3-stocktake-coverage.md` を網羅 |
| `scripts/oss-metrics.sh` | OK。`label_for` / `fp_count` / recall 集計の既存構造を拡張で維持、gate 条件式は不変 |
| `src/rules/types.rs` `issue_stable_target` | OK。照合キーは既存 JSON field の再利用で、Rust 側の変更なし |
| spec §17 横断work | OK。「検証用 OSS project set の regression test 化」の継続に該当 |
