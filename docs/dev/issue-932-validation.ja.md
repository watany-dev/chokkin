# #932 plugin entry の source discovery 検証

2026-10-18、Linux x86_64。baseline は `d2c30ac`、比較対象はこの PR の実装。
`docs/dev/release-validation.ja.md` に沿い、別 worktree・別 `CARGO_TARGET_DIR` で
release binary をビルドした。対象 project の Python code は実行していない。

## Binary と corpus

| 対象 | path | version | MD5 |
|---|---|---|---|
| base | `target/issue-932/target-base/release/chokkin` | 0.7.7 | `49d03f938920f1107e87c8c2a3c73d18` |
| head | `target/issue-932/target-head/release/chokkin` | 0.7.7 | `41c9c97508b056117dac5087c81aa991` |

MD5 は異なる。35 OSS project は `scripts/oss-clones.manifest` の SHA に固定し、
checkout は `target/oss-clones/clones.lock.tsv` に記録した。これに 10 recall sentinel を
加えた計 45 project を、default と strict で比較した。baseline は 1 回、HEAD は 3 回の
cold run。時間は別に両 binary を交互に 3 回ずつ測った。

## 実機の再現

`target/issue-932/repro/` の src layout の `docs/conf.py` と flat layout の `manage.py`
に `import requests` と helper の import を置いた。baseline は両方を skip し、helper と
requests を unused / likely にした。HEAD は entry を読み、両 finding が消え、無関係な
unused `rich` と orphan は certain に戻った。Django settings は読める形を置いた。
両 binary の JSON は `repro-{sphinx,django}-{base,head}.json` に残した。

## Gate

baseline / HEAD ともに全 gate を通過した。

| 項目 | HEAD |
|---|---|
| CHK002 FP rate | 3.7% (1 FP / 27 hits、unclassified 0) |
| labelled TP recall | 879 / 879 |
| crash / CLI・config error | 0 / 0 |
| project の到達性 floor・rule growth | すべて通過 |
| precision floor | すべて通過 |
| medium cold run | すべて 2,000 ms 以内 |
| CHK003 非 TP growth | baseline 12 → HEAD 12 |
| strict の TP 消失 | 0 |

Precision の列は `report.md` と同じ。info は hit に含めない。

| Rule | Reported | Info | tp | fp | deferred | unknown | Coverage % | Precision % | Stale |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| CHK002 | 27 | 0 | 26 | 1 | 0 | 0 | 100.0 | 96.3 | 74 |
| CHK003 | 120 | 140 | 82 | 0 | 12 | 26 | 68.3 | 100.0 | 0 |
| CHK004 | 1903 | 0 | 601 | 0 | 0 | 1302 | 31.6 | 100.0 | 0 |
| CHK006 | 207 | 2347 | 81 | 6 | 0 | 120 | 42.0 | 93.1 | 0 |
| CHK010 | 202 | 526 | 108 | 7 | 0 | 87 | 56.9 | 93.9 | 1 |

in-repo OSS gate も 203 / 203 が cold・warm・`--no-cache` で byte-identical、
203 / 203 が schema-valid、crash 0、意図した rejection 18 / 18 だった。

## Finding 差分の全件仕分け

下表は info を含む JSON の全件数。default と strict は同じ mode 同士で比較した。

| Rule | default base → HEAD | strict base → HEAD |
|---|---:|---:|
| CHK001 | 38 → 38 | 1293 → 1293 |
| CHK002 | 27 → 27 | 236 → 236 |
| CHK003 | 260 → 260 | 2636 → 2639 |
| CHK004 | 1903 → 1903 | 3755 → 3755 |
| CHK005 | 82 → 82 | 134 → 134 |
| CHK006 | 2554 → 2554 | 2554 → 2554 |
| CHK007 | 0 → 0 | 0 → 0 |
| CHK008 | 18 → 18 | 18 → 18 |
| CHK009 | 5 → 5 | 5 → 5 |
| CHK010 | 727 → 728 | 727 → 728 |

`docs/conf.py` の skipped-entry warning は次の 13 project で消えた。残る skipped entry
は member の manifest entry など、今回の plugin path entry の対象外である。

anyio、attrs、black、click、django、flask、jinja、pluggy、requests、structlog、
urllib3、virtualenv、werkzeug。

default の差分は virtualenv の CHK010 / info / likely 1 件だけ。
strict の差分は CHK002 の confidence 上昇 19 件と NEW 4 件である。

| Project | strict CHK002 likely → certain | 内訳 |
|---|---:|---|
| click | 1 | pip-compile-multi (unlabelled) |
| jinja | 3 | babel (FP)、pip-compile-multi / sphinx-issues (unlabelled) |
| pluggy | 1 | pygments (unlabelled) |
| requests | 3 | httpbin / pysocks / wheel (FP) |
| urllib3 | 10 | build / cffi / coverage / httpx / hypercorn / quart / quart-trio / towncrier / trio (FP)、requests (unlabelled) |
| werkzeug | 1 | cffi (unlabelled) |

