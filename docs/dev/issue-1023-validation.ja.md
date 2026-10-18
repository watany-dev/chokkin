# #1023: examples / docs / notebook discovery の検証

- 日付: 2026-10-18
- baseline: main `d2c30ac`。修正: この report と同じ commit の source。
- 手順: `release-validation.ja.md`。別 worktree・別 `CARGO_TARGET_DIR` で release build。
- Rust: 1.96.0。corpus は `scripts/oss-clones.manifest` の固定 SHA 35 project と recall fixture 10 件。
- 解析対象の Python は実行していない。新規検出を静的に検証し、labels / expectations を更新した。

## バイナリと再現

| Build | Binary | Version | md5 |
|---|---|---|---|
| baseline | `target/issue-1023/base-build/release/chokkin` | 0.7.7 | `49d03f938920f1107e87c8c2a3c73d18` |
| 修正 | `target/issue-1023/head-build/release/chokkin` | 0.7.7 | `e1c79e028beb38e2b9fef03da42341be` |

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

全 corpus の `--gate` は exit 0。判定条件・閾値は変更していない。

| 検証 | 結果 |
|---|---|
| `make check` | PASS: fmt / doc / clippy / 1,948 tests (2 ignored) / deny / machete |
| in-repo OSS gate | PASS: 203/203 cold・warm・no-cache 一致、schema 正常、crash 0、期待する reject 18/18 |
| self-analysis + `chokkin-baseline.json` | exit 0 |
| corpus CHK002 FP gate | PASS: 1 FP / 39 reported (2.6%)、unclassified 0 |
| corpus recall | PASS: 959/959。既存 879 件を保持し、新規 80 件も検出。strict 同士の labeled tp 消失 0 |
| corpus precision floor | PASS: CHK003 100%、CHK004 100%、CHK006 93.1%、CHK010 94.7% |
| corpus count-growth / runtime floor | PASS |
| corpus crash / config error | PASS: いずれも 0 |
| corpus medium <= 2 s | PASS |

### PEP 723 の新規検出と正解ラベル

最初の CI では、Transformers の新規 CHK002 123 件が未判定で gate が落ちた。
project scope の runtime peer 判定を script scope でも共有し、既知の backend / plugin
を追加した。また、subprocess に渡す literal な argv list / tuple の先頭も tool の
使用として読む。計算した program 名は使用の根拠にしない。

source と script block を照合し、`scripts/oss-fixtures.labels.tsv` に次を記録した。

| 分類 | 件数 | 根拠 |
|---|---:|---|
| CHK002 fp、修正後は非検出 | 111 | 下表の provider が peer を読む、または script が command として実行する |
| CHK002 tp、検出を維持 | 12 | text-only language-modeling の albumentations 7 件、evaluate を使わず math.exp / numpy で評価する script の evaluate 5 件 |
| CHK003 tp、新しく検出 | 68 | Transformers 63、MCP SDK 4、OpenAI 1。直接 import の distribution が script block に無いことを AST と metadata で確認 |

peer の対応は、model / metric / plugin によって使用し得ることを表す。特定の実行引数で
必須かどうかまでは推定しない。provider が未使用なら peer も未使用のままで、
関係のない boto3 が検出され続けることも regression test で固定した。

| peer | provider と確認箇所 |
|---|---|
| accelerate / torch | Transformers の Trainer / model / device_map backend |
| sentencepiece / protobuf | Transformers の tokenization_utils_sentencepiece.py / convert_slow_tokenizer.py |
| timm / torchvision / kernels / huggingface-hub | Transformers の model backbone、image processing、integrations/hub_kernels.py、model/server loading |
| librosa / torchaudio | Transformers の audio_utils.py、datasets 1.18 の features/audio.py |
| scikit-learn / scipy | Transformers の generation/candidate_generator.py、evaluate の accuracy / glue metric |
| jiwer / rouge-score / seqeval / sacrebleu | evaluate が load する wer / rouge / seqeval / sacrebleu metric の import |
| pycocotools | torchmetrics の MeanAveragePrecision / COCO backend |
| py7zr | datasets の utils/extract.py |
| openai / inspect-evals | inspect-ai の openai-api provider、inspect_ai entry-point group の plugin loader |
| transformers (serve script) | serve_cmd の先頭が transformers、それを subprocess.Popen に渡す |

