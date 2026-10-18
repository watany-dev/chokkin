# #1023: examples / docs / notebook discovery の検証

- 日付: 2026-10-18
- baseline: main `d2c30ac`。修正: この report と同じ commit の source。
- 手順: `release-validation.ja.md`。別 worktree・別 `CARGO_TARGET_DIR` で release build。
- Rust: 1.96.0。corpus は `scripts/oss-clones.manifest` の固定 SHA 35 project と recall fixture 10 件。
- 解析対象の Python は実行していない。labels / expectations は変更していない。

## バイナリと再現

| Build | Binary | Version | md5 |
|---|---|---|---|
| baseline | `target/issue-1023/base-build/release/chokkin` | 0.7.7 | `49d03f938920f1107e87c8c2a3c73d18` |
| 修正 | `target/issue-1023/head-build/release/chokkin` | 0.7.7 | `1f2f15f325ada1fe7210e2db3e3215f0` |

issue の再現を flat (`acme/`)・src (`src/acme/`)・package-root (`lib/acme/`) で測った。
`examples/demo.py` だけが `acme.plotting` と matplotlib を import し、
`notebooks/demo.ipynb` だけが seaborn を import する。後二者は dev group に宣言。
`--strict --no-cache` で、三 layout とも baseline は CHK001 1 件・CHK002 2 件 (exit 1)、
修正後は finding・diagnostic とも 0 件 (exit 0)。CHK005 は増えない。

discovery の regression は root・深い notebook、docs の stub、test/docs context の優先、
package 内 notebook の runtime、workspace member、explicit glob・exclude・gitignore・
production を検証する。package が推定できない layout の notebook は dev に倒す。
任意の package 外 directory の `.py` まで一律に広げる変更はしない。

## Gate

| 検証 | 結果 |
|---|---|
| `make check` | PASS: fmt / doc / clippy / 1,945 tests (2 ignored) / deny / machete |
| in-repo OSS gate | PASS: 203/203 cold・warm・no-cache 一致、schema 正常、crash 0、期待する reject 18/18 |
| self-analysis + `chokkin-baseline.json` | exit 0 |
| corpus recall | PASS: 879/879、strict 同士でも labeled tp の消失 0 |
| corpus precision floor | PASS: CHK003 100%、CHK004 100%、CHK006 93.1%、CHK010 94.7% |
| corpus crash / config error | PASS: いずれも 0 |
| corpus medium <= 2 s | PASS: 修正側の最大 183 ms |
| corpus CHK002 FP gate | **FAIL**: 新しく読む Transformers の PEP 723 script に unclassified 123 件 |
| corpus count-growth gate | **FAIL**: Transformers CHK002 0 → 123、CHK003 40 → 103 |

全 corpus の `--gate` は exit 1。discovery を直すと既存の script rule の対象も増える。
123 件は script に宣言され、script の直接 import では使用を確認できない依存で、
transitive/backend 用依存を含む。この検証では tp/fp の ground truth にせず、
既存の FP (`fastmcp` の PEP 723 pillow) と同じ限界も含めて未判定のまま記録する。
gate を通すための label 追加・expectation 引き上げ・discovery の巻き戻しはしない。

## Finding diff

warning 以上 / info の件数:

| Rule | baseline | 修正 |
|---|---|---|
| CHK001 | 33 / 5 | 49 / 5 |
| CHK002 | 27 / 0 | 150 / 0 |
| CHK003 | 120 / 140 | 187 / 140 |
| CHK004 | 1903 / 0 | 1903 / 0 |
| CHK005 | 28 / 54 | 28 / 54 |
| CHK006 | 207 / 2347 | 207 / 2414 |
| CHK007 | 0 / 0 | 0 / 0 |
| CHK008 | 18 / 0 | 18 / 0 |
| CHK009 | 0 / 5 | 0 / 5 |
| CHK010 | 202 / 525 | 203 / 598 |

既定 run は 362 行、strict 同士は 662 行。すべてを
`target/issue-1023/triage.tsv` に 1 finding 1 行で分類した。NEW はすべて追加した
`examples/`・`docs/`・notebook の import / symbol / script 宣言に由来することを確認した。
以下の表には info も含む。全件の severity/confidence は raw JSON / diff TSV に残る。

| Rule | 既定 NEW | GONE | CHANGED | strict NEW | GONE | CHANGED |
|---|---:|---:|---:|---:|---:|---:|
| CHK001 | 16 | 0 | 0 | 16 | 0 | 0 |
| CHK002 | 123 | 0 | 1 | 123 | 16 | 31 |
| CHK003 | 68 | 1 | 0 | 288 | 0 | 0 |
| CHK004 | 0 | 0 | 0 | 35 | 0 | 0 |
| CHK006 | 71 | 4 | 0 | 71 | 4 | 0 |
| CHK010 | 76 | 2 | 0 | 76 | 2 | 0 |

