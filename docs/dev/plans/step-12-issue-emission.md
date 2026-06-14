# Step 12: Issue Emission 設計

解析パイプライン §6 の **処理ステップ 12 (issue emission)** の実装設計。
Step 9–11 の候補と CHK001 を **統合・フィルタ・ignore** し、最終 `Issue` 列と exit code 判定を行う。
v0.1 reporter（human / compact / JSON / Markdown）の **入力契約**も定義する（描画は Phase 1 CLI）。

> **関連プラン**
>
> - [`step-09-reachability-analysis.md`](./step-09-reachability-analysis.md) — CHK001
> - [`step-10-dependency-reconciliation.md`](./step-10-dependency-reconciliation.md) — CHK002–CHK009
> - [`step-11-symbol-usage-analysis.md`](./step-11-symbol-usage-analysis.md) — CHK006–CHK007, CHK010
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

- **CHK001** — `ReachabilityReport.unreachable` から生成
- Step 10–11 の `IssueCandidate` 統合
- **ignore** 適用（§18）:
  - `[tool.chokkin.ignore]` glob
  - inline `# chokkin: ignore[CHK00x]`
  - file-level `# chokkin: file-ignore[...]`
- **confidence フィルタ** — `config.confidence` + `RuntimeOverrides.confidence_floor`
- **strict モード** — `RuntimeOverrides.strict` — maybe 表示、marker unused を error 昇格（Step 10 と連携）
- **`--no-exit-code`** — `RuntimeOverrides.no_exit_code`（Phase 1 CLI で型追加）
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
    pub confidence: Confidence,   // `crate::config::Confidence`
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
    Chk001, Chk002, Chk003, Chk004, Chk005,
    Chk006, Chk007, Chk008, Chk009, Chk010,
}
```

表示は `CHK001` 形式。`parse_rule_id("CHK002")` for `--explain`.

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
| config `ignore.CHK002 = ["boto3"]` | distribution 名 glob |
| config `CHK001 = ["legacy/**"]` | path glob |
| config `CHK006 = ["src/pkg.py:*"]` | `path:symbol_glob` |
| inline | 同一行の import / def |
| file-ignore | ファイル内全 rule |

ignored issue は `IssueReport.suppressed` に記録（`--debug` 用、v0.1 optional）。

### 3.5 CHK001 生成

```rust
fn chk001_from_unreachable(u: &UnreachableFile, mode: &ResolvedMode) -> Option<IssueCandidate>
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
    chk001.rs
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
    config: &ChokkinConfig,
    overrides: &RuntimeOverrides,
    mode: &ResolvedMode,
) -> IssueReport;
```

```rust
pub fn explain_issue(report: &IssueReport, selector: &str) -> Option<String>;
// selector: "CHK002:boto3" or "CHK001:src/legacy.py"
```

## 6. JSON reporter スキーマ（v0.1 draft）

```json
{
  "version": "0.1.0",
  "project": "acme-api",
  "mode": "app",
  "issues": [
    {
      "code": "CHK002",
      "severity": "error",
      "confidence": "certain",
      "message": "...",
      "file": null,
      "distribution": "boto3",
      "manifest": { "file": "pyproject.toml", "line": 18 }
    }
  ],
  "summary": { "total": 10, "by_code": { "CHK002": 3 } }
}
```

v1.0 まで schema は breaking 可 — `version` フィールドで識別。

## 7. テスト計画

- ignore glob マッチ単体
- confidence フィルタ
- strict 昇格
- inline ignore で CHK003 抑制
- exit_status: error issue あり → 1、`--no-exit-code` → 0
- `explain_issue` ゴールデン

## 8. Exit criteria

- [ ] 全 CHK001–CHK010 が `emit_issues` 経由で出る（CHK006 preview 含む）
- [ ] ignore 3 種が動作
- [ ] `IssueReport.exit_status` が §2 準拠
- [ ] `Reporter` trait と JSON 型定義
- [ ] `make check` 通過

## 9. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| `suppressed` の CLI 表示 | ノイズ | `--debug` のみ（Phase 1 optional） |
| JSON schema 凍結 | v1.0 まで可変 | `version` フィールドで識別 |

## 10. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `step-12-issue-emission.md` | 本プラン |
| `docs/dev/spec.ja.md` §2, §3, §11, §18 | exit code / ignore / confidence 2 軸 |
| `src/config/types.rs` | `Confidence`, `ignore` 読み込み済み。マッチは本 PR |
| `src/lib.rs` | `ExitStatus` 確定済み |
| `step-09`–`11` | 候補型 `IssueCandidate` |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `rules/emit` + `reporters/` trait のみ |
| 静的解析制約 | 20 | 20 | 変更なし |
| ルール / ポリシー | 20 | 20 | 全 CHK001–010。§18 ignore 3 種 |
| エラー処理 | 20 | 19 | `emit_issues` non-fatal。exit は `IssueReport` |
| テスト容易性 | 20 | 19 | ignore / exit / explain テスト |
| **合計** | **100** | **97** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| `ExitStatus` と §2 | OK |
| `Confidence` 命名 | OK — `IssueConfidence` は不採用 |
| reporter 描画境界 | OK — Phase 1 CLI |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | `RuntimeOverrides` に `no_exit_code` 未定義 | Phase 1 で拡張と明記済み |
| **P1** | 検証サマリが簡略 | 本セクションで補完 |

### 確定判定

**合格 — 実装着手可。** Step 9–11 完了後。reporter 描画は Phase 1 と並行可。
