# Phase 3.x / v0.4: CHK003 信頼性 + 契約形式化

- 状態: **計画済み**
- 親: `docs/dev/spec.ja.md` §17 Phase 3 (`v0.3〜v0.x 安定化`) / 横断work / §16 v1.0 list
- 日付: 2026-07-01
- 対応リリース: v0.4.0

## 1. 目的

v0.4 は v1.0 凍結前の非破壊マイナーとして、v0.3 で開始した契約安定化を継続する。
`docs/dev/spec.ja.md` 上の Phase 4 は v1.0 のため、本プランは **Phase 3.x** として扱う。

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | CHK003 (`missing_dependency`) の精度が CHK002 ほど計測・分類されていない。package/binary map のデータ収集が手動に寄っている。v1.0 前提の safe autofix / semver 契約が文書化されていない |
| 成果物 | CHK003 計測・分類基準、CHK003 FP 是正、再現可能な map データ収集 flow、ADR 0003 safe autofix contract、ADR 0004 semver contract、硬化テスト |
| 期間目安 | 4-6 週 |
| 非目標 | 既存 JSON/baseline schema version の破壊的変更、既存 `make oss-metrics ARGS=--gate` の CHK003 gate 化、外部 plugin loading の公開、Python project code の実行 |

### v0.4 Exit Criteria

v0.4 の exit criteria は §17 の CHK002 gate と分離して扱う。既存の `make oss-metrics ARGS=--gate`
は CHK002 / crash / speed / CHK002 recall の gate のまま維持する。

| 項目 | 合格条件 |
| --- | --- |
| 既存 gate | `make check` と `make oss-metrics ARGS=--gate` が合格する |
| CHK003 分類 | `target/oss-metrics/findings.tsv` の CHK003 を root-cause bucket 単位で分類し、release validation に分類総数・unknown 数・上位 root cause を記録する |
| CHK003 unknown 上限 | v0.4 release validation 時点で CHK003 unknown を 0 にする。ただし件数が 500 件を超える場合は、上位 95% coverage または上位 200 件のどちらか多い方を分類し、残りを `deferred` として明示する |
| CHK003 FP 削減 | Step 0 baseline から CHK003 FP 件数を削減し、削減対象ごとに regression fixture を追加する |
| CHK003 recall | in-repo CHK003 sentinel を 1 件以上追加し、過剰抑制で missing dependency が消えないことを検証する |
| map 自動化 | 外部取得は opt-in に分離し、CI は pinned snapshot / seed から生成物の再現性だけを検証する |
| 契約文書 | ADR 0003 / ADR 0004 を land し、既存 JSON/baseline/CLI/exit-code 互換方針と矛盾しない |
| 非破壊 | v0.3 の JSON/baseline reader 互換、exit code 意味、ignore syntax、既存 CLI flag 意味を維持する |

## 2. 現状と前提

### 2.1 v0.3.0 時点の事実

- CHK002 は Phase 1.5 で FP gate を通過済み。recall sentinel も `2/2`。
- `scripts/oss-metrics.sh` は CHK002 と CHK003 を `findings.tsv` に出すが、gate 判定は CHK002 のみ。
- `scripts/oss-fixtures.labels.tsv` は CHK003 label を受け付けるが、実質的な CHK003 分類は未整備。
- `scripts/generate-package-map.py` は `data/package-map.seed.json` / `data/binary-map.seed.json` と `AUTO_PACKAGES` から Rust 生成物を作る。
- fix 実装は `src/fix/` にあり、`FixOptions`, `SkippedReason`, atomic write, containment, dry-run が存在する。
- JSON reporter と baseline は `schema_version: "1"` を持つ。`additionalProperties: true` として将来 field 追加を許容している。

### 2.2 設計制約

- 解析対象の Python project code は実行しない。PyPI / wheel / dist-info の調査も analyzed project の import/exec ではなく、外部 metadata の静的読取に限定する。
- `main.rs` は CLI dispatch と exit code mapping に留め、新ロジックは `src/lib.rs` 配下の module に置く。
- production code で `unwrap` / `expect` / `panic` を使わない。
- path 処理は `std::path` API を使い、POSIX separator 前提にしない。
- v0.4 では既存 machine-readable output の破壊的変更をしない。

