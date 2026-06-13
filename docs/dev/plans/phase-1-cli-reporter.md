# Phase 1: CLI & Reporter 設計

v0.1 MVP の **CLI 統合**と **reporter 実装**の設計。
[`phase-0-cli-vertical-slice.md`](./phase-0-cli-vertical-slice.md) の `probe_project` を
[`step-12-issue-emission.md`](./step-12-issue-emission.md) まで伸ばした **フル分析 CLI** を定義する。

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | README / §2 が示す `uvx yokei` 体験（flags / 出力 / exit code）を完成させる |
| 成果物 | `analyze_project(...) -> Result<AnalysisReport, AnalyzeError>` + `clap` CLI + 4 reporters |
| 依存 | Steps 5–13 の library API |

## 2. スコープ

### In scope（v0.1）

**CLI flags（§2）:**

```text
uvx yokei [PATH]
  --production
  --strict
  --no-exit-code
  --include <rules>        # comma-separated YOK00x
  --exclude <rules>
  --reporter default|compact|json|markdown
  --confidence certain|likely|maybe
  --explain YOK002:boto3
  --trace src/acme/legacy.py
  --fix
  --fix --dry-run
  --project-root <PATH>
  -h, --version
```

**パイプラインオーケストレーション:**

```text
1–4  probe（既存）
5    extract_plugin_hints
6    parse_project_sources
7    resolve_imports + apply_resolution_to_graph
8    build_entry_roots + apply_entry_plan
9    analyze_reachability
10   reconcile_dependencies
11   analyze_symbols
12   emit_issues
13   apply_fixes（--fix 時のみ）
```

**Reporters:**

| ID | 用途 |
| --- | --- |
| `default` | §2 サンプル形式（human、グループ別） |
| `compact` | 1 行 1 issue |
| `json` | Step 12 スキーマ |
| `markdown` | CI summary / PR コメント向け |

**`--explain`:** `IssueReport` から `ExplainData` を整形出力（stderr 推奨）

**`--trace`:** `trace_to_file` の `TraceStep` を木形式で表示

### Out of scope

| 項目 | 時期 |
| --- | --- |
| `yokei --init` | v0.1 後半 |
| SARIF / GitHub reporter | v0.2 |
| `--timings` | §19 チューニング時 |
| cache | v0.2 |

## 3. モジュール構成

```
src/
  pipeline/
    probe.rs      # 既存
    analyze.rs    # analyze_project — 全ステップ
    error.rs      # 拡張
  cli.rs          # clap Command
  reporters/
    default.rs
    compact.rs
    json.rs
    markdown.rs
  main.rs         # probe vs analyze（--probe hidden で Phase 0 互換）
```

## 4. `analyze_project` API

```rust
pub struct AnalysisReport {
    pub probe: ProbeReport,
    pub graph: ProjectGraph,
    pub issues: IssueReport,
    pub fix: Option<FixReport>,
}

pub fn analyze_project(
    start: &Path,
    cli: &CliArgs,
) -> Result<AnalysisReport, AnalyzeError>;
```

## 5. 出力例（default reporter）

```text
yokei 0.1.0

Project: acme-api
Config : pyproject.toml
Mode   : app, production=false

Unused dependencies  3
  boto3  pyproject.toml:18  ...

Summary: 10 issues
```

`--production` / `Mode` 行は `ResolvedMode` から。

## 6. clap 導入

```toml
# Cargo.toml
clap = { version = "4", features = ["derive", "env"] }
```

`CliArgs` を `clap::Parser` に移行。`RuntimeOverrides` へマッピング。

## 7. Exit criteria（Phase 1 CLI）

- [ ] `uvx yokei` がサンプル fixture で §2 形式の出力
- [ ] 4 reporter 切替
- [ ] `--explain` / `--trace` 動作
- [ ] `--fix` で pyproject 編集（Step 13 連携）
- [ ] exit code §2 準拠
- [ ] `make check` 通過
- [ ] dogfooding 用 `scripts/run-oss-fixture.sh` 骨格（optional）

## 8. 実装順序

```text
1. pipeline/analyze.rs（ステップ配線のみ、reporter は compact）
2. clap CLI
3. reporters/default + json
4. --explain / --trace
5. reporters/compact + markdown
6. --fix 配線
7. OSS fixture dogfooding
8. update-docs + README から pre-alpha 警告を緩和
```

## 9. update-plan 検証サマリ（確定）

| **合計** | **96 — 合格** |

**合格 — 実装着手可。** Step 12 完了後。probe CLI は Phase 0 と独立に先行可能。
