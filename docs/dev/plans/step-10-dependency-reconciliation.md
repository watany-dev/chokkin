# Step 10: Dependency Reconciliation 設計

解析パイプライン §6 の **処理ステップ 10 (dependency reconciliation)** の実装設計。
到達性・import 解決・manifest・plugin binary 使用を照合し、**YOK002–YOK005 / YOK008–YOK009** の issue 候補を生成する。

> **関連プラン**
>
> - [`step-09-reachability-analysis.md`](./step-09-reachability-analysis.md) — `used_modules`
> - [`step-07-import-resolution.md`](./step-07-import-resolution.md) — `TransitiveIndex`
> - [`step-05-config-plugin-extraction.md`](./step-05-config-plugin-extraction.md) — `BinaryUsage`
> - [`step-12-issue-emission.md`](./step-12-issue-emission.md) — 最終 issue 化・ignore

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | 宣言済み依存と **実際の使用**（import / config / binary）の整合性を context 付きで判定する |
| 成果物 | `reconcile_dependencies(...) -> DependencyReport` |
| Phase 0 / 1 との関係 | v0.1 MVP の主要価値（unused / missing deps） |
| 後続ステップへの入力 | Step 12（issue 統合） |

## 2. スコープ

### In scope（v0.1）

| Rule | 判定概要 |
| --- | --- |
| **YOK002** | 宣言あり・使用なし（import / module ref / binary いずれもなし） |
| **YOK003** | import あり・直接宣言なし（lockfile なし or 推移閉包外） |
| **YOK004** | 直接 import あるが推移閉包内のみで直接宣言なし |
| **YOK005** | file context と dependency context の不一致 |
| **YOK008** | binary 使用あるが distribution 未宣言 |
| **YOK009** | 同一 distribution が複数 context に重複宣言 |

**使用の定義（§10）:**

```text
used(distribution D) :=
  ∃ import resolved to D in reachable code with matching context
  ∨ ∃ plugin ModuleReference resolved to D
  ∨ ∃ BinaryUsage resolved to D
  ∨ ∃ config string ref（Step 5）が D の module を指す
```

**context マッピング（§10）:**

| File / import context | 期待する dependency context |
| --- | --- |
| `Runtime` import | `runtime` / optional-extra |
| `Type` import | `type` / `type_groups` |
| `Test` file | `test` / `dev` groups |
| `Docs` file | `docs` |
| Binary dev tool | `dev` / `test` |

### Out of scope

| 項目 | 担当 |
| --- | --- |
| YOK001 / YOK006–YOK007 / YOK010 | Step 11–12 |
| environment marker の実行時評価 | 静的 — marker 付きは confidence 低下（§10） |
| Poetry/PDM lock transitive | v0.2 |
| workspace member 境界 | v0.2 — v0.1 は root manifest 統合 |

## 3. 仕様との対応

### 3.1 YOK003 vs YOK004 優先（§10）

```text
import I from file F
  if I resolves to distribution D:
    if D declared directly in matching context → OK
    else if D in transitive closure of declared deps (lockfile) → YOK004
    else → YOK003
  else if I unresolved → YOK010 (Step 12)
```

lockfile なし: YOK004 を発行せず YOK003 に縮退。message に `no lockfile` を含める。

### 3.2 YOK002 confidence（§10）

| 条件 | confidence |
| --- | --- |
| 通常 unused | Certain |
| environment marker 付き宣言 | Likely（`--strict` で error） |
| opaque / setup.py 由来宣言 | Likely |
| types-* stub のみで runtime 未使用 | 別途 stub 規則 — runtime も未使用なら Certain |

### 3.3 YOK005 例

```text
src/ で pytest を import + pytest が [dependency-groups].dev のみ → misplaced
tests/ のみで requests + requests が [project.dependencies] のみ → OK（test は main 許容）
src/ で requests + requests が test group のみ → misplaced
```

**v0.1 簡略:** test ファイルからの runtime 依存使用は warning なし（test が main dep を使うのは許容）。

### 3.4 YOK009

同一 PEP 508 `name` が `runtime` + `dev` 等に出現 → 重複。保持 context を message に列挙。

### 3.5 `DependencyReport`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DependencyReport {
    pub candidates: Vec<IssueCandidate>,
    pub used_distributions: IndexSet<String>,  // normalized names
    pub diagnostics: Vec<ReconcileDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCandidate {
    pub rule: RuleId,           // YOK002, ...
    pub subject: IssueSubject,  // Distribution | Binary
    pub severity: Severity,
    pub confidence: IssueConfidence,
    pub message: String,
    pub origins: Vec<Origin>,   // manifest line / import line / binary ref
    pub explain: ExplainData,   // Step 12 --explain
}
```

## 4. モジュール構成

```
src/
  rules/
    mod.rs
    types.rs              # RuleId, Severity, IssueCandidate 共有型
    deps/
      mod.rs
      reconcile.rs        # reconcile_dependencies
      used.rs             # UsedDistributionIndex 構築
      unused.rs           # YOK002
      missing.rs          # YOK003, YOK004
      misplaced.rs        # YOK005
      binary.rs           # YOK008
      duplicate.rs        # YOK009
```

Step 11–12 も `rules/` 配下に置き、Step 10 は `rules/deps/` のみ実装。

## 5. API

```rust
pub fn reconcile_dependencies(
    manifest: &LoadedManifest,
    resolution: &ResolutionIndex,
    reachability: &ReachabilityReport,
    plugins: &PluginHints,
    config: &YokeiConfig,
    strict: bool,
) -> DependencyReport;
```

`strict`: `RuntimeOverrides.strict` — marker unused を error、maybe confidence を表示。

## 6. テスト計画

fixture カテゴリ `tests/fixtures/deps/`:

| ケース | Rule |
| --- | --- |
| boto3 宣言のみ | YOK002 |
| import yaml, 宣言なし | YOK003 |
| urllib3 via requests lockfile | YOK004 |
| pytest in src | YOK005 |
| tox.ini pytest 未宣言 | YOK008 |
| requests in main + dev | YOK009 |
| pywin32 marker only | YOK002 Likely |

## 7. Exit criteria

- [ ] YOK002–YOK005, YOK008–YOK009 が候補生成される
- [ ] lockfile あり/なしで YOK003/YOK004 分岐
- [ ] `ExplainData` に used/unused 根拠が入る
- [ ] `make check` 通過

## 8. update-plan 検証サマリ（確定）

| カテゴリ | 得点 |
| --- | ---: |
| モジュール設計 | 19 |
| 静的解析制約 | 20 |
| ルール / ポリシー | 20 |
| エラー処理 | 19 |
| テスト容易性 | 19 |
| **合計** | **97 — 合格** |

**合格 — 実装着手可。** Step 9 + Step 7 完了後。