## 3. Step 0: CHK003 計測設計を固定

最初の PR で、コード変更より先に CHK003 の測り方を固定する。

### 3.1 実施内容

```bash
make oss-clones
make oss-metrics
```

- `target/oss-metrics/findings.tsv` から CHK003 の分布を出す。
- `scripts/oss-fixtures.labels.tsv` に CHK003 label を追加する。
- label は `fp` / `tp` に加えて、note に root-cause bucket を必ず書く。
- release validation に CHK003 の `reported`, `fp`, `tp`, `unknown`, `deferred` を記録する。

### 3.2 CHK003 分類ルール

| verdict | 意味 |
| --- | --- |
| `tp` | manifest に直接宣言されていない third-party import で、chokkin の CHK003 が正しい |
| `fp` | 直接宣言済み、first-party/workspace 由来、optional/dev/transitive の扱いなどにより CHK003 として出すべきではない |
| `unknown` | 調査未完了。v0.4 release validation では 0 または明示的な `deferred` へ移す |
| `deferred` | 件数上限を超えたため v0.5 以降に回す。release validation に数と代表例を残す |

`scripts/oss-metrics.sh` は現在 `fp` / `tp` / `unknown` を前提にしているため、`deferred`
を導入する場合は report 上では unknown とは別集計にする。既存 `--gate` の合否には入れない。

### 3.3 Root-Cause Bucket

| bucket | 例 | 対応方針 |
| --- | --- | --- |
| `map-gap` | import root と distribution name が一致しない | seed map 追加 + regression fixture |
| `workspace-boundary` | workspace member / root dependency 境界の誤判定 | resolver / rules の member context を確認 |
| `optional-import` | optional dependency / extra guard / try import | parser optional import evidence と missing rule を接続 |
| `dev-context` | test/docs/dev import を runtime CHK003 として扱う | context policy を CHK003 にも適用 |
| `transitive-policy` | lockfile 上は推移依存だが直接宣言なし | CHK003 / CHK004 の policy と message を確認 |
| `metadata-gap` | venv RECORD / entry_points / dist-info から mapping できる | metadata reader / bundled map を補強 |

## 4. A: CHK003 信頼性改善

### A1. Map Gap 是正

対象: `src/resolver/bundled/package_modules.rs`, `data/package-map.seed.json`,
`scripts/generate-package-map.py`, resolver tests。

実施内容:

1. Step 0 で `map-gap` と分類された CHK003 を seed に追加する。
2. import root が複数ある distribution は複数 mapping を保持する。
3. map 追加ごとに最小 fixture を作り、CHK003 が消えることを確認する。
4. generated Rust file は script から再生成し、手編集しない。

Exit:

- `map-gap` root cause の既知 FP が削減される。
- seed と generated file が一致する。
- `make check` が通る。

### A2. Optional / Conditional Import の CHK003 接続

対象: `src/parser/`, `src/rules/deps/missing.rs`, `src/rules/deps/reconcile.rs`。

実施内容:

1. `try/except ImportError`, `TYPE_CHECKING`, platform guard, extra guard の既存 evidence を確認する。
2. 宣言済み optional dependency の利用 evidence と CHK003 の missing 判定を分離する。
3. optional import でも直接宣言がない third-party import は CHK003 の候補として残す。
4. 過剰抑制を防ぐ CHK003 sentinel を追加する。

Exit:

- optional/dev evidence による FP は削減する。
- genuine missing dependency は残る。
- CHK002 recall sentinel を壊さない。

### A3. Dev Context / Workspace Context の CHK003 Policy

対象: `src/sources/context.rs`, `src/plugins/context.rs`, `src/rules/deps/context.rs`,
`src/rules/deps/missing.rs`, workspace tests。

実施内容:

1. test/docs/dev file からの import を runtime CHK003 と同じ重みで扱うべきかを rule policy として固定する。
2. `--strict` の workspace member-local policy と default policy を分けて明文化する。
3. member-local CHK003 は `workspace_member` を維持し、reporter output の existing shape を壊さない。

