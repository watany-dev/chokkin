---
name: handle
description: >
  指定した issue を修正し、実機 (ビルドしたバイナリ + OSS corpus) で確かめ、
  /wrapup を通してから、それらの issue を閉じる PR を出す。PR を出した後は
  15 分ごとに issue と PR を確認して CI を緑に保ち、残った課題は issue に起票する。
  不明な点があれば作業を止めて user に質問する。user が「/handle #123 #456」の
  ように issue 番号を渡したときに発動する。
argument-hint: "<issue番号> [<issue番号>...]"
---

# handle

引数で受け取った issue (`#123 456` のどちらの書き方でもよい) をすべて片付ける。
issue が複数あっても PR は 1 本にする。手順は次の 7 段階。

## 不明点があれば止める (全段階で有効)

次のどれかに当たったら、その場で作業を止めて `AskUserQuestion` で質問する。
推測で進めない。質問には推奨する回答と理由を添え、1 度に 1 つずつ聞く。

- issue が期待する挙動を一意に決められない (既定挙動を変えるのか、
  `--strict` だけを変えるのか、新しい config を足すのか、など)
- 直し方が複数あり、どれを選ぶかで出力・既定値・公開 surface (CLI フラグ /
  config キー / JSON schema / rule code) が変わる
- `docs/dev/spec.ja.md` または ADR と矛盾する
- 指定された issue どうしが衝突している、または 1 本の PR にまとめるには
  大きすぎる
- 再現できない、または issue がもう直っている (閉じるかどうかは user が決める)
- OSS corpus の gate が落ち、それが仕様どおりの変化なのか回帰なのか判断できない

コードを読めば答えが出る質問はしない。自分で調べて解決する。

## 1. issue を読む

- 各 issue について本文・コメント・ラベル・リンク先の issue / PR を読む
  (GitHub MCP の `issue_read`)
- 触る解析ロジックに対応する `docs/dev/spec.ja.md` の節と、関係する
  `docs/adr/` を読む
- issue ごとに「何が起きているか / 期待する挙動 / 受け入れ条件」を 1〜3 行で
  まとめる。ここで決められなければ質問する

## 2. 修正する (issue ごと)

1. **再現**: 失敗するテストを先に書く。CLI の挙動なら `tests/cli_analyze.rs`
   など既存の統合テストに合わせ、fixture は `tests/fixtures/` に置く。テストが
   落ちることを確認してから直す
2. **修正**: AGENTS.md のルールを守る。ロジックは `src/lib.rs` 配下に置き、
   `std::path` を使い、Python コードは実行しない
3. 仕様が変わる場合は `docs/dev/spec.ja.md` を同じ PR で更新する
4. `make check` を通す。issue ごとに 1 コミット (`fix:` / `feat:` などの prefix、
   本文の末尾に `(#<issue>)`)

## 3. 実機で確かめる

テストが通るだけでは完了にしない。実際のバイナリで確かめる。

- `cargo build --release --locked` でビルドし、issue の再現ケースに対して
  `target/release/chokkin` を走らせる。修正前に出ていた finding が消えている
  (または出るようになっている) ことを出力で確認する
- issue が公開リポジトリを名指ししていれば、それを `target/` 配下に clone して
  走らせる。**clone したプロジェクトのコードは実行しない** (`pip install`、
  `python setup.py`、テスト実行もしない)
- 解析ロジックを変えたときは OSS corpus で回帰を測る。測り方は
  `docs/dev/release-validation.ja.md` に従う:

  ```sh
  scripts/clone-oss-fixtures.sh
  scripts/oss-metrics.py -b <base の chokkin> -o target/handle/metrics/base -r 1 --with-strict
  scripts/oss-metrics.py -b <HEAD の chokkin> -o target/handle/metrics/head -r 3 \
    --baseline target/handle/metrics/base --with-strict --gate
  ```

  base と HEAD は `CARGO_TARGET_DIR` を分けてビルドし、md5 が違うことを
  確認する。`finding-diff.tsv` の NEW / GONE / CHANGED を 1 件ずつ見て、
  issue の意図どおりの変化か仕分ける。意図しない変化は直すか、判断できなければ
  質問する
