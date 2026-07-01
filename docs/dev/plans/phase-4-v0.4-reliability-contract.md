# Phase 4 / v0.4: 信頼性(A 主軸) + 契約形式化(B 軽量並行)

- 状態: **計画済み**
- 親: `docs/dev/spec.ja.md` §17 Phase 4 / §16 v1.0 list
- 日付: 2026-07-01
- 対応リリース: v0.4.0

## 1. 目的

v1.0 凍結に向けた非破壊マイナー。v0.3→v0.4 で「2 minor 連続・非破壊」カウントの1本目。

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | CHK003（missing dependency）誤検知が未計測・未分類。map データ収集が手動依存。v1.0 契約が文書化未了 |
| 成果物 | CHK003 label 分類＋FP 是正、map データ収集の自動化、autofix/semver 契約 ADR + 硬化テスト |
| 期間目安 | 4–6 週 |
| exit criteria | make check + oss-metrics --gate 合格、CHK003 全件分類済み＋FP 削減、ADR 0003/0004 landed、非破壊、v0.4.0 リリース |

## 2. 背景

### 現状（v0.3.0 時点）

- CHK002（unused dependency）FP 0% / recall 2/2 / crash 0 ✅
- CHK003（missing dependency）未分類（label 0件、§17 注記「現状1747件」は全件未分類）
- map 生成 pipeline は `scripts/generate-package-map.py` が存在するが、seed は手動保守
- fix 契約の実装はほぼ完了（atomic write / containment / idempotency / SkippedReason）だが文書化未了
- v1.0 条件「2 minor version 連続で breaking change なし」の起点は v0.3

### 発見事項（コード調査より）

1. `scripts/generate-package-map.py` が `data/package-map.seed.json` + `data/binary-map.seed.json` + `AUTO_PACKAGES` から `package_modules.rs` / `binaries.rs` を生成済み（`// @generated` ヘッダ付き）。本当のギャップは **データ収集の自動化（PyPI top-N / dist-info からの自動取り込み）**。
2. CHK003 label は `oss-fixtures.labels.tsv` に **0件**。metrics は「FP/unknown」件数を表示するだけ。
3. fix 契約の **実装はほぼ完了**（`src/fix/write.rs` / `containment.rs` / `apply.rs`）。B は「実装を ADR + 硬化テストで凍結」が中心。

## 3. Step 0: CHK003 現状計測（A のゲート、最初に実行）

以下をこのプランの最初の PR として実施する。

```bash
make oss-clones          # 既存 clone があれば skip
make oss-metrics         # CHK003 findings 抽出
```

- findings.tsv から CHK003 件数・分布を確認
- `oss-fixtures.labels.tsv` に CHK003 label を `fp`/`tp` で分類（現在0件）
- CHK003 FP rate のベースライン確立
- 判断: CHK003 を §17 gate に昇格するか、§20 信頼性指標のまま FP 削減だけやるか

## 4. A: 信頼性・データ品質（主軸）

### A1. CHK003 FP 是正

Phase 1.5 のパターン（4.A–4.D）を CHK003 に適用:

| 根因 | インパクト見積もり | 対応 |
| --- | --- | --- |
| map gap（import名≠distribution名） | 高い | seed map 拡張 + regression fixture |
| optional/conditional import 判定漏れ | 中 | `missing.rs` の optional import 扱いを改善 |
| dev context 誤判定 | 中 | dependency-group / extras 文脈を CHK003 にも反映 |
| transitive 誤判定 | 低 | lockfile なし transitive の扱いを明確化 |

各 root cause に regression fixture を追加して CI 化する。

### A2. map データ収集の自動化

- `generate-package-map.py` を拡張: PyPI top-N JSON API / dist-info metadata からの自動取り込み
- 手動 seed 保守の範囲を縮減
- 生成物は既存 `@generated` ファイル形式を維持（非破壊）

### A3. OSS 検証セット拡充（real tp label）

- 20 プロジェクト中の「本当に未宣言な依存」を CHK003 tp label に追加
- recall guard を in-repo sentinel 超に強化