Exit:

- default と `--strict` の CHK003 挙動が fixture で固定される。
- JSON/SARIF/GitHub reporter の field 追加は backward-compatible な追加に限る。

## 5. B: Map データ収集の再現可能化

### B1. 生成 flow の分離

`scripts/generate-package-map.py` は CI で使う deterministic generator として維持する。
外部取得は別 command / option に分離する。

| flow | ネットワーク | 入力 | 出力 | CI での扱い |
| --- | --- | --- | --- | --- |
| deterministic generate | なし | `data/*.seed.json` | `src/resolver/bundled/*.rs` | 必須 |
| metadata harvest | opt-in | pinned package list / local dist-info / downloaded wheel metadata | seed candidate JSON | 通常 CI では実行しない |

### B2. Opt-In Harvest の制約

- `--update-from-pypi` のような明示 flag なしに network access しない。
- 取得した package list は日付・source・hash を持つ snapshot として保存する。
- analyzed project の Python code は実行しない。
- wheel / dist-info metadata を読む場合も static archive extraction のみ。
- candidate は seed へ直接上書きせず、review 可能な diff として出す。

Exit:

- seed から generated file が再現できる。
- network なしの `make check` が維持される。
- harvest output は review 可能で、取り込みすぎによる FP 増加を検出できる。

## 6. C: 契約形式化

### C1. ADR 0003 Safe Autofix Contract

追加先: `docs/adr/0003-safe-autofix-contract.md`

固定する内容:

- Scope: `--fix`, `--dry-run`, `--allow-remove-files`, `--add-missing`
- Safety: root containment, symlink escape rejection, same-directory atomic write, permission preservation
- Applicability: `Confidence::Certain` の dependency issue を中心にし、それ以外は `SkippedReason` で説明する
- Reporting: applied / skipped / reminders を stderr に出す既存 behavior
- Lockfile: manifest edit 後に `uv lock` / `poetry lock` reminder を返すが、lockfile は自動更新しない
- Idempotency: 同じ fix の二重適用で追加重複しない

硬化テスト:

- dry-run が file を変更しない。
- root 外 path / symlink escape が拒否される。
- atomic write が権限を維持する。
- add-missing が duplicate を作らない。
- unsupported target は skipped として報告され、panic しない。

### C2. ADR 0004 Semver Contract

追加先: `docs/adr/0004-semver-contract.md`

breaking change の定義は既存 schema 方針に合わせる。

Breaking:

- JSON / baseline の既存 required field の削除・rename・型変更。
- existing field の意味変更。
- baseline fingerprint inputs の変更。ただし旧 fingerprint を読み続ける migration がある場合を除く。
- 同じ issue set に対する exit code 意味変更。
- CLI flag の削除、既存 flag の意味変更、既存 workflow に対する新 required flag 追加。
- ignore syntax の既存構文を読めなくする変更。
- rule ID の削除・rename、既定 severity の互換性を壊す変更。

Non-breaking:

- optional top-level JSON field の追加。
- optional issue field の追加。
- `additionalProperties: true` の範囲内での reporter metadata 追加。
- 新 rule code の追加。ただし default severity と exit code impact を release note に書く。
- 新 CLI flag の追加。
- warning/info の説明文改善。

Exit:

- `docs/dev/schema-migration-notes.md` と矛盾しない。
- `docs/schema/` の `additionalProperties` policy と矛盾しない。
- v1.0 条件「2 minor version 連続で breaking change なし」の判定基準になる。

## 7. PR 分割

