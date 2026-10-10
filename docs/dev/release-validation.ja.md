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

scripts/oss-metrics.py -b "$BASE" -o "$S/metrics/base" -r 1 --with-strict
scripts/oss-metrics.py -b "$HEAD" -o "$S/metrics/head" -r 3 --baseline "$S/metrics/base" --with-strict --gate
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
§2 の `--with-strict` は、各 project を既定の run に加えて `--strict` でも 1 回測り、
JSON を `strict/` に置く。`--strict` は confidence の表示 floor を `maybe` まで下げるので、
`maybe` に降格した finding も残る (info はもともと JSON に出る)。

`--strict` は表示 floor だけでなく判定も変える。たとえば member の dev 宣言があると、
runtime import は CHK003 ではなく CHK005 になる。このため strict の出力は baseline の
strict 出力とだけ比べる (#716)。既定 run の baseline と比べると、CHK005 に変わった
だけの finding が GONE に混ざる (v0.7.3 → main の langchain CHK003 15 件はすべてこれ)。
`--strict` の run に既定 run の `--baseline` を渡すと (逆も)、`oss-metrics.py` は
exit 2 で止まる。

`compare.md` の「Downgrade vs vanish」表は、rule ごとに次を並べる。

| 列 | 意味 |
| --- | --- |
| GONE | 既定 run の GONE (`finding-diff.tsv`) |
| Downgraded | そのうち HEAD の strict 出力に同じ fingerprint が残るもの (降格) |
| Vanished tp / fp / other | strict 同士の GONE (`finding-diff-strict.tsv`、消失) を label 別に数えたもの |

gate (`--gate`) が落ちるのは `tp` が消えたときだけ。ラベル無し (other) の消失は件数を
出すだけにしている。FP を直すリリースでは消失が数百件出るからで、v0.7.3 → main では
CHK010 215 件、CHK003 98 件 (airflow)、CHK007 50 件 (fastmcp) が other だった。
other は `finding-diff-strict.tsv` から project ごとに引いて、原因の PR を確かめる。

```sh
awk -F'\t' '$1=="GONE" {print $3, $2}' "$S/metrics/head/finding-diff-strict.tsv" | sort | uniq -c | sort -rn
```

confidence を落とす変更でも、件数減のすべてが降格とは限らない。library mode の public
symbol には CHK006 / CHK007 自体を出さない (spec §12) ので、`--strict` にも残らない。
v0.7.2 → v0.7.3 では CHK006 1,578 件と CHK007 66 件がすべて other の消失になった。
CHK001 も、1,329 件の GONE のうち Downgraded は 270 件だけだった (django の消失 744 件の
うち 742 件は `tests/` 配下)。

`--with-strict` で増える時間は、corpus 一周の `-r 1` の 1 回分になる。

## 4. レポートに残すもの

`docs/dev/v<version>-release-validation.md` に次を固定する (`target/` は生成物)。

- §1 のバイナリ一覧 (path、`--version`、md5)
- `report.md` の exit criteria と per-rule precision
- rule 別件数の baseline → HEAD と、`compare.md` の finding-level diff の集計
- 件数が動いた project ごとの内訳と原因の PR (`finding-diff.tsv` から)
- §3 の「Downgrade vs vanish」表と、other の消失の原因
- 実行時間 (環境ノイズが大きいので baseline と HEAD を交互に複数回)
- 実行したコマンド