Transformers は corpus の固定 SHA
[`a906d3c`](https://github.com/huggingface/transformers/tree/a906d3c4b65095f2308b6a6a193e934d03b8eb5d/src/transformers) を確認した。
外部 provider の source も静的に読み、snapshot を `target/issue-1023/peer-evidence/` に保存した:
[evaluate](https://github.com/huggingface/evaluate/tree/a5db2ad54aaeadf15c7aa1f056bc6f7bbeeb7e5f/metrics)、
[datasets](https://github.com/huggingface/datasets/blob/18378f7322c7eb1c2d93f895dd8e9f71f8623934/src/datasets/utils/extract.py)、
[datasets 1.18 audio](https://github.com/huggingface/datasets/blob/1.18.0/src/datasets/features/audio.py)、
[torchmetrics](https://github.com/Lightning-AI/torchmetrics/blob/6c551c6469dc04fe92291c000ce2607860df92d2/src/torchmetrics/detection/helpers.py)、
[inspect-ai](https://github.com/UKGovernmentBEIS/inspect_ai/blob/a699376315c95b6410f5aec01af37c8dec452aea/src/inspect_ai/_util/entrypoints.py)、
[inspect-evals](https://github.com/UKGovernmentBEIS/inspect_evals/blob/8ef810f09930362302fc59b10a382d96dd767171/pyproject.toml)。

Transformers の期待件数は、上の tp と追加 inventory を確認して今回の測定値に更新した。
CHK002 は 0 → 12、CHK003 は 40 → 103。新規 CHK003 は全 63 件を正解ラベルとして固定した。
ほかの project の期待件数は変更していない。古い baseline の recall は 879/959、修正側は 959/959。

## Finding diff

warning 以上 / info の件数:

| Rule | baseline | 修正 |
|---|---|---|
| CHK001 | 33 / 5 | 49 / 5 |
| CHK002 | 27 / 0 | 39 / 0 |
| CHK003 | 120 / 140 | 187 / 140 |
| CHK004 | 1903 / 0 | 1903 / 0 |
| CHK005 | 28 / 54 | 28 / 54 |
| CHK006 | 207 / 2347 | 207 / 2414 |
| CHK007 | 0 / 0 | 0 / 0 |
| CHK008 | 18 / 0 | 18 / 0 |
| CHK009 | 0 / 5 | 0 / 5 |
| CHK010 | 202 / 525 | 203 / 598 |

既定 run は 251 行、strict 同士は 554 行。すべてを
`target/issue-1023/triage-reviewed.tsv` に 1 finding 1 行で分類した。NEW はすべて追加した
`examples/`・`docs/`・notebook の import / symbol / script 宣言に由来することを確認した。
以下の表には info も含む。全件の severity/confidence は raw JSON / diff TSV に残る。

| Rule | 既定 NEW | GONE | CHANGED | strict NEW | GONE | CHANGED |
|---|---:|---:|---:|---:|---:|---:|
| CHK001 | 16 | 0 | 0 | 16 | 0 | 0 |
| CHK002 | 12 | 0 | 1 | 12 | 19 | 31 |
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
| transformers | 84 / 4 / 0 | PEP 723 CHK002 12・CHK003 63、CHK010 (info) 9。既存 symbol 3 の追加参照と test の `run_glue` 解決で 4 件消失 |
| werkzeug | 18 / 1 / 0 | example の CHK010 (info) 18。追加参照で `LocalManager` の CHK006 (info) が消える |

strict の CHK002 GONE 19 のうち 16 は新たに読んだ docs/example が依存を使用するもの。
残り 3 は共有した peer 判定による修正で、mlflow の rouge-score と Transformers の
rouge-score / sacrebleu。いずれも evaluate が metric backend として読む。
CHANGED 31 のうち 23 は docs/conf.py や member entry の skip gap が解消したことによる
likely → certain、mitmproxy の 8 は上の未解決 import による certain → likely。
strict で追加される CHK003 / CHK004 も dev/docs の宣言との照合で、runtime import への
変更ではない。71 件の NEW CHK006 はすべて info。

### Downgrade vs vanish

| Rule | 既定 GONE | Recontexted | Demoted | To info | strict vanished tp / fp / other |
|---|---:|---:|---:|---:|---|
| CHK002 | 0 | 0 | 1 | 0 | 0 / 0 / 19 |
| CHK003 | 1 | 1 | 0 | 0 | 0 / 0 / 0 |
| CHK006 | 4 | 0 | 0 | 0 | 0 / 0 / 4 |
| CHK010 | 2 | 0 | 0 | 0 | 0 / 1 / 1 |

## 実行時間

corpus 本測定は baseline 1 回、修正 3 回の cold-run median。
環境による揺れがあるため、この値から速度改善は結論しない。
追加の 7 project は各 project で baseline → 修正を 3 回交互に測り、
`target/issue-1023/timing.json` に全 sample を残した。

| Project | baseline median ms | 修正 median ms |
|---|---:|---:|
| requests | 32 | 29 |
| werkzeug | 58 | 53 |
| flask | 40 | 41 |
| pydantic | 117 | 114 |
| mlflow | 640 | 909 |
| llama_index | 3178 | 2278 |
| transformers | 1030 | 1201 |

large project の値は揺れるため、速度改善の根拠にはしない。medium の 2 s gate はすべて通る。

## 残るもの

- 未知の runtime peer や計算した command 名は追えず、PEP 723 の未使用判定は引き続き likely。既存の fastmcp / pillow の FP 1 件は残る。
- Flask の README だけにある `celery -A make_celery`、MCP callback / loader entry は引き続き見落とす (NEW CHK001 16 件はいずれも likely)。
- nested example の import root、notebook の `%pip install` (#991)、未認識 magic (#992) の制限はそのまま。読めない cell は既存の warning/gap を通る。

## コマンドと生成物

```sh
git worktree add --detach target/issue-1023/base-tree d2c30ac
(cd target/issue-1023/base-tree && CARGO_TARGET_DIR="$PWD/../base-build" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR=target/issue-1023/head-build cargo build --release --locked --bin chokkin
scripts/clone-oss-fixtures.sh
python3 scripts/oss-metrics.py -b target/issue-1023/base-build/release/chokkin -o target/issue-1023/metrics/base-reviewed -r 1 --with-strict
python3 scripts/oss-metrics.py -b target/issue-1023/head-build/release/chokkin -o target/issue-1023/metrics/head-reviewed -r 3 --baseline target/issue-1023/metrics/base-reviewed --with-strict --gate
python3 scripts/oss-gate.py -b target/issue-1023/head-build/release/chokkin --in-repo -o target/issue-1023/oss-gate-reviewed
make check
```

`target/issue-1023/metrics/{base-reviewed,head-reviewed}/` に default/strict JSON、report、summary、
`head-reviewed/finding-diff{,-strict}.tsv`、`head-reviewed/compare.md`。固定 SHA は
`target/oss-clones/clones.lock.tsv`。再現 fixture・結果は
`target/issue-1023/repro/`・`repro-results.json`、補助 script は同 directory に置いた。
