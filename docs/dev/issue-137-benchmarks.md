# #137: bundled resolver map の再利用

2026-09-15、Linux x86_64 / 2 logical CPUs / Rust 1.93.0。
production baseline は `825f46d`、同じ benchmark を変更前後で実行。
他のビルド・テストと重ねず、100 samples の Criterion median を比較する。

```bash
cargo bench --bench resolver --locked -- --save-baseline issue137
# src の変更後
cargo bench --bench resolver --locked -- --baseline issue137
```

| map | before | after | median change |
|---|---:|---:|---:|
| imports | 70.387 µs | 0.018 µs | -99.974% |
| binaries | 4.536 µs | 1.571 µs | -65.359% |

両方とも Criterion で有意な改善（p < 0.05）。import reverse map は `LazyLock` の
静的参照を再利用する。binary map は正規化済みの静的 map を clone してから
user → venv の順に overlay し、公開 `BTreeMap` 返却型と上書き順を維持する。

これは同一プロセスの2回目以降の map 構築の比較。最初の LazyLock 初期化コストは残るため、
CLI を起動し直すたびの初回解析の速度向上を示す測定ではない。

検証: fmt / clippy / cargo doc / 全640 tests / cargo machete は成功。
設定の上書きが静的 bundled defaults を汚さない regression test も含む。
`make check` の cargo deny は既存 lockfile の RUSTSEC-2026-0258 (h2) と
RUSTSEC-2026-0285 (rustls) により失敗。Cargo.lock は変更していない。