- ネットワークが無く corpus を clone できないときは、その旨を PR 本文に書き、
  `make oss-fixtures` (リポジトリ内の fixture) で代える

## 4. /wrapup

`wrapup` スキルを実行する (レビュー 2 本 → 有用な指摘の取り込み → コメント掃除 →
`make check`)。見送った指摘とその理由は PR 本文の材料として控えておく。

## 5. PR を出す

- 作業ブランチに push する (指定されたブランチがあればそれを使う。無ければ
  `fix/<issue>-<slug>`。`main` には push しない)
- PR のタイトルは主な変更を表すコミットに合わせる。本文は既存 PR (例: #815) に
  合わせて英語で書き、次の構成にする:

  ```markdown
  Closes #123
  Closes #456

  ## Summary
  issue ごとに何をどう変えたか。

  ## Decisions
  issue を読んでも分からないことを書く。ここがこの PR 本文の主眼:
  - 採用した直し方と、検討して捨てた案、その理由
  - issue に書かれていない挙動をどう決めたか (境界条件、既定値、--strict との関係)
  - 意図して変えなかったこと・スコープ外にしたこと
  - wrapup で見送った指摘と理由
  - user に質問して決まったこと

  ## Real-world check
  実機確認で走らせたコマンドと結果。OSS corpus の gate 結果、
  finding-diff の仕分け (意図どおり消えた / 出た / 残った件数と代表例)。

  ## Test
  追加したテストの名前と、何を固定しているか。`make check` の結果。

  ## Follow-ups
  起票した issue へのリンク (無ければ節ごと消す)。
  ```

  Summary や Test を diff の言い換えで埋めない。読み手が diff と issue から
  分かることは書かず、そこから読み取れない判断を書く。

## 6. 残った課題を起票する

修正の途中で見つけたがスコープ外にしたもの (別の誤検知、corpus で見つかった
未対応パターン、後回しにしたリファクタ) は issue に起票する。起票する前に
`search_issues` で重複を探し、あれば新しく作らずにそちらへコメントする。
issue 本文には、再現方法・見つけた経緯 (どの PR のどの作業か)・考えられる
直し方を書き、PR の Follow-ups 節からリンクする。

## 7. PR を追い続ける (15 分ごと)

PR を出したら `subscribe_pr_activity` で PR を購読し、加えて 15 分ごとに
自分で確認する。Claude Code on the web なら `send_later` (`delay_minutes: 15`)
を、ローカルの CLI なら `/loop 15m` を使う。毎回次を確認する:

- **CI**: 最新 head の check run。赤ければ原因を突き止めて直し、`make check` を
  通して push する。flake で片付けない。base ブランチでも赤い失敗は、その旨を
  PR に 1 回コメントする
- **merge conflict**: base ブランチを merge して解消する
- **レビューコメント**: 小さな指摘は直して push し、スレッドに返信する。設計に
  関わる指摘や判断に迷う指摘は user に質問する
- **issue**: 対象 issue に新しいコメントが付いていないか。受け入れ条件が
  変わったら追従し、判断に迷えば質問する
- 新しく見つかった課題は 6. の手順で起票する

何も変わっていなければ黙って次の確認を予約する。PR が merge / close されたら、
または user が止めるよう言ったら確認をやめ、予約済みの確認を取り消す。CI が
緑で、残っているのがレビュー待ちだけになったら、そのことを user に 1 回伝える。

## 注意

- chokkin は静的解析ツール。検証のためでも、解析対象プロジェクトのコードは
  絶対に実行しない (`import`、`exec`、`setup.py`、テスト実行のどれも)
- テストを skip / 無効化して CI を緑にしない。空コミットで CI を再実行しない
- 他人のブランチの履歴を書き換えない (rebase / amend / force-push をしない)
