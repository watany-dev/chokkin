# 性能ボトルネック調査 (ベンチ計測 + プロファイル)

2026-09-22、Linux x86_64 / 4 logical CPUs、HEAD `e0d2f62` (#171 まで)。
Python code は実行しない。

## 計測上の制約

- この環境では crates.io が 403 を返す (org policy)。そのため HEAD の build と
  `cargo bench` はできなかった。同じ理由で `make check` も実行できない。
- 実測値はすべて PyPI 配布の `chokkin==0.4.0` バイナリ (`uvx chokkin@0.4.0`) のもの。
- HEAD については、v0.4.0 の実測で見えたホットスポットがすでに修正済みかをコードで照合した。
  残っている候補は静的解析で抽出した。

## fixture

`benches/support/mod.rs` の `synth_realistic_project(N)` と同じ構造にした。
約 2KiB の module を N 個含み、各 module は `import requests` と関数・ループを持つ。
これに tests / scripts / docs が加わる。これを Python で生成した。
OS page cache は温まった状態で、各値は 7 回の median。

## 実測 (v0.4.0, ms)

| N | `--probe` | cold `--no-cache` | cold + cache 書き込み | warm |
|---:|---:|---:|---:|---:|
| 1k | 5.2 | 243.5 | 322.4 | 43.7 |
| 5k | 14.1 | 1198.9 | 2439.6 | 226.6 |
| 10k | 28.6 | 2269.9 | 4586.3 | 469.2 |

- cold の cache 書き込みコストは超線形だった。`strace -c` によると、
  openat 1 回あたりが 1k の 9µs から 5k では 57µs に増える。原因は module ごとの cache ファイル。
- `taskset -c 0` で 1 CPU に制限してもほぼ同値だった (v0.4.0 は並列 parse なし)。
- warm は N にほぼ線形 (約 45µs/file)。

### warm 1k の callgrind (v0.4.0, stripped binary)

シンボルがないため、定数と呼び出し元からアドレスを同定した。

| 割合 | 内容 |
|---:|---|
| 12.6% | FNV-1a の 1 byte ずつのループ (`CacheKeyHasher::write`) で source 全文を hash |
| ~17% | malloc / free |
| ~9% | serde_json の文字列スキャン (cache JSON 読み込み) |
| 6.7% | UTF-8 検証 |
| 3.9% | `Path` components 比較。呼び出し元は 1 箇所で 1 回だけ呼ばれる (discovery 時の path sort / BTreeMap と推定) |
| 2.5% | memcmp |

### unused-dep evidence の A/B

`requirements` があるケースとないケースの差から、`rules/deps/unused.rs` の
evidence 構築コストを見積もった。5k で約 27 ms、10k で約 49 ms かかり、warm の約 10% に当たる。

## HEAD で修正済みのもの

| v0.4.0 の症状 | HEAD の修正 |
|---|---|
| module ごとの cache ファイルによる超線形な書き込み | #171 cache context ごとに 1 bundle |
| warm で source を 2 回読み、全文を hash | #169 stat ベースの `SourceFingerprint`、#167 |
| 並列 parse なし | #170 並列 parse |

ただし `from_absolute_stat` は、mtime が現在時刻から 2 秒以内 (`RACY_MTIME_WINDOW`)
なら全文 hash に戻る。そのため、編集直後の実行と fixture 生成直後のベンチでは
12% 級の byte-loop hash がまだ発生する (`src/cache.rs` の `CacheKeyHasher::write`)。

## HEAD に残る候補 (優先度順)

1. **parse cache の余分な clone と毎回新しく作られる in-memory store**
   - `src/pipeline/analyze.rs:165` は毎回 `ParseCacheStore::new()` を作る。
     1 プロセス 1 回の CLI では再利用されない。
   - `src/parser/parse.rs` の `drain_caches` と retained ループでは、
     `ParsedModule` を module ごとに最大 3 回 clone する
     (`stored.get().cloned()`、`store.insert(.., parsed.clone())`、`retained.insert(.., parsed.clone())`)。
     v0.4.0 で約 17% を占めた malloc/free の主因候補。
2. **bundle の全体書き換え**
   - 1 ファイルでも変わると bundle 全体を serde_json で再シリアライズして書く。
     10k では cold 書き込みの大半を占める。
   - 対策の方向は、差分があるときだけ書く現状を維持したうえで、
     より高速な形式にすること (bincode 等。依存追加の判断が必要)。
3. **`rules/deps/unused.rs` の evidence 構築** (実測で warm の約 10%)
   - `build_reachability_evidence` が unused dep 1 件ごとに imports を 2 回走査する (O(U×I))。
   - `top_level_modules_for_distribution` が dep ごとに全 `graph.edges()` を走査する。
   - `unreachable_file_suffix` が `reachability.unreachable` を線形に `.find` する (O(I×N_unreachable))。
   - 対策: distribution → imports の索引と unreachable の HashSet を 1 回だけ構築する。
4. **`rules/deps/missing.rs` の `is_transitive_only`** (~274 行)
   - import site ごとに lockfile graph を String clone しながら BFS する。
   - `optional_imports.contains(&(import.file.clone(), import.line))` でも site ごとに String を確保する。
   - distribution 単位で memo 化できる。未実測: CHK003 を出す lockfile シナリオを作れなかった。
5. **reachable 集合の重複構築**
   - `HashSet<String>` を `rules/deps/reconcile.rs:83`、`rules/deps/used.rs:56`、
     `rules/symbols/analyze.rs:68` の 3 箇所で個別に作る。`RuleContext` に 1 つ持たせられる。
6. **symbols の二重ループと線形探索**
   - `rules/symbols/graph.rs:117-126` の imports × accesses のネストループ。
   - `rules/symbols/graph.rs:69` と `rules/symbols/exports.rs:52` の `__all__` の線形 scan。
7. **小さなもの**
   - `resolver/resolve.rs:151-189, 263-275` で site ごとに String を確保する。
   - `rules/ignore.rs:265-270` は glob を呼び出しのたびにコンパイルする。
   - module-index cache を二重に deserialize する (`reachability/module_index.rs`、`cache.rs:226-241`)。
   - `pipeline/probe.rs:106-131` で workspace members を 2 回 walk する。

## ベンチ fixture の忠実度の問題

`synth_realistic_project` は `src/bench_acme/__init__.py` から全 module を import する。
ところが library mode の entry roots は `scripts/run.py` と tests だけになる。
そのため `src/bench_acme` 配下は全 module が unreachable と判定される。
結果として、すべてのファイルが import している `requests` まで CHK002 (unused) になる。

この fixture では、reachable な経路 (到達 module の symbol 解析、used-dep 判定) の
コストを測れていない。entry root から package を import する script を追加するか、
`[project.scripts]` を宣言した fixture を追加することを推奨する。

## 次の一手

crates.io に到達できる環境で以下を行う。

1. `CHOKKIN_BENCH_LARGE=1 cargo bench --bench pipeline` で HEAD の baseline を取る。
2. 上の 1 (clone 削減) と 3 (evidence の索引化) から着手し、Criterion で差分を確認する。
