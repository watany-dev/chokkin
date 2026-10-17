# #1031: setup.py の dev entry 判定の検証

2026-10-17。`docs/dev/release-validation.ja.md` に従い、baseline
`b9d703e` と本 PR を別 worktree / target directory で release build した。
解析対象の Python コードは実行していない。

## バイナリと再現例

作業 directory は `target/issue-1031`。

| binary | version | md5 |
| --- | --- | --- |
| `target-base/release/chokkin` | 0.7.7 | `928aa72b4497fca5b9073fe7610ac53e` |
| `target-head/release/chokkin` | 0.7.7 | `49d03f938920f1107e87c8c2a3c73d18` |

#1031 の `pyproject.toml`、`setup.py`、`main.py` の 3-file 再現例を
両バイナリで `--no-cache --reporter json` により解析した。

| 対象 | baseline | 本 PR |
| --- | --- | --- |
| root `setup.py` | CHK001 error / certain | finding なし |

この再現例の `setup()` に対する既存の non-static manifest warning は残る。
entry の認識では `setup()` の引数を評価しない。

## 回帰テストとレビュー

`make check` は PASS。fmt、rustdoc、clippy、全テスト
（1,941 passed / 2 ignored）、cargo-deny、cargo-machete を通した。

- unknown / flat / src layout の root `setup.py` は dev context になり、
  そこから import される helper とともに CHK001 の対象から外れる。
- `setup.py` だけの従来形式も、既定・`--strict` の両方で entry になる。
- workspace member の `setup.py` は、その member の build requires を使う。
  `examples/` 配下の member でも dev context を保つ。
- `--production` は build script を外す。package 内の同名 `setup.py` と
  未使用の control module は引き続き CHK001 になる。
- backend または build requires を明示した場合は、legacy の setuptools を
  補完せず、未宣言の import を strict の CHK003 として残す。
- 既存の文字コードエラーを含む `setup.py` の回帰テストも PASS。

CI の mutation test は legacy 補完条件の `&&` を `||` にしても検出しない
2 件を見つけたため、上の明示した build 環境の境界を回帰テストに追加した。
解析ロジックには変更がなく、release バイナリと corpus 比較は上記のまま。

全体テストで見つかった既存の gitignore fixture の誤りは別コミットで修正した。
不正バイトより前の有効な rule は `ignore` が保持するため、全く読めない
fixture では不正バイトを先頭に置く。解析ロジックの変更はない。

wrapup の正しさ・過剰設計レビューに未対応の指摘はない。追加コメントは
root の走査と legacy setuptools の既定値を必要とする理由を説明するものだけを残した。

## OSS corpus

35 OSS project と 10 recall fixture の同じ checkout を両バイナリで解析した。
`target/oss-clones/clones.lock.tsv` の SHA-256 は
`d04ca0fbab5d17e35dcc98abf4ef23d9deab3fd93a97bd04f6cf89e90ed7bef0`。

baseline は各 project 1 回、本 PR は 3 回の cold run の中央値を使い、
両方で `--with-strict` を付けた。本 PR の `--gate` は exit 0。

| 検証 | 結果 |
| --- | --- |
| finding 差分（既定 / strict） | 両方 NEW 0 / GONE 0 / CHANGED 0 |
| 消失した tp / 降格 | 0 / 0 |
| recall | 879 / 879 |
| CHK002 FP 率 | 3.7%（1 / 27、未分類 0） |
| rule 別 precision floor | すべて PASS |
| crash / CLI・config error | 0 / 0 |
| expectations | PASS |
| medium project の最大 cold run | baseline 431 ms / 本 PR 136 ms（両方 2,000 ms 以内） |

実行中の CPU 負荷が違うため、上の時間差を速度改善とはみなさない。
rule 別件数はすべて不変。CHK001 は reported 33 / info 5、label 付き hit の
precision は 39.4% のまま。本 PR による誤検知の解消は上の再現例で確認しており、
この corpus の集計上の precision が上がったという結果ではない。

初回計測では、新しく読んだ legacy `setup.py` の `setuptools` import が strict の
CHK003 に 6 件増えた（click、djangorestframework、python-dotenv、requests 2 件、
transformers）。従来形式の frontend が用意する setuptools を build 環境に含めて
解消した。最終計測の `finding-diff.tsv` と `finding-diff-strict.tsv` は header のみ。
finding が変わらないため corpus label / expectation の変更はない。

## 再計測コマンド

```sh
S="$PWD/target/issue-1031"
git worktree add --detach "$S/base-worktree" b9d703e
(cd "$S/base-worktree" && CARGO_TARGET_DIR="$S/target-base" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR="$S/target-head" cargo build --release --locked --bin chokkin
scripts/clone-oss-fixtures.sh
scripts/oss-metrics.py -b "$S/target-base/release/chokkin" -o "$S/metrics/base" -r 1 --with-strict
scripts/oss-metrics.py -b "$S/target-head/release/chokkin" -o "$S/metrics/head" -r 3 --baseline "$S/metrics/base" --with-strict --gate
```

raw JSON / stderr、`report.md`、`compare.md` と両 diff は
`target/issue-1031/metrics/{base,head}` に保存した。
