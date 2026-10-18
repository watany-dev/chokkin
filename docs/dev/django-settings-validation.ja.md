# Django settings の dotted path 参照: #1003 の検証

2026-10-18、Linux x86_64、Rust 1.96.0。解析対象の Python code は実行していない。

## ビルドと再現条件

baseline は `d2c30ac`、HEAD の Rust source tree は `1a326af5a56e69bc4e27680935199c1a29ae5fb6`。baseline は独立した worktree と target directory、HEAD は別の target directory で `cargo build --release --locked --bin chokkin` した。

| Build | Binary | Version | MD5 |
|---|---|---|---|
| base | `target/issue-1003/target-base/release/chokkin` | `chokkin 0.7.7` | `49d03f938920f1107e87c8c2a3c73d18` |
| head | `target/issue-1003/target-head/release/chokkin` | `chokkin 0.7.7` | `ef69e31506b58afbe8cd80f42423cf7e` |

同じ package version だが MD5 は異なる。fixture の `AUTHENTICATION_BACKENDS` でだけ参照する backend と、その backend が import する helper を release binary で解析した。`--production` の CHK001 は baseline の backend / helper / unused の 3 件から、HEAD は unused だけの 1 件になった。

## Saleor

checkout: `saleor/saleor@0c0e2697cc72aa999f7786ae8aeda7665bd4285f`。両 binary で同じ checkout を `--no-cache --reporter json`、追加 option なし / `--production` / `--strict` で測定。以下は info を含む JSON の finding 件数。

| Mode | Rule | Baseline | HEAD |
|---|---|---:|---:|
| default | CHK001 | 40 | 36 |
| default | CHK002 | 8 | 8 |
| default | CHK004 | 3 | 3 |
| default | CHK006 | 311 | 314 |
| default | CHK007 | 38 | 38 |
| default | CHK010 | 1 | 2 |
| production | CHK001 | 71 | 43 |
| production | CHK002 | 10 | 8 |
| production | CHK004 | 3 | 3 |
| production | CHK006 | 364 | 338 |
| production | CHK007 | 43 | 43 |
| production | CHK010 | 1 | 2 |
| strict | CHK001 | 40 | 36 |
| strict | CHK002 | 19 | 19 |
| strict | CHK004 | 4 | 4 |
| strict | CHK006 | 311 | 314 |
| strict | CHK007 | 38 | 38 |
| strict | CHK010 | 1 | 2 |

production で消えた CHK001 は 28 件。すべて HEAD の `--trace` で settings 参照から到達することを確認した。

| File | Trace |
|---|---|
| `saleor/core/auth_backend.py` | `Trace path: → └─ plugin ref saleor.core.auth_backend (AUTHENTICATION_BACKENDS)` |
| `saleor/core/db_routers.py` | `Trace path: → └─ plugin ref saleor.core.db_routers (DATABASE_ROUTERS)` |
| `saleor/core/hashers.py` | `Trace path: → └─ plugin ref saleor.core.hashers (PASSWORD_HASHERS)` |
| `saleor/core/logging.py` | `Trace path: → └─ plugin ref saleor.core.logging (LOGGING)` |
| `saleor/core/sqs.py` | `Trace path: → └─ plugin ref saleor.core.sqs (CELERY_BROKER_TRANSPORT)` |
| `saleor/core/storages.py` | `Trace path: → └─ plugin ref saleor.core.storages (STORAGES)` |
| `saleor/payment/gateways/stripe/consts.py` | `Trace path: → ├─ plugin ref saleor.payment.gateways.stripe.plugin (BUILTIN_PLUGINS) → └─ import saleor.payment.gateways.stripe.consts (line 24)` |
| `saleor/payment/gateways/stripe/plugin.py` | `Trace path: → └─ plugin ref saleor.payment.gateways.stripe.plugin (BUILTIN_PLUGINS)` |
| `saleor/payment/gateways/stripe/stripe_api.py` | `Trace path: → ├─ plugin ref saleor.payment.gateways.stripe.plugin (BUILTIN_PLUGINS) → └─ import saleor.payment.gateways.stripe.stripe_api (line 34)` |
| `saleor/payment/gateways/stripe/webhooks.py` | `Trace path: → ├─ plugin ref saleor.payment.gateways.stripe.plugin (BUILTIN_PLUGINS) → └─ import saleor.payment.gateways.stripe.webhooks (line 47)` |
| `saleor/payment/gateways/utils.py` | `Trace path: → ├─ plugin ref saleor.payment.gateways.stripe.plugin (BUILTIN_PLUGINS) → └─ import saleor.payment.gateways.utils (line 23)` |
| `saleor/plugins/admin_email/constants.py` | `Trace path: → ├─ plugin ref saleor.plugins.admin_email.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.admin_email.constants (line 24)` |
| `saleor/plugins/admin_email/notify_events.py` | `Trace path: → ├─ plugin ref saleor.plugins.admin_email.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.admin_email.notify_events (line 25)` |
| `saleor/plugins/admin_email/plugin.py` | `Trace path: → └─ plugin ref saleor.plugins.admin_email.plugin (BUILTIN_PLUGINS)` |
| `saleor/plugins/avatax/plugin.py` | `Trace path: → └─ plugin ref saleor.plugins.avatax.plugin (BUILTIN_PLUGINS)` |
| `saleor/plugins/openid_connect/client.py` | `Trace path: → ├─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.openid_connect.client (line 27)` |
| `saleor/plugins/openid_connect/const.py` | `Trace path: → ├─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.openid_connect.const (line 28)` |
| `saleor/plugins/openid_connect/dataclasses.py` | `Trace path: → ├─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.openid_connect.dataclasses (line 29)` |
| `saleor/plugins/openid_connect/exceptions.py` | `Trace path: → ├─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.openid_connect.exceptions (line 30)` |
| `saleor/plugins/openid_connect/plugin.py` | `Trace path: → └─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS)` |
| `saleor/plugins/openid_connect/utils.py` | `Trace path: → ├─ plugin ref saleor.plugins.openid_connect.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.openid_connect.utils (line 31)` |
| `saleor/plugins/user_email/constants.py` | `Trace path: → ├─ plugin ref saleor.plugins.user_email.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.user_email.constants (line 23)` |
| `saleor/plugins/user_email/notify_events.py` | `Trace path: → ├─ plugin ref saleor.plugins.user_email.plugin (BUILTIN_PLUGINS) → └─ import saleor.plugins.user_email.notify_events (line 25)` |
| `saleor/plugins/user_email/plugin.py` | `Trace path: → └─ plugin ref saleor.plugins.user_email.plugin (BUILTIN_PLUGINS)` |
| `saleor/plugins/webhook/plugin.py` | `Trace path: → └─ plugin ref saleor.plugins.webhook.plugin (BUILTIN_PLUGINS)` |
| `saleor/site/context_processors.py` | `Trace path: → └─ plugin ref saleor.site.context_processors (context_processors)` |
| `saleor/webhook/circuit_breaker/storage.py` | `Trace path: → └─ plugin ref saleor.webhook.circuit_breaker.storage (BREAKER_BOARD_STORAGE_CLASS)` |
| `saleor/webhook/transport/payment.py` | `Trace path: → ├─ plugin ref saleor.plugins.webhook.plugin (BUILTIN_PLUGINS) → └─ import saleor.webhook.transport.payment (line 105)` |

