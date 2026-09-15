# #139: パイプライン回帰ベンチ

2026-09-15、Linux x86_64 / 2 logical CPUs / Rust 1.93.0、baseline `825f46d`。
`/tmp` の synthetic fixture。1,000 個の約 2KiB module に関数・ループ・条件式を含み、
既存 fixture の tests / entry module も追加される。Python code は実行しない。
OS page cache は温まった状態。cold/warm は chokkin の disk cache を指す。

```bash
cargo bench --bench pipeline --locked -- --save-baseline issue139
CHOKKIN_BENCH_LARGE=1 cargo bench --bench pipeline --locked
# 比較には --baseline issue139。全 bench は make bench-save / bench-cmp にも含まれる。
```

上段の通常 1k 実行を計測。5k/10k は opt-in として登録し、通常計測には含めない。
20 samples の Criterion median。セットアップで cache 有効・無効の finding 一致を assert する。

| benchmark | median (1k) |
|---|---:|
| `analyze_cold` | 476.696 ms |
| `analyze_warm` | 62.105 ms |
| `discover_populated_cache` | 4.794 ms |
| `parse_cold_no_cache` | 325.693 ms |
| `parse_cold_with_disk_cache` | 449.740 ms |
| `parse_disk_warm` | 33.101 ms |
| `reachability_cache_off` | 0.475 ms |
| `reachability_cache_on` | 17.045 ms |

cache 削除・fixture 作成・graph clone は測定外。parse_disk_warm は
in-memory store を渡さず disk JSON を再利用する。reachability は graph の clone / drop
を測定外に置く。最後の discovery は parse cache が埋まった状態で実行する。

この branch は回帰の計測範囲を追加するもので、解析自体の性能改善は含まない。

検証: fmt / clippy / cargo doc / 全639 tests / cargo machete は成功。
`make check` の cargo deny は既存 lockfile の RUSTSEC-2026-0258 (h2) と
RUSTSEC-2026-0285 (rustls) により失敗。Cargo.lock は変更していない。
