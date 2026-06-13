# Step 12: Issue Emission 設計

解析パイプライン §6 の **処理ステップ 12 (issue emission)** の実装設計。
Step 9–11 の候補と YOK001 を **統合・フィルタ・ignore** し、最終 `Issue` 列と exit code 判定を行う。
v0.1 reporter（human / compact / JSON / Markdown）の **入力契約**も定義する（描画は Phase 1 CLI）。

> **関連プラン**
>
> - [`step-09-reachability-analysis.md`](./step-09-reachability-analysis.md) — YOK001
> - [`step-10-dependency-reconciliation.md`](./step-10-dependency-reconciliation.md) — YOK002–YOK009
> - [`step-11-symbol-usage-analysis.md`](./step-11-symbol-usage-analysis.md) — YOK006–YOK007, YOK010
> - [`phase-1-cli-reporter.md`](./phase-1-cli-reporter.md) — CLI 描画・`--explain` / `--trace`

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | 全 rule の issue を **一貫した型**で出力し、confidence / severity / ignore / exit code を適用する |
| 成果物 | `emit_issues(...) -> IssueReport` |
| Phase 0 / 1 との関係 | v0.1 MVP のユーザー可視出力の根幹 |
| 後続ステップへの入力 | Phase 1 CLI reporter、Step 13 fix |

## 2. スコープ

### In scope（v0.1）

- **YOK001** — `ReachabilityReport.unreachable` から生成
- Step 10–11 の `IssueCandidate` 統合
- **ignore** 適用（§18）:
  - `[tool.yokei.ignore]` glob
  - inline `# yokei: ignore[YOK00x]`
  - file-level `# yokei: file-ignore[...]`
- **confidence フィルタ** — `config.confidence` + `--confidence` override
- **strict モード** — maybe 表示、marker unused を error 昇格（Step 10 と連携）
- **`--include` / `--exclude`** rule セット（CLI 型のみ定義、Phase 1 で配線）
- `IssueReport` — 最終 issue 列 + summary 統計
- **exit code 判定** — §2: error かつ confidence ≥ likely → exit 1
- `ExplainData` / `TracePath` の保持（`--explain` / `--trace`）

### Out of scope

| 項目 | 担当 |
| --- | --- |
| stdout フォーマット | Phase 1 CLI `reporters/` |
| SARIF / GitHub reporter | v0.2 |
| baseline 抑制 | v0.2 §18 |

## 3. 仕様との対応

### 3.1 `Issue` 型

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub rule: RuleId,
    pub severity: Severity,
    pub confidence: IssueConfidence,
    pub message: String,
    pub location: IssueLocation,
    pub subject: IssueSubject,
    pub explain: Option<ExplainData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLocation {
    pub file: Option<String>,   // root-relative
    pub line: Option<u32>,
    pub manifest: Option<DependencyOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueSubject {
    File { path: String },
    Distribution { name: String },
    Symbol { module: String, name: String },
    Binary { name: String },
    Import { module: String, file: String, line: u32 },
}
```

### 3.2 RuleId（§3）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    Yok001, Yok002, Yok003, Yok004, Yok005,
    Yok006, Yok007, Yok008, Yok009, Yok010,
}
```

表示は `YOK001` 形式。`parse_rule_id("YOK002")` for `--explain`.

### 3.3 confidence と severity（§11）

独立 2 軸:

```text
表示: confidence >= config.confidence_floor
exit 1: severity >= Error AND confidence >= Likely（default）
        strict: severity >= Warning AND confidence >= Maybe
--no-exit-code: 常に 0（issue があっても）。config error は 2 維持
```

### 3.4 ignore マッチ（§18）

| 種別 | マッチタイミング |
| --- | --- |
| config `ignore.YOK002 = ["boto3"]` | distribution 名 glob |
| config `YOK001 = ["legacy/**"]` | path glob |
| config `YOK006 = ["src/pkg.py:*"]` | `path:symbol_glob` |
| inline | 同一行の import / def |
| file-ignore | ファイル内全 rule |

ignored issue は `IssueReport.suppressed` に記録（`--debug` 用、v0.1 optional）。

### 3.5 YOK001 生成

```rust
fn yok001_from_unreachable(u: &UnreachableFile, mode: &ResolvedMode) -> Option<IssueCandidate>
```

- `ExcludedInit` 等は `None`
- library mode → confidence Maybe → デフォルト非表示
- app + NotReachable → confidence Certain

### 3.6 `IssueReport`

```rust
pub struct IssueReport {
    pub issues: Vec<Issue>,
    pub suppressed: Vec<SuppressedIssue>,
    pub summary: IssueSummary,
    pub exit_status: ExitStatus,
}

pub struct IssueSummary {
    pub by_rule: BTreeMap<RuleId, u32>,
    pub total: u32,
}
```

## 4. モジュール構成

```
src/
  rules/
    emit.rs           # emit_issues
    ignore.rs         # IgnoreMatcher
    filter.rs         # confidence / include / exclude
    yok001.rs
  reporters/
    mod.rs
    types.rs          # ReporterId, RenderContext
    traits.rs         # trait Reporter { fn render(...) }
    default.rs        # human-readable（Phase 1 で実装）
    compact.rs
    json.rs
    markdown.rs
```

Step 12 PR では `reporters/` は **trait + types のみ**。default 実装は Phase 1 CLI PR。

## 5. API

```rust
pub fn emit_issues(
    unreachable: &ReachabilityReport,
    deps: &DependencyReport,
    symbols: &SymbolReport,
    parse: &ParseSummary,
    config: &YokeiConfig,
    overrides: &RuntimeOverrides,
    mode: &ResolvedMode,
) -> IssueReport;
```

```rust
pub fn explain_issue(report: &IssueReport, selector: &str) -> Option<String>;
// selector: "YOK002:boto3" or "YOK001:src/legacy.py"
```

## 6. JSON reporter スキーマ（v0.1 draft）

```json
{
  "version": "0.1.0",
  "project": "acme-api",
  "mode": "app",
  "issues": [
    {
      "code": "YOK002",
      "severity": "error",
      "confidence": "certain",
      "message": "...",
      "file": null,
      "distribution": "boto3",
      "manifest": { "file": "pyproject.toml", "line": 18 }
    }
  ],
  "summary": { "total": 10, "by_code": { "YOK002": 3 } }
}
```

v1.0 まで schema は breaking 可 — `version` フィールドで識別。

## 7. テスト計画

- ignore glob マッチ単体
- confidence フィルタ
- strict 昇格
- inline ignore で YOK003 抑制
- exit_status: error issue あり → 1、`--no-exit-code` → 0
- `explain_issue` ゴールデン

## 8. Exit criteria

- [ ] 全 YOK001–YOK010 が `emit_issues` 経由で出る（YOK006 preview 含む）
- [ ] ignore 3 種が動作
- [ ] `IssueReport.exit_status` が §2 準拠
- [ ] `Reporter` trait と JSON 型定義
- [ ] `make check` 通過

## 9. update-plan 検証サマリ（確定）

| **合計** | **97 — 合格** |

**合格 — 実装着手可。** Step 9–11 完了後。reporter 描画は Phase 1 と並行可。
