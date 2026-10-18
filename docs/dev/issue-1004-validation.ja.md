# #1004: ASGI / WSGI entry の実機検証

2026-10-18。解析対象の Python code は実行せず、release binary で静的解析した。
手順は `release-validation.ja.md` に従い、baseline と修正後の worktree / build directory を分けた。

## Binary と固定した入力

baseline は `d2c30aca6d55b6ce5d35a5c217551aa4553df3cb`、修正後はこの変更の source。
両方とも `chokkin 0.7.7`。MD5 が異なることを確認した。

| 対象 | Binary path（repository root 相対） | MD5 |
|---|---|---|
| baseline | `target/issue-1004/target-base/release/chokkin` | `49d03f938920f1107e87c8c2a3c73d18` |
| 修正後 | `target/issue-1004/target-head/release/chokkin` | `733abfceb5004d840c09cc82910b7971` |

OSS corpus は `scripts/oss-clones.manifest` の 35 個の固定 commit と in-repo fixture 10 個。
`target/oss-clones/clones.lock.tsv` を両 binary で共有し、baseline は 1 回、修正後は 3 回と、
それぞれ独立した `--strict` run を測った。Saleor は
`0c0e2697cc72aa999f7786ae8aeda7665bd4285f` に固定した。

## Saleor `--production`

| Rule | baseline | 修正後 |
|---|---:|---:|
| CHK001 | 71 | 65 |
| CHK002 | 10 | 10 |
| CHK004 | 3 | 3 |
| CHK006 | 364 | 363 |
| CHK007 | 43 | 42 |
| CHK010 | 1 | 1 |

NEW / severity・confidence の変更は無い。GONE 8 件はすべて実際の利用を読み取った結果。

- CHK001: `saleor/asgi/` の `asgi_handler.py`、`cors_handler.py`、`gzip_compression.py`、`health_check.py`、`telemetry.py`、`usage_telemetry.py`。`saleor.asgi:application` の entry から到達する。
- CHK006: `saleor/core/telemetry/saleor_attributes.py:SALEOR_ENVIRONMENT_DOMAIN`。到達した `asgi/telemetry.py:24` が属性を読む。
- CHK007: `saleor/core/telemetry/__init__.py:set_global_attributes`。同じ `asgi/telemetry.py:8,23` が import して呼ぶ。

`--trace saleor/asgi/asgi_handler.py` は次の経路と両方の origin を出す。
`start.envfile`（180 行目）でなく command の 181 行目を示す。

```text
Trace path:
├─ file saleor/asgi/__init__.py
└─ import saleor.asgi.asgi_handler (line 15)
  entry root origins:
    symbol: CMD (Dockerfile:67)
    symbol: tool.poe.tasks.start (pyproject.toml:181)
```

Dockerfile exec form、poe の文字列 task / dotted `start.cmd`、Python の `uvicorn.run` をそれぞれ単独にした
小さい再現 project にも両 release binary を当てた。どれも CHK001 は served module・helper・
orphan の 3 件から orphan の 1 件になり、設定 / call の行が trace に残った。

## OSS corpus の差分と gate

既定 run / `--strict` とも、finding の差分は次の 1 件だけ。

| Status | Project | Finding | 判定 |
|---|---|---|---|
| GONE | airflow | `CHK006:airflow-core/src/airflow/utils/serve_logs/log_server.py:get_app` | FP 解消。`serve_logs/core.py:60-61` の `uvicorn.run("airflow.utils.serve_logs.log_server:get_app", ...)` が使う factory |

NEW / CHANGED は 0。降格 / info への変更 / context の変更は 0。
`--strict` の消失も同じ 1 件で、label は other。labelled TP / FP の消失は 0。
CHK002 / CHK003 は全 project で同じ。

全 gate が通った。CHK002 の FP 率は 3.7%（1 / 27）、labelled TP は 879 / 879 を保持し、
CHK003 / CHK004 / CHK006 / CHK010 の precision floor、runtime floor / rule growth の期待値、
medium project の cold run 2 秒以内を満たした。crash と CLI / config error は 0。
計測中は build も動いていたため、時間の差を高速化の根拠には使わない。

## 再実行

```sh
source /workspace/.chokkin-tools/env.sh
scripts/clone-oss-fixtures.sh
CARGO_TARGET_DIR=target/issue-1004/target-head cargo build --release --locked --bin chokkin
scripts/oss-metrics.py -b target/issue-1004/target-base/release/chokkin -o target/issue-1004/metrics/base -r 1 --with-strict
scripts/oss-metrics.py -b target/issue-1004/target-head/release/chokkin -o target/issue-1004/metrics/head-context -r 3 --baseline target/issue-1004/metrics/base --with-strict --gate
target/issue-1004/target-head/release/chokkin --production --no-cache --no-exit-code --reporter json --trace saleor/asgi/asgi_handler.py target/issue-1004/saleor
make check
```

baseline の build は `d2c30ac` の別 worktree で
`CARGO_TARGET_DIR=/workspace/chokkin/target/issue-1004/target-base cargo build --release --locked --bin chokkin`
を実行した。raw JSON / trace / metrics は `target/issue-1004/` に残した。

`make check` は 1,953 tests を含め全項目が通った。wrapup の correctness / ponytail-review /
comment sweep を実施した。初回 CI の mutation shard 0 は、server root の caller context と、
未発見の test app を production で意図的に外す guard の 2 件を missed とした。
source discovery が test file を落とす前の entry plan を直接検査する 2 tests と、
除外した test app の skipped-entry warning を default / production で比べる CLI test を追加した。
報告された `==` → `!=` と `!production` → `production` を個別に再現し、新しい test が
それぞれ失敗することを確認してから実装を戻した。shard 1 の残る 1 件は Poe の文字列 task
の match arm の削除だったため、直接の文字列 task を CLI の再現 matrix に追加し、同じ削除を
行うと test が失敗することも確認した。runtime の実装変更は無い。

## Found, not fixed

- 固定した Saleor の `saleor/__main__.py:3-5` は `uvicorn.Config("saleor.asgi:application", ...)` を使う。この形は今回の literal `uvicorn.run` の対象外（recall）。同じ target は Dockerfile / poe から読むので、今回の 6 件は解消する。
