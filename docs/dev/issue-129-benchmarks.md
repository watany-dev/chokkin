# #129: module-index key の全ソース再読込を除去

2026-09-15、Linux x86_64 / 2 logical CPUs / Rust 1.93.0。
production baseline は `825f46d`。`/tmp` の約 2KiB modules（関数・ループ・条件式）を
1k/10k 個作り、tests / entry modules も含めて到達性を計測。
fixture / graph clone / drop は計測外。cache on は module-index cache の warm hit。
セットアップで cache on/off の finding が一致することを assert する。

```bash
cargo bench --bench reachability --locked -- --save-baseline issue129
# src の変更後
cargo bench --bench reachability --locked -- --baseline issue129
# 10k の control に変動が出たため、測定時間を延ばして再確認
cargo bench --bench reachability --locked -- '10000$' --baseline issue129 --measurement-time 10
```

他のビルド・テストと重ねない単独実行。30 samples の Criterion median。
1k は通常実行、10k は最後の長時間測定の値。

| modules | cache | before | after | median change |
|---|---|---:|---:|---:|
| 1000 | off | 0.469 ms | 0.474 ms | +1.2% |
| 1000 | on | 16.796 ms | 0.936 ms | -94.4% |
| 10000 | off | 5.870 ms | 5.972 ms | +1.7% |
| 10000 | on | 173.915 ms | 12.580 ms | -92.8% |

1k は cache on / off = 1.97 倍（測定上の「2倍以内」を満たす）。10k の cache on も
数十 ms 以下。cache on の改善は両規模で有意（p < 0.05）。2倍の境界には近く、
環境差・測定誤差まで含めた保証ではない。

10k の最初の最終実装測定では、on の推定値 14.11 ms に対し、変更していない off にも
約16%の遅延が出た。上記の長時間再測定では off の有意差は再現せず（p = 0.51）、
on の約93%短縮は維持した。変動を隠さないため、再測定の理由も記録する。

key は graph のパス列（順序保持）と layout だけを使い、ソース本文・mtime を読まない。
`module-index-v2` に更新し旧 key を無効化する。payload の文字列はコピーせず
復元 map に移し、entry 数に合わせて HashMap を確保する。

検証: fmt / clippy / cargo doc / 全639 tests / cargo machete は成功。
ソース本文を削除しても index の cache hit が成立し、graph のパス追加・順序変更と
layout 変更で key が変わることを regression test で確認する。
初回の `make check` は lockfile の RUSTSEC-2026-0258 (h2) と
RUSTSEC-2026-0285 (rustls) により cargo deny で失敗した。
後続の CI 修正で修正版へ更新し、`make check` の全項目が成功した。
