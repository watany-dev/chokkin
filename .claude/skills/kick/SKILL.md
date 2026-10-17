---
name: kick
description: >
  指定した issue を修正し、実機で確かめ、wrapup を通してから issue を閉じる PR を
  出し、merge まで CI を緑に保つ。user が「$kick #123 #456」（Codex）または
  「/kick #123 #456」（Claude Code）のように issue 番号を渡したときに発動する。
argument-hint: "<issue番号> [<issue番号>...]"
---

# kick

指定された issue をすべて片付ける。PR は 1 本。issue は作らない。

## 実行環境と依存

- リポジトリルートで作業し、`AGENTS.md` を守る。issue 番号は user のメッセージ
  から読む。番号が無ければ確認し、推測で選ばない。
- GitHub 操作には認証済みの `gh` または利用可能な GitHub ツールを使う。
  ビルド・検証には `git`、`make`、Rust toolchain、`cargo-deny`、`cargo-machete`、
  `python3` が必要。`make tools` の定義と既存の開発手順に従う。
- `wrapup` は [../wrapup/SKILL.md](../wrapup/SKILL.md) を読み、その場で手順を
  実行する。Codex では Claude Code の slash command をツールとして呼ばない。
  仕様・文書の同期が必要なら [../update-docs/SKILL.md](../update-docs/SKILL.md)
  も適用する。
- 依存スキルにもこのタスクの範囲を引き継ぐ。新規 issue は手順 6 の例外だけ。
  `cleanup-comments` が見つけた TODO も同じ方針で扱う。

不明点があれば作業を止めて user に質問する。推奨する回答を添える。コードを
読めば分かることは質問しない。

## 手順

1. **読む**: issue と関係する `docs/dev/spec.ja.md` の節・ADR を読み、受け入れ
   条件を issue ごとにまとめる
2. **直す**: issue ごとに失敗するテストを書いてから修正し、1 コミットにする。
   仕様が変わるなら spec も更新する。認識する形を足すときは、認識できない入力が
   どうなるかも決める（ADR 0005）。`make check` を通す
3. **実機で確かめる**: release ビルドを再現ケースに走らせる。解析ロジックを変えた
   ら `docs/dev/release-validation.ja.md` の手順で OSS corpus を測り、差分を
   1 件ずつ仕分ける。隣の不具合や読めない形が見えたら、軽いものはその場で直し
   （別コミット）、3 をやり直す
4. **wrapup** の手順を実行する。修正を取り込んだら 3 をやり直す
5. **PR を出す**: 本文は英語。対象の issue を閉じる。中心は issue から読み取れない
   判断・意図・捨てた案。実機の結果も載せる。diff を見れば分かることは書かない。
   最後に「Found, not fixed」の節を置き、途中で見つけたことを 1 件 1 行で書く
6. **起票しない**: 重いものは 5 の節に書いて終わり。user が必要なものだけ issue に
   する。例外は、再現手順のある certain の誤検出かクラッシュで、open issue・
   roadmap・epic（#888 など）のどれにも入らないもの。PR 1 本につき 1 件まで。
   本文に recall（形の認識）か gap（読めないことが finding を下げない）かを書く
7. **追い続ける**: 15 分ごとに PR を確認し CI を緑に保つ。レビューに追従する。
   merge / close されたらやめる。利用可能な継続実行・スケジュール機能を使い、
   終了時には監視を止める。継続実行できない環境では、現在の CI・レビュー状態を
   確認し、PR URL と未完了事項、監視を継続できないことを明示して引き継ぐ。
   監視を設定していないのに「15 分ごとに確認する」と約束しない。

## 軽い・重い

軽い: 触っている module の中で直せて、spec を変えず、テスト込みで数十行。見つけた
順に直し、やり直しは合計 3 周まで。重い: それ以外、または 3 周を超えて残ったもの。
