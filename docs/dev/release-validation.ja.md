# Release validation 手順

リリースごとの実測は `v<version>-release-validation.md` に固定する。このページは
その測り方の共通手順で、過去の検証で踏んだ落とし穴を避けるための手順を含む。
判定基準は spec §17 の exit criteria と corpus regression gate (#495)、測定は
`scripts/oss-metrics.py` で行う (`oss-metrics.yml` と同じ)。

## 1. バージョンごとにビルドを分ける

baseline (前回の tag、例は v0.7.3) と HEAD を、worktree も `CARGO_TARGET_DIR` も
バージョンごとに分けてビルドする。

```sh
S=target/release-validation
git worktree add "$S/wt-0.7.3" v0.7.3
(cd "$S/wt-0.7.3" && CARGO_TARGET_DIR="$PWD/../target-073" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR="$S/target-head" cargo build --release --locked --bin chokkin

for b in "$S"/target-*/release/chokkin; do
  echo "$b $("$b" --version) $(md5sum <"$b" | cut -d' ' -f1)"
done
```

最後のループ (macOS では `md5sum` を `md5 -q` に置き換える) の出力 (path、`--version`、md5) をレポートに残し、**md5 が
バージョン間で一致しないこと**を確認してから測定に進む。

理由: package version が同じ worktree (例: v0.7.3 tag と、bump 前の main。どちらも
`0.7.3`) を同じ `CARGO_TARGET_DIR` でビルドすると、cargo が lib の rlib を
fingerprint 一致とみなして再利用し、`src/` が違っていても再コンパイルしない。
v0.7.3 の検証では HEAD のバイナリが v0.7.3 と同一 (md5 一致) になり、HEAD の初回
計測が v0.7.3 と完全一致したことで気付いた。気付かずに測ると、別バージョンの
コードで「差分なし」と結論してしまう。`--version` は package version しか出さない
ので、この取り違えは md5 でしか検出できない。

## 2. corpus を固定して測る

```sh
scripts/clone-oss-fixtures.sh
BASE="$S/target-073/release/chokkin"
HEAD="$S/target-head/release/chokkin"

scripts/oss-metrics.py -b "$BASE" -o "$S/metrics/base" -r 1
scripts/oss-metrics.py -b "$HEAD" -o "$S/metrics/head" -r 3 --baseline "$S/metrics/base" --gate
```

clone は `target/oss-clones/clones.lock.tsv` で固定され、両バージョンで同じ
checkout を測る。`--baseline` を付けた run は次を書く。

- `compare.md`: CHK002 / CHK003 の before/after、finding-level diff の集計
  (rule 別、project 別の NEW / GONE / CHANGED)、§17 scorecard
- `finding-diff.tsv`: 動いた finding の一覧。列は
  `status slug code fingerprint before after`、`before` / `after` は
  `severity/confidence`

`finding-diff.tsv` は (slug, fingerprint) を key にした diff で、status の意味は次のとおり。

| Status | 意味 |
| --- | --- |
| NEW | HEAD にだけある (新規検出) |
| GONE | baseline にだけある (消失、または既定の confidence filter で隠れる `maybe` への降格) |
| CHANGED | 両方にあり、severity か confidence が動いた (例: `warning/likely` → `info/likely`) |

fingerprint は `CODE:stable-target` (行番号を含まない) なので、行がずれただけの
finding は動いたことにならない。同じ fingerprint を持つ finding が複数ある場合
(同じファイルで同じ module を何度も import する CHK003 など) は、level が変わらない
ものを相殺し、残りを CHANGED、余りを NEW / GONE として数える。件数の増減をレポートに書くときは、rule 別件数の
表に加えて `finding-diff.tsv` から project ごとの内訳を引く。

```sh
# fastmcp の NEW を rule 別に
awk -F'\t' '$1=="NEW" && $2=="fastmcp" {print $3}' "$S/metrics/head/finding-diff.tsv" | sort | uniq -c
# examples/ 配下の NEW
awk -F'\t' '$1=="NEW" && $4 ~ /examples\//' "$S/metrics/head/finding-diff.tsv"
```

## 3. `--strict` で降格と消失を分ける

library mode の自動判定 (#652/#664) のように finding を info や `maybe` に落とす変更が
入ると、既定の filter の件数だけでは「降格」と「消失」を区別できない (#693 P-B)。
HEAD を `--strict` でも測り、baseline の既定 run と比べる。

```sh
scripts/oss-metrics.py -b "$HEAD" -o "$S/metrics/head-strict" -r 1 --strict --baseline "$S/metrics/base"
```

`--strict` は confidence の表示 floor を `maybe` まで下げるので、`maybe` に降格した
finding も出力に残る (info はもともと JSON に出る)。この run の `finding-diff.tsv`
の GONE は降格では説明できない、本当に消えた finding になる。§2 の GONE のうち、
ここで GONE に残らないものは降格。レポートには既定 run と strict run の GONE 件数を
並べて書く。

この run で見るのは GONE だけにする。strict は dev / test context の CHK003 や
marker 付き依存の CHK002 を新たに報告し、一部の severity も上げる (marker 付き依存の
CHK002 が warning → error など) ので、NEW と CHANGED、`compare.md` の CHK002 /
CHK003 の表には strict 自体による差が混ざる。NEW / CHANGED は §2 の run で見る。
gate の基準 (expectations、CHK003 growth) も既定の filter 前提なので `--gate` は
付けず、出力先も既定 run と分ける。

## 4. レポートに残すもの

`docs/dev/v<version>-release-validation.md` に次を固定する (`target/` は生成物)。

- §1 のバイナリ一覧 (path、`--version`、md5)
- `report.md` の exit criteria と per-rule precision
- rule 別件数の baseline → HEAD と、`compare.md` の finding-level diff の集計
- 件数が動いた project ごとの内訳と原因の PR (`finding-diff.tsv` から)
- §3 の strict run の GONE 件数
- 実行時間 (環境ノイズが大きいので baseline と HEAD を交互に複数回)
- 実行したコマンド
