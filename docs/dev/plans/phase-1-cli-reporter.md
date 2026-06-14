# Phase 1: CLI & Reporter 設計

v0.1 MVP の **CLI 統合**と **reporter 実装**の設計。
[`phase-0-cli-vertical-slice.md`](./phase-0-cli-vertical-slice.md) の `probe_project` を
[`step-12-issue-emission.md`](./step-12-issue-emission.md) まで伸ばした **フル分析 CLI** を定義する。

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | README / §2 が示す `uvx chokkin` 体験（flags / 出力 / exit code）を完成させる |
| 成果物 | `analyze_project(...) -> Result<AnalysisReport, AnalyzeError>` + `clap` CLI + 4 reporters |
| 依存 | Steps 5–13 の library API |

## 2. スコープ

### In scope（v0.1）

**CLI flags（§2）:**

```text
uvx chokkin [PATH]
  --production
  --strict
  --no-exit-code
  --include <rules>        # comma-separated CHK00x
  --exclude <rules>
  --reporter default|compact|json|markdown
  --confidence certain|likely|maybe
  --explain CHK002:boto3
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
| `chokkin --init` | v0.1 後半 |
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
  main.rs         # デフォルトは analyze。`--probe` で Phase 0 互換
```

**移行方針:** Phase 1 マージ後、`chokkin` 引数なしは `analyze_project` を呼ぶ。Steps 5–12 未実装期間は feature flag または compile-time で `probe` にフォールバックしない — **Phase 0 CLI を先にマージ**してから Phase 1 を繋ぐ。

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
chokkin 0.1.0

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

### 6.1 `RuntimeOverrides` 拡張（Step 2 型の拡張）

Phase 0 では `production` / `strict` / `confidence_floor` のみ。Phase 1 CLI PR で `src/config/types.rs` を拡張:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    pub production: Option<bool>,
    pub strict: Option<bool>,
    pub confidence_floor: Option<Confidence>,
    pub no_exit_code: Option<bool>,
    pub include_rules: Option<Vec<RuleId>>,   // Step 12 filter へ
    pub exclude_rules: Option<Vec<RuleId>>,
    pub reporter: Option<ReporterId>,
    pub fix: bool,
    pub fix_dry_run: bool,
    pub explain: Option<String>,
    pub trace: Option<String>,
}
```

`RuleId` / `ReporterId` は `rules::types` / `reporters::types` から型エイリアスまたは `String` 受け口（循環参照回避のため CLI 層で parse → enum 変換）。

## 7. Exit criteria（Phase 1 CLI）

- [x] `uvx chokkin` がサンプル fixture で §2 形式の出力
- [x] 4 reporter 切替
- [x] `--explain` / `--trace` 動作
- [x] `--fix` で pyproject 編集（Step 13 連携）
- [x] exit code §2 準拠
- [x] `make check` 通過
- [x] dogfooding 用 `scripts/run-oss-fixture.sh` 骨格（`make oss-fixtures`）

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

## 9. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| `clap` と手動パース共存期間 | Phase 0 → 1 移行 | Phase 1 で clap に一本化 |
| OSS dogfooding 20 件 | §17 exit | スクリプト骨格のみ v0.1 |

## 10. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `phase-1-cli-reporter.md` | 本プラン |
| `docs/dev/spec.ja.md` §2, §16, §17 | flags / reporter / exit criteria |
| `phase-0-cli-vertical-slice.md` | `probe_project` 前提 |
| `step-12` | `IssueReport`, `Reporter` trait |
| `src/main.rs` | Phase 1 CLI — `analyze_project` デフォルト、`--probe` 互換 |
| `src/config/types.rs` | `RuntimeOverrides` 拡張必要（§6.1） |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `pipeline/analyze` + `reporters/` |
| 静的解析制約 | 20 | 20 | オーケストレーションのみ |
| ルール / ポリシー | 20 | 19 | exit code / reporter §2 準拠 |
| エラー処理 | 20 | 19 | `AnalyzeError` で step 別ラップ |
| テスト容易性 | 20 | 19 | CLI + golden + fixture |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| パイプライン順 1–13 | OK |
| Phase 0 probe との共存 | OK — `--probe` / 段階マージ方針を明記 |
| `AGENTS.md` pre-alpha 文言 | Phase 1 完了時 `update-docs` |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | `RuntimeOverrides` 不足 | §6.1 で拡張フィールド定義 |
| **P1** | analyze デフォルトと未実装ステップ | Phase 0 先行マージを明記 |

### 確定判定

**合格 — 実装着手可。** Step 12 完了後。probe CLI は Phase 0 と独立に先行可能。