| PR | 内容 | 依存 | 検証 |
| --- | --- | --- | --- |
| 1 | Step 0: CHK003 計測 report と label policy。必要なら `deferred` 集計を追加 | なし | `make oss-metrics`; `make check` |
| 2 | CHK003 sentinel fixture 追加 | 1 | `scripts/run-oss-fixture.sh --build`; `make check` |
| 3 | A1 map-gap 是正 + seed/generator consistency test | 1 | `make check`; `make oss-metrics` |
| 4 | A2 optional/conditional import の CHK003 policy 修正 | 1,2 | `make check`; fixture |
| 5 | A3 dev/workspace context の CHK003 policy 修正 | 1,2 | `make check`; workspace fixture |
| 6 | B deterministic generator check + opt-in harvest design | 3 | `python3 scripts/generate-package-map.py`; `make check` |
| 7 | ADR 0003 safe autofix contract + hardening tests | なし | `cargo test --locked fix`; `make check` |
| 8 | ADR 0004 semver contract + docs sync | なし | `make check` |
| 9 | v0.4.0 release validation docs | 1-8 | `make check`; `make oss-clones`; `make oss-metrics ARGS=--gate`; `make bench`; `scripts/run-oss-fixture.sh --build` |

## 8. リスク管理

| リスク | 対応 |
| --- | --- |
| CHK003 が大量で全件分類が現実的でない | Step 0 で件数を測り、500 件超なら上位 95% coverage または上位 200 件の大きい方まで分類し、残りは `deferred` として release validation に残す |
| CHK003 gate 化で既存 OSS gate が赤化する | v0.4 では existing `--gate` に CHK003 を入れない。別指標として report し、v0.5 以降に gate 昇格を判断する |
| map harvest が非決定的になる | network 取得を opt-in に分け、CI は pinned seed からの deterministic generation のみ検証する |
| CHK003 FP 削減が over-suppression になる | CHK003 sentinel と `tp` label を追加し、genuine missing dependency が残ることを確認する |
| semver ADR が既存 schema 方針と矛盾する | `additionalProperties: true` と field 追加互換の既存方針を ADR に明記する |
| autofix contract が実装より広くなる | ADR は現行 `src/fix/` の保証に限定し、未実装保証は future work とする |

## 9. update-plan 検証サマリ

### 9.1 採点

| カテゴリ | 点 | 評価 |
| --- | ---: | --- |
| モジュール / struct 設計 | 18/20 | `resolver`, `parser`, `rules`, `fix`, `scripts` の境界を分けた |
| 静的解析制約 | 20/20 | analyzed project code の実行禁止、metadata 静的読取、network opt-in を明記 |
| ルール / ポリシー | 18/20 | CHK003 の label / root-cause / gate 非昇格 / semver 互換方針を固定 |
| エラー処理 | 18/20 | `SkippedReason`, unsupported target, non-gate CHK003 指標、deferred 分類を明記 |
| テスト容易性 | 19/20 | sentinel, fixture, generator consistency, make check, oss metrics, bench を PR ごとに配置 |

総合: **93/100**。

### 9.2 レビュー指摘の反映

| 優先度 | 指摘 | 反映 |
| --- | --- | --- |
| P0 | 「CHK003 全件分類」と「全件分類は必須ではない」が矛盾 | exit criteria を unknown 0 または explicit `deferred` に整理 |
| P1 | `Phase 4 / v0.4` が spec の v1.0 Phase 4 と衝突 | `Phase 3.x / v0.4` に変更 |
| P1 | CHK003 は現行 `oss-metrics --gate` に入っていない | v0.4 では既存 gate を維持し、CHK003 は別指標として扱う |
| P1 | JSON field 追加を breaking とする定義が既存方針と矛盾 | optional field 追加は non-breaking、既存 field の削除・rename・型変更を breaking と定義 |
| P2 | PyPI top-N 自動取得が非決定的 | network harvest を opt-in に分離し、CI は pinned seed generation のみ |

### 9.3 整合性チェック

| 対象 | 結果 |
| --- | --- |
| `docs/dev/spec.ja.md` §17 Phase 3 | OK。v0.4 は `v0.3〜v0.x 安定化` の継続 |
| `docs/dev/spec.ja.md` §17 Phase 4 | OK。v1.0 の Phase 4 とは分離 |
| `scripts/oss-metrics.sh` | OK。既存 gate は CHK002 のまま、CHK003 は別指標として拡張 |
| `docs/dev/schema-migration-notes.md` | OK。field 追加互換方針と semver ADR 方針を一致 |
| `src/fix/` | OK。safe autofix ADR は現行実装の保証範囲を超えない |