## 5. B: 契約形式化（軽量並行）

### B1. Safe autofix contract ADR（`docs/adr/0003-safe-autofix-contract.md`）

既存実装の保証を文書化:

- **Atomicity**: 同ディレクトリ temp → sync_all → rename（`write.rs`）
- **Root containment**: 絶対パス/親/ルート/symlink 脱出拒否（`containment.rs`）
- **Idempotency**: 二重適用で結果不変（既存テスト `add_runtime_dependency_is_idempotent` 他）
- **Scope**: `--fix` / `--allow-remove-files` / `--add-missing` / `--dry-run`
- **Skipped reporting**: `SkippedReason` enum + stderr 出力
- **Lockfile reminders**: uv.lock / poetry.lock 変更後通知

硬化テストで各保証を lock する。

### B2. Semver contract ADR（`docs/adr/0004-semver-contract.md`）

breaking change の定義:

- JSON schema field の追加/削除/名称変更
- exit code の意味変更
- CLI flag の削除・意味変更・必須化
- ignore syntax の構文変更
- rule ID / severity / default config の変更
- reporter 出力形状の変更（JSON key / SARIF property / GitHub annotation format）

v1.0 凍結の前提文書とする。

## 6. 分割戦略

各 PR は1論理変更 + regression fixture + 文書更新を原則とする。

### PR 分割案

| # | 内容 | 依存 | 予想サイズ |
| --- | --- | --- | --- |
| 1 | Step 0: CHK003 計測＋label 分類（code 変更なし、data のみ） | なし | S |
| 2 | A1a: map gap 是正（seed 拡張 + regression fixture） | 1 | M |
| 3 | A1b: optional import 判定改善 | 1 | M |
| 4 | A1c: dev context CHK003 反映 | 1 | S |
| 5 | A2: map データ収集自動化（generate-package-map.py 拡張） | 2 | M |
| 6 | A3: OSS tp label 追加 | 5 | S |
| 7 | B1: ADR 0003 + 硬化テスト | なし | M |
| 8 | B2: ADR 0004 | なし | S |
| 9 | v0.4.0 release validation | 1–8 | S |

### CI / 検証

各 PR の CI gate:

```text
make check
make oss-metrics ARGS=--gate
cargo test --locked
```

v0.4.0 リリース前:

```text
make check
make oss-clones && make oss-metrics ARGS=--gate
make bench
scripts/run-oss-fixture.sh --build
```

## 7. リスク管理

| リスク | 対応 |
| --- | --- |
| CHK003 分類作業が重い（1747件超の可能性） | 自動化支援スクリプトで scaffold、サンプリングで優先度判定、全件分類は必須ではない |
| CHK003 gate 昇格で OSS gate が赤化 | まず §20 指標のまま削減、gate 昇格は v0.5 以降 |
| map 自動化で取り込みすぎて FP 増 | 生成物は既存形式維持・差分 review |
| B が A を遅らせる | B は文書中心・別 PR で並行、A のクリティカルパスに乗せない |
| 非破壊原則に抵触する変更 | 各 PR のレビューで「v1.0 条件（非破壊連続）」を確認。疑わしい変更は v0.5 以降に deferred |

## 8. update-plan 自己評価

| カテゴリ | 点 | 評価 |
| --- | ---: | --- |
| 目的とスコープ | 18/20 | CHK003 信頼性 + 契約形式化に絞れている |
| 既存設計との整合 | 19/20 | §16–§17 / §20 と一致。コード調査で実装状態を確認済み |
| 実装分割 | 17/20 | PR 境界を定義。A1 根因の複数 PR は計測結果で調整 |
| 検証可能性 | 18/20 | oss-metrics --gate / make check / regression fixture を列挙済み |
| リスク管理 | 17/20 | 非破壊原則を明記。CHK003 計測後に scope 調整あり |

総合: **89/100**（Step 0 で CHK003 計測後、A1 の scope を確定させて 90 超えを目指す）