| Project | 既定 NEW / GONE / CHANGED | 仕分け |
|---|---|---|
| click | 2 / 0 / 0 | nested example の `complex` を未解決とする CHK010 (info) |
| flask | 2 / 0 / 0 | README の Celery entry の CHK001、Flask callback の CHK006 (info) |
| llama_index | 0 / 1 / 0 | package 未推定の namespace member の notebook が dev になり CHK003 を再分類。strict に同じ finding が残る |
| mcp-python-sdk | 90 / 0 / 0 | example member の entry 見落とし CHK001 15、public symbol CHK006 (info) 70、PEP 723 CHK003 4、CHK010 (info) 1 |
| mitmproxy | 6 / 0 / 1 | examples の未解決 import CHK010 (info) 6。`requests_mock` 未解決の証拠が CHK002 msgpack を certain → likely にする |
| mlflow | 32 / 1 / 0 | notebook/example の CHK010 (info) 32、追加 inventory により既存 test の `databricks` が解決され CHK010 が消える |
| openai-python | 3 / 0 / 0 | PEP 723 CHK003 (`typing-extensions`) 1、example CHK010 (info) 2 |
| pluggy | 3 / 0 / 0 | docs の nested example の `eggsample` が未解決 (info) |
| sqlalchemy | 3 / 0 / 0 | examples の CHK010 (info) 3 |
| transformers | 195 / 4 / 0 | PEP 723 CHK002 123・CHK003 63、CHK010 (info) 9。既存 symbol 3 の追加参照と test の `run_glue` 解決で 4 件消失 |
| werkzeug | 18 / 1 / 0 | example の CHK010 (info) 18。追加参照で `LocalManager` の CHK006 (info) が消える |

strict の CHK002 GONE 16 は新たに読んだ docs/example が依存を使用するもの。
CHANGED 31 のうち 23 は docs/conf.py や member entry の skip gap が解消したことによる
likely → certain、mitmproxy の 8 は上の未解決 import による certain → likely。
strict で追加される CHK003 / CHK004 も dev/docs の宣言との照合で、runtime import への
変更ではない。71 件の NEW CHK006 はすべて info。

### Downgrade vs vanish

| Rule | 既定 GONE | Recontexted | Demoted | To info | strict vanished tp / fp / other |
|---|---:|---:|---:|---:|---|
| CHK002 | 0 | 0 | 1 | 0 | 0 / 0 / 16 |
| CHK003 | 1 | 1 | 0 | 0 | 0 / 0 / 0 |
| CHK006 | 4 | 0 | 0 | 0 | 0 / 0 / 4 |
| CHK010 | 2 | 0 | 0 | 0 | 0 / 1 / 1 |

## 実行時間

corpus 本測定は baseline 1 回、修正 3 回の cold-run median。
compile の CPU 競合があるため、この値から速度改善は結論しない。
追加の 7 project は各 project で baseline → 修正を 3 回交互に測り、
`target/issue-1023/timing.json` に全 sample を残した。

| Project | baseline median ms | 修正 median ms |
|---|---:|---:|
| requests | 17 | 20 |
| werkzeug | 27 | 30 |
| flask | 23 | 22 |
| pydantic | 75 | 78 |
| mlflow | 469 | 437 |
| llama_index | 1743 | 1998 |
| transformers | 2226 | 1794 |

llama_index の修正側 sample は 1789 / 1998 / 3304 ms、Transformers は
3412 / 1794 / 1603 ms と揺れた。いずれも large project で medium の 2 s gate の対象外。

## 残るもの

- PEP 723 の backend / transitive 用依存を含む CHK002 と script-local の CHK003 が discovery 拡大で増える。
- Flask の README だけにある `celery -A make_celery`、MCP callback / loader entry は引き続き見落とす (NEW CHK001 16 件はいずれも likely)。
- nested example の import root、notebook の `%pip install` (#991)、未認識 magic (#992) の制限はそのまま。読めない cell は既存の warning/gap を通る。

## コマンドと生成物

```sh
git worktree add --detach target/issue-1023/base-tree d2c30ac
(cd target/issue-1023/base-tree && CARGO_TARGET_DIR="$PWD/../base-build" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR=target/issue-1023/head-build cargo build --release --locked --bin chokkin
scripts/clone-oss-fixtures.sh
python3 scripts/oss-metrics.py -b target/issue-1023/base-build/release/chokkin -o target/issue-1023/metrics/base -r 1 --with-strict
python3 scripts/oss-metrics.py -b target/issue-1023/head-build/release/chokkin -o target/issue-1023/metrics/head -r 3 --baseline target/issue-1023/metrics/base --with-strict --gate
python3 scripts/oss-gate.py -b target/issue-1023/head-build/release/chokkin --in-repo -o target/issue-1023/oss-gate
make check
```

`target/issue-1023/metrics/{base,head}/` に default/strict JSON、report、summary、
`head/finding-diff{,-strict}.tsv`、`head/compare.md`。固定 SHA は
`target/oss-clones/clones.lock.tsv`。再現 fixture・結果は
`target/issue-1023/repro/`・`repro-results.json`、補助 script は同 directory に置いた。