各 finding の NEW / GONE / CHANGED は [`issue-1003-saleor-diff.tsv`](./issue-1003-saleor-diff.tsv) に 1 件ずつ固定した。

- CHK002 の production の GONE 2 件は `kombu` と `stripe`。到達するようになった transport / payment module の import が根拠。
- CHK006 の production の GONE 45 件は、新しく到達した plugin とその import 先から参照される symbol。NEW 19 件は、新しく到達した module の未参照 symbol。registry 経由の class 使用を CHK006 に反映する作業は #698。
- default / strict の CHK001 の GONE はそれぞれ 4 件。NEW の CHK006 はそれぞれ 3 件。
- 各 mode の NEW CHK010 1 件は、新しく到達した `saleor/core/logging.py:5` の `pythonjsonlogger`。`python-json-logger` は宣言済みだが provider map が対応しない。
- 3 mode とも severity / confidence の CHANGED は 0 件。default と strict の CHK001 の消失数は同じで、表示 floor による降格ではなく、参照が根拠となる消失。
- production にだけ残る ASGI の 6 file は #1004。Django の management command / AppConfig の規約による entry は #399 の範囲。

## OSS corpus

35 の pinned OSS checkout と 10 の recall fixture、合計 45 project を baseline 1 回 / HEAD 3 回、両方 `--with-strict` で測った。default / strict の finding-level diff はともに 0 件。Recontexted / Demoted / To info / Vanished tp・fp・other もすべて 0 件で、未分類の消失もない。CHK003 growth gate は対象外 project の未分類 hit が 12 → 12、strict の TP 消失は 0。

| Criterion | Target | Measured | Result |
|---|---|---|---|
| Unused-dep FP rate (CHK002) | < 5% | 3.7% (1 FP / 27 reported, 0 unclassified) | ✅ PASS |
| Precision floor (labelled hits) | CHK003 >= 90%, CHK004 >= 95%, CHK006 >= 90%, CHK010 >= 90% | all at or above | ✅ PASS |
| Recall (`tp` labels) | all detected | 879/879 detected | ✅ PASS |
| Crashes (exit 3) | 0 | 0 | ✅ PASS |
| CLI/config errors (exit 2) | 0 | 0 | ✅ PASS |
| Expectations (runtime floor, rule growth) | within | all within | ✅ PASS |
| Cold run, medium project | <= 2000 ms | all within budget | ✅ PASS |

label 付き finding の precision と rule 別件数 (info は別列):

