---
name: handle
description: >
  指定した issue を修正し、実機で確かめ、/wrapup を通してから issue を閉じる PR を
  出す。その後 15 分ごとに issue と PR を確認して CI を緑に保ち、残課題は起票する。
  user が「/handle #123 #456」のように issue 番号を渡したときに発動する。
argument-hint: "<issue番号> [<issue番号>...]"
---

# handle

指定された issue をすべて片付ける。PR は 1 本にする。

## 不明点があれば止める

次の場合は作業を止め、`AskUserQuestion` で 1 つずつ質問する。質問には推奨する回答を
添える。コードを読めば分かることは質問しない。

- 期待する挙動が一意に決まらない、または直し方次第で既定値 / CLI / config /
  JSON schema / rule code が変わる
- spec / ADR と矛盾する、issue どうしが衝突する、1 本の PR には大きすぎる
- 再現できない、または既に直っている
- corpus の差分が仕様どおりか回帰か判断できない

## 手順

1. **読む**: issue の本文・コメント・リンク先 (`issue_read`)、関係する
   `docs/dev/spec.ja.md` の節と `docs/adr/` を読み、受け入れ条件を issue ごとに
   1〜3 行でまとめる
2. **直す**: issue ごとに、まず失敗するテストを書く (`tests/` の既存の統合テストと
   `tests/fixtures/` に合わせる)。それから修正し、仕様が変わるなら spec も更新する。
   `make check` を通して 1 コミットにする (本文の末尾に `(#<issue>)`)
3. **実機で確かめる**: `cargo build --release --locked` でビルドしたバイナリを
   再現ケースに走らせ、finding が変わったことを出力で確かめる。解析ロジックを
   変えたら、`docs/dev/release-validation.ja.md` の手順で OSS corpus を測る
   (`scripts/oss-metrics.py --baseline ... --with-strict --gate`)。
   `finding-diff.tsv` の差分を、意図どおりの変化かどうか 1 件ずつ仕分ける。
   ネットワークが無いときは `make oss-fixtures` で代え、その旨を PR に書く
4. **/wrapup** を実行する。見送った指摘とその理由を控えておく
5. **PR を出す**: 作業ブランチに push する (指定が無ければ `fix/<issue>-<slug>`)。
   本文は既存 PR (#815) に合わせて英語で書く:
   - `Closes #N` (issue ごとに 1 行)
   - **Summary**: issue ごとに何を変えたか
   - **Decisions** (この本文の主眼): issue を読んでも分からない判断を書く。
     捨てた案とその理由、issue に書かれていない挙動の決め方、意図して変えなかった
     こと、wrapup で見送った指摘、質問して決まったこと
   - **Real-world check**: 走らせたコマンド、gate の結果、差分の仕分け
   - **Test**: 追加したテストが固定していること
   - **Follow-ups**: 起票した issue へのリンク

   diff を見れば分かることは書かない
6. **残課題を起票する**: スコープ外にしたものは `search_issues` で重複を確かめて
   から起票する。重複があればそちらにコメントする。起票した issue には再現方法と
   見つけた経緯を書く
7. **追い続ける**: `subscribe_pr_activity` で PR を購読し、15 分ごとに確認する
   (web 版では `send_later` の `delay_minutes: 15`、ローカルの CLI では `/loop 15m`)。
   確認すること:
   - CI が赤ければ原因を直して push する
   - conflict があれば base を merge する
   - 小さなレビュー指摘は直して返信する
   - 対象 issue の新しいコメントに追従する

   設計に関わる指摘は質問する。何も変わっていなければ黙って次の確認を予約する。
   CI が緑でレビュー待ちだけになったら、user に 1 回伝える。merge / close されたら、
   または止めるよう言われたら確認をやめる

## 禁止

- 解析対象のコードを実行しない (`import` / `exec` / `setup.py` / テスト実行)
- テストを skip しない。空コミットで CI を再実行しない。他人のブランチを force-push しない
