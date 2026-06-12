---
name: update-docs
description: src/ の実装変更を README.md / README.ja.md / docs/dev/ / CLAUDE.md / AGENTS.md に同期する。コード変更後やドキュメントの陳腐化が疑われるとき、user が「/update-docs」と言ったときに発動する。
---

# update-docs

yokei の進化するソースコードと陳腐化しがちなドキュメントの差分を体系的に埋める
スキル。5 フェーズで実施する。

## Phase 1: コード監査

実装の現状を把握する:
- `src/lib.rs` の public API（モジュール階層 cli/config/manifest/parser/resolver/
  graph/rules/reporters/fix/plugins のうち実装済みのもの）
- `src/main.rs` の CLI 挙動（引数ディスパッチ、ExitStatus → 終了コード）
- `Cargo.toml` / `pyproject.toml` のメタデータ（バージョン、edition、maturin bin 設定）

## Phase 2: 設計ドキュメント更新

`docs/dev/spec.ja.md`（§1–§21）を実装と突き合わせる:
- API 変更を反映する
- ステータスラベル（design phase / pre-alpha / 実装済み）を実態に合わせる
- ルール YOK001–YOK010 と reporters（default/compact/JSON/Markdown）の記述を整合させる

## Phase 3: README 更新

`README.md`（英）と `README.ja.md`（日）の両方に以下が含まれるか確認・更新する:
- プロジェクト概要 1 段落（Python 向け到達可能性解析・未使用 files/deps/symbols 検出、
  Knip 相当）
- インストール／ビルド手順（maturin wheel、`make` ターゲット）
- CLI 利用例と終了コード（0 = 問題なし、非 0 = 未使用検出 / エラー）
- `make` ターゲット一覧

## Phase 4: 整合性チェック

全 Markdown 間で次を揃える:
- コマンド名・パス・クレート識別子・ルール番号・終了コード・用語
- 言語規約: `README.md` と `CLAUDE.md` / `AGENTS.md` は英語、`README.ja.md` と
  `docs/dev/` 配下（`spec.ja.md` 等）は日本語
- 「Python コードを絶対に実行しない（static parse only）」の制約が崩れていないか

## Phase 5: 報告

構造化レポートを出す:
- 更新したファイル一覧
- ドキュメント化候補（実装にあるが未記載の項目）
- 設計判断が必要な未解決の不整合
