---
name: wrapup
description: タスク完了時に simplify と update-docs を順に実行し、最後に /compact のリマインドを出す。Stop hook の block reason から起動されるか、user が「タスク完了」「/wrapup」と言ったときに発動する。
---

# wrapup

yokei の編集タスク後に手動で行っていた `/simplify` → `/update-docs` → `/compact`
の連鎖を自動化するスキル。Stop hook (`.claude/hooks/stop-wrapup.sh`) が未実行を
検知して `decision:"block"` で起動を促すか、user が「タスク完了」「/wrapup」と
言ったときに発動する。

## 実行手順

1. **simplify** を Skill ツールで呼び出し、変更したコードを再利用性・品質・効率の
   観点でレビュー／リファクタする。
2. **update-docs** を Skill ツールで呼び出し、`src/` の変更を `README.md` /
   `README.ja.md` / `docs/dev/spec.ja.md` / `CLAUDE.md` / `AGENTS.md` に同期する。
3. ファイルを変更した場合は `make check` を実行し、fmt-check → clippy → test →
   doc → cargo-deny が通ることを確認する。
4. **完了マーカーを必ず touch する。** Stop hook の block reason に埋め込まれた
   絶対パス (例 `/tmp/yokei-wrapup-<session_id>`) を `touch` する。reason から
   取得できない場合は `/tmp/yokei-wrapup-default` をフォールバックに使う。
5. user に簡潔にリマインドする: 「wrapup 完了。`/compact` でコンテキストを圧縮して
   ください。」

## 注意

- 手順 4 は **常に実行する**。省くと次回 Stop hook で無限ループに陥る。
- simplify / update-docs が「変更なし」と報告しても、手順 4 と 5 は実行する。
- git-check hook が同時に commit を要求している場合は、simplify / update-docs が
  ドキュメントを変更しうるため、先に wrapup を終えてから commit & push する。
- yokei は Python コードを実行しない静的解析ツール。simplify / update-docs が
  解析対象プロジェクトのコードを実行することは絶対にない。
