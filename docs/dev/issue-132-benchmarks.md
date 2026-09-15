# #132: 自前キャッシュの探索除外

2026-09-15、Linux x86_64 / 2 logical CPUs / Rust 1.93.0。
production baseline は `825f46d`。同じ追加 benchmark を変更前後で単独実行した。
fixture は `/tmp`、1,000 modules + tests。populated は root の
`.chokkin/cache/parse/` に 5,000 JSON files を追加した状態。
OS page cache は温まった状態で、30 samples の Criterion median。

```bash
cargo bench --bench sources --locked -- discover_cache --save-baseline issue132
# src の変更後
cargo bench --bench sources --locked -- discover_cache --baseline issue132
```

| case | before | after | median change |
|---|---:|---:|---:|
| empty | 3.531 ms | 3.582 ms | +1.4% |
| populated | 8.862 ms | 3.609 ms | -59.3% |

Criterion の比較でも populated の改善は有意（p < 0.05、推定区間 -61.3%〜-55.1%）。
empty は有意差なし。修正後の populated は empty と同程度。
最初に別 branch のコンパイルと重なった測定は破棄し、単独実行し直した値を使った。
既存 `filter_entry` の exclude 枝刈りを再利用し、専用 walker は追加していない。

検証: fmt / clippy / cargo doc / 全640 tests / cargo machete は成功。
初回の `make check` は lockfile の RUSTSEC-2026-0258 (h2) と
RUSTSEC-2026-0285 (rustls) により cargo deny で失敗した。
後続の CI 修正で修正版へ更新し、`make check` の全項目が成功した。