| Rule | Reported | Info | tp | fp | deferred | unknown | Coverage % | Precision % | Stale |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| CHK001 | 33 | 5 | 13 | 20 | 0 | 0 | 100.0 | 39.4 | 16 |
| CHK002 | 27 | 0 | 26 | 1 | 0 | 0 | 100.0 | 96.3 | 74 |
| CHK003 | 120 | 140 | 82 | 0 | 12 | 26 | 68.3 | 100.0 | 0 |
| CHK004 | 1903 | 0 | 601 | 0 | 0 | 1302 | 31.6 | 100.0 | 0 |
| CHK005 | 28 | 54 | 10 | 0 | 0 | 18 | 35.7 | 100.0 | 0 |
| CHK006 | 207 | 2347 | 81 | 6 | 0 | 120 | 42.0 | 93.1 | 0 |
| CHK007 | 0 | 0 | 0 | 0 | 0 | 0 | n/a | n/a | 0 |
| CHK008 | 18 | 0 | 7 | 1 | 0 | 10 | 44.4 | 87.5 | 0 |
| CHK009 | 0 | 5 | 0 | 0 | 0 | 0 | n/a | n/a | 0 |
| CHK010 | 202 | 525 | 108 | 7 | 0 | 87 | 56.9 | 93.9 | 1 |

build と corpus 測定の並行実行による負荷を避け、medium の 16 project は追加で baseline / HEAD の順序を交互に入れ替えて各 3 回ずつ測った。以下は cold run の median (ms)。最大の HEAD median は 132.5 ms で 2 秒の gate 内。速度改善の主張はしない。

| Project | Baseline ms | HEAD ms |
|---|---:|---:|
| requests | 23.2 | 18.0 |
| urllib3 | 27.7 | 27.3 |
| click | 20.8 | 17.5 |
| jinja | 20.5 | 19.7 |
| werkzeug | 26.0 | 27.5 |
| flask | 20.9 | 21.0 |
| httpx | 20.0 | 23.4 |
| starlette | 40.4 | 22.7 |
| uvicorn | 29.5 | 20.9 |
| attrs | 21.2 | 21.0 |
| structlog | 20.8 | 20.4 |
| typer | 63.1 | 47.9 |
| pydantic | 151.6 | 132.5 |
| virtualenv | 33.8 | 49.3 |
| poetry | 57.3 | 61.2 |
| pdm | 38.2 | 40.6 |

## チェックとレビュー

`make check` は `d2c30ac` に rebase 後、通常の `/tmp` で全て成功した。最初の baseline では `/tmp/.git` の影響で既存の invalid-UTF-8 gitignore fixture が失敗したが、clean TMPDIR で成功し、最新 main の fixture 修正も取り込んだ。対象コードの変更で回避してはいない。

`$wrapup` の正しさレビューでは source と test の差分を読み、computed key / tuple unpacking の gap、nested field の sibling 保持、star import、scope、first-party 限定、重複、origin を確認した。unresolved reference の `--explain` に field・行を残す修正も含めた。過剰設計レビューは `Lean already. Ship.`。コメントは unknown path や scope の理由を残し、コメントのみの追加変更は不要だった。

初回 CI の mutation gate では 6 件が生き残った。partial tuple、named key を要求する位置の list / tuple、list unpacking、import slot 外の subscript 更新、従来の Django field を二重に参照にしないことの negative case を追加した。報告された 6 mutation を個別に再適用し、それぞれ regression test が失敗することをローカルで確認した。

追加の reference-kind test で、既存の WSGI / ASGI reader が `module:application` だけを読み、Django の通常の `module.application` を落としていたことも分かった。同じ module 内の小さな修正として別 commit にした。colon 表記は保ち、`None` / 空文字列は未設定、計算式や不正な target は field の gap にする。両 field の release 再現で backend / helper / unused の CHK001 は 3 → 1、computed target の gap は settings の 2 行目を指すことを確認した。変更後に release build、Saleor の全 mode と 28 trace、OSS corpus を再測定し、結果は上の表のまま変わらなかった。

## 再現コマンド

```sh
git worktree add target/issue-1003/wt-base d2c30ac
(cd target/issue-1003/wt-base && CARGO_TARGET_DIR="$PWD/../target-base" cargo build --release --locked --bin chokkin)
CARGO_TARGET_DIR=target/issue-1003/target-head cargo build --release --locked --bin chokkin
scripts/clone-oss-fixtures.sh
scripts/oss-metrics.py -b target/issue-1003/target-base/release/chokkin -o target/issue-1003/metrics/base-final -r 1 --with-strict
scripts/oss-metrics.py -b target/issue-1003/target-head/release/chokkin -o target/issue-1003/metrics/head-server -r 3 --baseline target/issue-1003/metrics/base-final --with-strict --gate
make check
```

初回の実測は base / head の収集を並行に行い、`scripts/oss-metrics.py` の比較関数で diff / gate を計算した。追加の WSGI / ASGI 修正後は上の HEAD コマンドを `--baseline --with-strict --gate` 付きで実行し、`metrics/head-server/` に最終結果を固定した。生 JSON、corpus の lock、比較表、trace、再現 harness は `target/issue-1003/` と `/tmp/issue-1003-*.py` に残した。
