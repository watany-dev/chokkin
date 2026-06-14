---
name: update-design
description: docs/dev/spec.ja.md の設計書を 5 カテゴリ×20点で評価・改善し、ソースコードとの齟齬を双方向で検出する。設計レビューや実装着手前の品質確認、user が「/update-design」と言ったときに発動する。
---

# update-design

chokkin（Python 向け静的到達可能性解析ツール）の設計書 `docs/dev/spec.ja.md` を
体系的に評価・改善するスキル。4 フェーズで検証する。

## Phase 1: コンテキスト収集

`docs/dev/spec.ja.md`（§1–§21）と `src/` の API を対応付け、モジュール階層
（cli/config/manifest/parser/resolver/graph/rules/reporters/fix/plugins）の
依存関係を確認する。`Cargo.toml` の lint 設定（unsafe forbid、unwrap/expect/panic 禁止）
も前提として押さえる。

## Phase 2: 品質評価（100点満点）

5 カテゴリ × 20点で採点する:

| カテゴリ | 評価観点 |
| --- | --- |
| モジュール / struct 設計 | Rust の境界が明確か、`lib.rs` 中心の構成か |
| 静的解析制約 | Python コードを実行しない設計が貫かれているか（import/exec/spawn 禁止） |
| ルール / ポリシー | CHK001–CHK010 の判定ロジックと既定挙動が妥当か |
| エラー処理 | Result/Option 伝播、unwrap 回避、終了コード規約の遵守 |
| テスト容易性 | カバレッジ維持戦略、parser/graph のテスト設計 |

## Phase 3: 整合性チェック

設計書とソースコード間の齟齬を双方向で検出する:
- シンボル名・パスが一致するか
- 現行コードと設計記述が矛盾していないか
- フェーズ順序（roadmap）が依存関係と整合するか

## Phase 4: 改善提案

- 90点以上で実装準備完了、50点未満は再設計が必要と判定する
- 指摘を P0 / P1 / P2 の優先度で記述し、設計書へ反映する
- 採点サマリと主要な所見を末尾に付記する