これらはすべて既存 finding で、読めた conf の skip gap が外れた結果。13 件が既存の
FP label、6 件が unlabelled、TP は 0 件。依存使用を新しく見逃した結果ではないが、
既存 FP の根因を直す変更でもない。label を書き換えて隠していない。

NEW の 4 件は次のとおり。

| Mode | Project / finding | 仕分け |
|---|---|---|
| strict のみ | flask / CHK003 `docs/conf.py:packaging.version` | conf の先頭で直接 import。docs の直接宣言が無いという既存の strict policy が新たに見える |
| strict のみ | flask / CHK003 `docs/conf.py:docutils.nodes` | conf の function 内の直接 import。同上 |
| strict のみ | flask / CHK003 `docs/conf.py:docutils.parsers.rst.roles` | 同じ function 内の直接 import。同上 |
| default / strict | virtualenv / CHK010 `docs/conf.py:render_cli` (info / likely) | `docs/render_cli.py` が inventory 外で、`setup()` 内の sys.path 追加も追わない既存の制限。広い docs discovery は #1023 / #1035 の対象 |

全 24 diff 行 (default 1 + strict 23) の個別仕分けは
`target/issue-932/finding-classification.tsv`。GONE、demotion、recontext、info への降格、
strict で消えた TP / FP / other はすべて 0。

## 時間

35 OSS project について、project ごとに base → HEAD、HEAD → base、base → HEAD の
順で 3 回ずつ、他の build / check と重ねずに cold / `--no-cache` を測った。表は ms の中央値。
全 sample は `target/issue-932/timing.tsv`。

| Project | base ms | HEAD ms | 増減 |
|---|---:|---:|---:|
| requests | 14.59 | 17.72 | 21.5% |
| urllib3 | 19.52 | 22.42 | 14.9% |
| flask | 18.33 | 19.25 | 5.0% |
| virtualenv | 21.66 | 24.89 | 14.9% |
| django | 234.07 | 313.85 | 34.1% |
| airflow | 2904.03 | 3017.32 | 3.9% |
| fastmcp | 112.88 | 109.18 | -3.3% |
| llama_index | 1613.03 | 1575.86 | -2.3% |

35 project の中央値の和は 7199.59 → 7467.27 ms。
medium の最大は pydantic の 60.86 ms。環境ノイズがあるため
benchmark gate の代用にはしない。追加 entry があると既存の discovery policy をもう一度
通す費用は残る。review では inventory 内の file だけを選ぶ plugin の二重走査を削り、
prepass を Django / Sphinx / Alembic に限定した。

## Check と review

- `make check`: 1,946 tests pass、2 ignored。fmt、doc、clippy、deny、machete も pass。
- 回帰 test は修正前に失敗することを確認した。src / flat、明示 project、workspace、
  settings package、exclude、gitignore、production、syntax error、cold / warm cache を確認した。
- `.gitignore` が 2 回の walk の間に壊れる回帰 test も加えた。新しい warning を落とす
  mutation がこの test で失敗することを確認した (`target/issue-932/mutation-proof.log`)。
- `/wrapup`: correctness review、`ponytail-review`、`cleanup-comments` を実施した。
  採用した指摘は上記の重複走査の削減。見送った指摘は無し。未完了を示す新しい comment は無し。
- `/update-docs`: spec §8 / §9 を同期した。CLI / exit code / package metadata と
  README の説明に追加の変更は必要なかった。

## 実行コマンド

```sh
git worktree add --detach target/issue-932/wt-base d2c30ac
# baseline worktree で実行
CARGO_TARGET_DIR=/workspace/chokkin/target/issue-932/target-base cargo build --release --locked --bin chokkin
# 元の checkout で実行
CARGO_TARGET_DIR=target/issue-932/target-head cargo build --release --locked --bin chokkin
scripts/clone-oss-fixtures.sh
scripts/oss-metrics.py -b target/issue-932/target-base/release/chokkin -o target/issue-932/metrics/base -r 1 --with-strict
scripts/oss-metrics.py -b target/issue-932/target-head/release/chokkin -o target/issue-932/metrics/head -r 3 --baseline target/issue-932/metrics/base --with-strict --gate
scripts/oss-gate.py --in-repo -b target/issue-932/target-head/release/chokkin -o target/issue-932/oss-gate
make check
```

交互の時間測定も各 binary に `--no-cache --reporter json <clone>` を渡した。
再現 fixture と測定・仕分けの raw artifact は `target/issue-932/` にある。
