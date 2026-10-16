---
name: kick
description: >
  指定した issue を修正し、実機で確かめ、/wrapup を通してから issue を閉じる PR を
  出し、merge まで CI を緑に保つ。user が「/kick #123 #456」のように issue 番号を
  渡したときに発動する。
argument-hint: "<issue番号> [<issue番号>...]"
---

# kick

指定された issue をすべて片付ける。PR は 1 本。issue は作らない。

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
   1 件ずつ仕分ける
4. **/wrapup** を実行する
5. **PR を出す**: 本文は英語。対象の issue を閉じる。中心は issue から読み取れない
   判断・意図・捨てた案。実機の結果も載せる。diff を見れば分かることは書かない。
   最後に「Found, not fixed」の節を置き、途中で見つけたことを 1 件 1 行で書く
6. **起票しない**: 残課題は 5 の節に書いて終わり。user が必要なものだけ issue に
   する。例外は、再現手順のある certain の誤検出かクラッシュで、open issue・
   roadmap・epic（#888 など）のどれにも入らないもの。PR 1 本につき 1 件まで。
   本文に recall（形の認識）か gap（読めないことが finding を下げない）かを書く
7. **追い続ける**: 15 分ごとに PR を確認し CI を緑に保つ。レビューに追従する。
   merge / close されたらやめる
