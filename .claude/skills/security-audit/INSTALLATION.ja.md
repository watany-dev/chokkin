# security-audit の導入情報

Cloudflare 公式の、コードベースを対象とするセキュリティ監査スキル。
Cloudflare のアカウント設定を監査する API ツールではない。

- 配布元: https://github.com/cloudflare/security-audit-skill
- 導入コミット: `c1c8a8c1471069fb0e188eeaff69b8e8db6564a8`
- 導入日: 2026-10-18
- ライセンス: MIT。原文は同梱の `LICENSE`。
- `skills/security-audit/` の全20ファイルを収録。監査手順と検証コードは変更なし。
- ローカル差分は `SKILL.md` の `disable-model-invocation: true` の追加。
- このファイル、`LICENSE`、`agents/openai.yaml` は導入時に追加。

## 配置

このリポジトリを開いたときに利用するプロジェクト単位の導入。

| ツール | 読込先 |
| --- | --- |
| Claude Code | `.claude/skills/security-audit/SKILL.md` |
| Codex | `.agents/skills/security-audit/SKILL.md` |
| 従来の Codex 共有設定 | `.codex/skills/security-audit/SKILL.md` |

実体は `.claude/skills/security-audit/`。
`.agents/skills/security-audit` はその実体への相対シンボリックリンク。
既存の `.codex/skills -> ../.claude/skills` でも同じ実体を参照する。

## 使い方

両 CLI とも明示的な呼出し専用。通常のレビューやセキュリティ相談からは
自動選択しない。

| ツール | 自動呼出しを無効にする設定 |
| --- | --- |
| Claude Code | `SKILL.md` の `disable-model-invocation: true` |
| Codex | `agents/openai.yaml` の `policy.allow_implicit_invocation: false` |

リポジトリを開いて、Claude Code では次を入力する。

```text
/security-audit このコードベースをセキュリティ監査して
```

Codex では次を入力する。

```text
$security-audit このコードベースをセキュリティ監査して
```

局所的な相談なら、呼出しコマンドに「src/ の入力処理をセキュリティレビュー
して」と続ける。通常の相談・部分レビューと、
レポートを生成する全工程の監査は、公式スキルの動作モードに従う。
一覧に表示されない場合は CLI のセッションを再起動する。

同梱の検証スクリプトには Node.js が必要。全工程の監査にはサブエージェント
対応も必要で、対象コードの実行には公式スキルの隔離条件が適用される。
監査結果の保存先は原則リポジトリ外。クラウド環境では、たとえば
`/workspace/scratch/security-audits/chokkin/run-1` のような新しい書込可能な
ディレクトリを依頼時に指定できる。

## 検証・更新

同梱の公式テストはリポジトリルートで実行できる。

```sh
node --test .claude/skills/security-audit/validate-findings.test.cjs .claude/skills/security-audit/validate-coverage-ledger.test.cjs
```

導入時は Node.js v24.19.0 で公式テスト65件すべて成功。
Codex CLI 0.159.0-alpha.3 の `skills/list` でも、`security-audit` が
`scope: repo`、`enabled: true`、同名の重複なしで認識されることを確認した。
自動呼出しの無効化後は、通常の依頼のモデル向けプロンプトから
`security-audit` が除外され、スキル一覧では有効なまま残ることも確認した。
Claude Code 本体はこの環境にないため、公式の配置形式と参照先までを確認。

更新時は配布元の新コミットを取得し、差分を確認してから
`skills/security-audit/` の全ファイルと `LICENSE` を置き換える。
`SKILL.md` の `disable-model-invocation: true` と `agents/openai.yaml` の
`policy.allow_implicit_invocation: false` は更新後も維持する。
削除された公式ファイルも反映し、このファイルのコミットと日付を更新する。
リンク先が共通なので両 CLI に同じ更新が適用される。
