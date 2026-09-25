# #294: parser 移行の検証計画 (R-14、#320 / #321)

2026-09-25 時点の計画。方針は ADR 0001 の Amendment 2026-09-25 に記録した。
このドキュメントは、pin と v0.6 の切り替えを確定させる前に取る実測 (#320 / #321) の
手順と判定基準をまとめる。#140 の
[`issue-140-parser-reevaluation.md`](issue-140-parser-reevaluation.md) の続き。

## 現状: 実測は未取得

作業環境の agent proxy が `static.crates.io` を拒否するため、`ruff_python_parser` を
download できず、PoC の build も bench もまだ取れていない (#140 と同じ制約)。
下の手順は crate を取得できる環境 (手元、または GitHub Actions) でそのまま実行できる。
結果は各 issue の comment に書き、ADR 0001 の pin を確定する。

## 影響範囲 (2026-09-25 の main で確認)

| 範囲 | rustpython 依存 | 移行時の作業 |
|---|---|---|
| `src/parser/parse.rs` | `ast::Suite::parse`、`RandomLocator`、`ParseError` | parse の入口、行番号の計算、`syntax_diagnostic` / `syntax_target_hint` / `note_unsupported_syntax` |
| `src/parser/visit.rs` | `Stmt` / `Expr` / `Alias` / `Arguments` / `Comprehension` / `ExceptHandler` / `StmtTry(Star)`、`Ranged` | AST 走査の本体。移行の工数の大半はここ |
| `src/parser/exports.rs` | `Stmt` / `Expr` / `Constant::Str`、`RandomLocator` | `__all__` の抽出 |
| `src/parser/dynamic.rs` | `Alias` / `Constant` / `Expr` / `ExprCall` | `importlib.import_module` の文字列引数 |
| `src/parser/decorators.rs` / `attributes.rs` / `type_checking.rs` / `platform_guard.rs` | `Expr` / `Stmt` / `CmpOp` | 小さな helper。型名の置き換えが中心 |
| `src/parser/ignores.rs` / `relative.rs` / `types.rs` / `error.rs` | なし | 変更不要 |
| `src/parser/` 外 | なし (`ParsedModule` などの backend 非依存な型だけを使う) | 変更不要 |
| `src/manifest/literals.rs` | なし (文字列走査) | 変更不要。AST 化は別件 |
| parse cache の `unit_version` (`src/parser/parse.rs` の `parse-v5`) | — | 移行 PR で上げる |
| `deny.toml` | `unic-*` 由来の unmaintained advisory 6 件を ignore | 移行 PR で ignore を消す |
| `.github/dependabot.yml` | — | `ruff_*` を 1 PR にまとめる group を足す |

## #320: PoC と差分・性能計測

### 手順

1. `origin/main` から PoC branch を切る (main には merge しない)。
2. `Cargo.toml` に feature gate 付きで追加する。version は全 crate を同じ `N` に完全固定する:

   ```toml
   [features]
   parser-ruff = ["dep:ruff_python_parser", "dep:ruff_python_ast", "dep:ruff_text_size"]

   [dependencies]
   ruff_python_parser = { version = "=0.0.N", optional = true }
   ruff_python_ast = { version = "=0.0.N", optional = true }
   ruff_text_size = { version = "=0.0.N", optional = true }
   ```

3. `src/parser/` に backend module を足し、`parse_python_source` の中身を
   `#[cfg(feature = "parser-ruff")]` で差し替える。`ParsedModule` /
   `ParseDiagnostic` の形は変えない。
4. 差分と性能を取る:

   ```bash
   # baseline (rustpython)
   CHOKKIN_BENCH_LARGE=1 cargo bench --bench pipeline --locked -- --save-baseline rustpython-10k
   # ruff backend
   cargo test --features parser-ruff --locked
   CHOKKIN_BENCH_LARGE=1 cargo bench --features parser-ruff --bench pipeline --locked -- --baseline rustpython-10k
   ```

5. 新構文の fixture を PoC branch に足し、parse できることを確かめる:
   `class C[T]: ...` / `def f[T](x: T) -> T: ...` / `type X[T] = list[T]` (PEP 695)、
   `t"{x}"` (PEP 750)、`except A, B:` (PEP 758)、`lazy import x` /
   `lazy from x import y` (PEP 810、parser が対応していれば)。

### 判定基準

| 軸 | 合格 | 記録するもの |
|---|---|---|
| 既存 test | 全件通る、または差分を「意図した改善 / 回帰 / AST 差による書き換え」に分類し、回帰が 0 | 失敗 test ごとの分類表 |
| 行番号 | `tests/fixtures/parse` と `tests/fixtures/parser_spike` の import / symbol / decorator の行番号が一致 | 不一致の一覧 |
| 新構文 | PEP 695 / 750 / 758 が syntax error にならず、中の import が edge になる | fixture ごとの結果 |
| cold parse 10k | rustpython 比で遅くならない (median の比 ≤ 1.0) | `parse_cold_no_cache` / `analyze_cold` の median と比 |
| `cargo deny` / `cargo machete` | 通る | 新しく出た advisory / license |

新構文への対応は correctness の問題なので、cold parse が遅くなったことだけを理由に
不採用にはしない。比が 1.2 を超えたら ADR に記録し、v0.7 の性能作業で扱う。

## #321: cross wheel build 検証

### 手順

`Release` workflow (`.github/workflows/release.yml`) は `pull_request` と
`workflow_dispatch` でも 7 target の `build-wheels` と `build-sdist` を回す
(GitHub Release と PyPI publish は tag push のときだけ)。検証用の workflow は足さず、
次のどちらかで回す:

- PoC branch から draft PR を開く (main には merge しない)。
- `gh workflow run release.yml --ref <poc-branch>` で手動実行する。

wheel は default feature のまま build されるので、PoC branch では ruff backend を
default にする (`default = ["parser-ruff"]`) か、backend を直接差し替えた commit を
積んでから回す。

結果の取り方:

```bash
# build 時間: job ごとの開始・終了時刻
gh run view <run-id> --json jobs --jq '.jobs[] | [.name, .startedAt, .completedAt] | @tsv'
# wheel サイズ: artifact の size (retention は 1 日なので当日中に取る)
gh api repos/watany-dev/chokkin/actions/runs/<run-id>/artifacts \
  --jq '.artifacts[] | [.name, .size_in_bytes] | @tsv'
```

main の直近 run に同じ操作をして baseline にする。

### 判定基準

| 軸 | 合格 | 不合格時 |
|---|---|---|
| 7 target の wheel | 全 target で build 成功 | 失敗 target、原因 (特に `stacker` → `psm` の asm build script と musl / aarch64 の cross toolchain)、回避策の有無を記録。回避策が無ければ ADR の改訂を reopen |
| sdist | `build-sdist` が成功し、sdist から `uvx maturin build` できる | git 依存を使った場合は、build 時に GitHub へ到達できる必要がある点を記録 |
| wheel サイズ | target ごとの増減を記録 (閾値は置かない) | — |
| build 時間 | target ごとの増減を記録。60 分の job timeout に余裕がある | timeout に近ければ ADR に記録 |
| `cargo deny` | CI の `Security (cargo-deny)` が通る。git 依存なら `deny.toml` に `allow-git` を足した上で通る | 出た advisory / license を記録 |
| `cargo audit` | `Security Audit` workflow (手動実行可) が通る | 同上 |

## 検証用 workflow を足さない理由

既存の `Release` workflow が PR と手動実行で全 target を build するので、検証専用の
workflow は同じ matrix の複製になる。複製は pin した action の更新や matrix の変更で
ずれていくため、足さない。
