# #912: task command の first-party entry の検証

2026-10-18。`release-validation.ja.md` に従い、baseline `34915ef` と
本 PR を別 worktree / target directory で release build した。
解析対象の Python コードは実行していない。作業 directory は `target/issue-912`。

## バイナリと再現例

| binary | version | md5 |
| --- | --- | --- |
| `target-base/release/chokkin` | 0.7.7 | `ef69e31506b58afbe8cd80f42423cf7e` |
| `target-head/release/chokkin` | 0.7.7 | `19f12ed521e24ee67a3663668237cd1d` |

#912 の 5 command に、command からも import からも届かない control file
`mypkg/orphan.py` を加えた。同じ project を app / library の両 mode で解析した。
baseline は #913 の gap があるため、元の issue の certain ではなく likely になる。

| 対象 | baseline | 本 PR |
| --- | --- | --- |
| Makefile の `python mypkg/gen.py` | CHK001 error / likely | finding なし |
| Makefile の `python -m mypkg.modrun` | CHK001 error / likely | finding なし |
| tox の `python mypkg/toxgen.py` | CHK001 error / likely | finding なし |
| local pre-commit の `python -m mypkg.ghgen` | CHK001 error / likely | finding なし |
| Procfile の `gunicorn mypkg.server:app` | CHK001 error / likely | finding なし |
| `mypkg/orphan.py` | CHK001 error / likely | CHK001 error / certain |
| command warning | 4 | 0 |
| library mode | CHK008 3 件、command warning 4 件 | CHK008 3 件のみ |

app mode の exit 1 は未使用の control file によるもの。library mode は exit 0。
`--trace mypkg/orphan.py` の entry 一覧にも Makefile:2 / Makefile:4、tox.ini:2、
`.pre-commit-config.yaml:6`、Procfile:1 が残る。`repro-results.json` と
`repro-{base,head}-{app,library}.json` / stderr に保存した。

## 判定と回帰テスト

- standalone の task script は dev。package module と Procfile / Dockerfile
  の deploy target は元の context を保つ。task と deploy が同じ file を指せば
  runtime を優先し、`--production` で落とす dev entry は gap にしない。
- `python -m pkg` は `pkg.__main__`、`pkg` の順で解決する。server は named module
  自身を import するので、package の `__main__` に置き換えない。
- server の属性と factory 名を entry symbol に残す。省略時の gunicorn の
  `application`、Celery の `app` / `celery` も残し、file の CHK001 が消えた代わりに
  app 属性の CHK006 が出ることを防ぐ。
- computed target と heuristic な `python -c` の import は従来の gap に残す。
  typo / third-party target は entry を増やさない。config scan cache は v7 にする。

修正前に 5-target regression が失敗することを確認した。最終 `make check` は
fmt、rustdoc、clippy、全テスト（1,960 passed / 2 ignored）、cargo-deny、machete が PASS。
全 task-file scanner、5 server、推移的な import、production、package execution、
factory と暗黙の app 属性、origin の行、computed / 未解決 target をテストした。

wrapup の正しさレビューでは production の gap、module execution と server import の
区別、factory / 暗黙の app 属性を取り込んだ。過剰設計レビューに削減案はなく、
コメントは context / unknown-input の理由を説明するものを残した。

## OSS corpus

35 OSS project と 10 recall fixture を同じ checkout で比較した。
`target/oss-clones/clones.lock.tsv` の SHA-256 は
`d04ca0fbab5d17e35dcc98abf4ef23d9deab3fd93a97bd04f6cf89e90ed7bef0`。
baseline は 1 回、HEAD は 3 回の cold run とし、両方で `--with-strict` を使った。
wrapup の修正後にも release build と計測をやり直した。最終 gate は exit 0。

| 検証 | 結果 |
| --- | --- |
| 既定 / strict の finding 差分 | 両方 NEW 0 / GONE 0 / CHANGED 0 |
| 消失した tp / 降格 / 文脈変更 | 0 / 0 / 0 |
| recall | 879 / 879 |
| CHK002 FP 率 | 3.7%（1 / 27、未分類 0） |
| rule 別 precision floor / expectations | すべて PASS |
| crash / CLI・config error | 0 / 0 |
| in-repo determinism / JSON schema | 203 / 203 PASS（609 JSON outputs） |
| in-repo crash / 期待する config rejection | 0 / 18 件すべて維持 |

両 finding diff は header のみで、個別に仕分ける変化はない。label / expectation
も変更していない。rule 別件数も不変で、下の reported は info を除く。

| Rule | reported（base = HEAD） | info（base = HEAD） |
| --- | ---: | ---: |
| CHK001 | 33 | 5 |
| CHK002 | 27 | 0 |
| CHK003 | 120 | 140 |
| CHK004 | 1903 | 0 |
| CHK005 | 28 | 54 |
| CHK006 | 207 | 2347 |
| CHK007 | 0 | 0 |
| CHK008 | 18 | 0 |
| CHK009 | 0 | 5 |
| CHK010 | 202 | 525 |

CHK001 の labelled hit の precision は 39.4% のまま。この corpus の集計上の
precision 改善ではなく、冒頭の再現例で誤検出の解消を確認した変更である。

追加で 35 OSS project を baseline / HEAD 交互に 3 組ずつ `--no-cache` で測った。
各 project の中央値と全 sample は `timings.json` に保存した。

| 時間 | baseline | HEAD |
| --- | ---: | ---: |
| project 別中央値の合計 | 9,505.31 ms | 8,739.22 ms |
| medium project の最大中央値 | 92.38 ms | 102.60 ms |

medium project はいずれも 2,000 ms の gate 内。時間差を速度改善とは断言しない。

## 再計測コマンド

```sh
VALIDATION_912_DIR="$PWD/target/issue-912"
git worktree add --detach "$VALIDATION_912_DIR/base" 34915ef
(cd "$VALIDATION_912_DIR/base" && CARGO_TARGET_DIR="$VALIDATION_912_DIR/target-base" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR="$VALIDATION_912_DIR/target-head" cargo build --release --locked --bin chokkin
make check
scripts/clone-oss-fixtures.sh
python3 "$VALIDATION_912_DIR/reproduce.py"
scripts/oss-metrics.py -b "$VALIDATION_912_DIR/target-base/release/chokkin" -o "$VALIDATION_912_DIR/metrics/base" -r 1 --with-strict
scripts/oss-metrics.py -b "$VALIDATION_912_DIR/target-head/release/chokkin" -o "$VALIDATION_912_DIR/metrics/head" -r 3 --baseline "$VALIDATION_912_DIR/metrics/base" --with-strict --gate
.venv/bin/python scripts/oss-gate.py --in-repo -b "$VALIDATION_912_DIR/target-head/release/chokkin" -o "$VALIDATION_912_DIR/oss-gate"
python3 "$VALIDATION_912_DIR/timing.py"
```

raw JSON / stderr / 両 diff / scorecard は `metrics/{base,head}`、fixture gate は
`oss-gate` に保存した。再現・交互計測のローカル helper も上記の作業 directory に残した。

## Found, not fixed

- `cd work && python -m pkg.task` と `uv run --directory` の module 探索場所は
  追わない（recall / gap、#888）。baseline と HEAD の両方で同じ CHK001 certain を
  確認した。module target の directory と workspace layout の対応付けが必要なので、
  この修正では拡張しない。
- workflow / skill doc の `python -m` は今回の task-file 拡張に含めない。
- open PR #1038 は同じ server command reader の抽出と option 解析を拡充する。
  後から merge する branch で差分を整合させる必要がある。
