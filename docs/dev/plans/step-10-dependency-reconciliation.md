# Step 10: Dependency Reconciliation 設計

解析パイプライン §6 の **処理ステップ 10 (dependency reconciliation)** の実装設計。
到達性・import 解決・manifest・plugin binary 使用を照合し、**CHK002–CHK005 / CHK008–CHK009** の issue 候補を生成する。

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
| **CHK002** | 宣言あり・使用なし（import / module ref / binary いずれもなし） |
| **CHK003** | import あり・直接宣言なし（lockfile なし or 推移閉包外） |
| **CHK004** | 直接 import あるが推移閉包内のみで直接宣言なし |
| **CHK005** | file context と dependency context の不一致 |
| **CHK008** | binary 使用あるが distribution 未宣言 |
| **CHK009** | 同一 distribution が複数 context に重複宣言 |

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
| CHK001 / CHK006–CHK007 / CHK010 | Step 11–12 |
| environment marker の実行時評価 | 静的 — marker 付きは confidence 低下（§10） |
| Poetry/PDM lock transitive | v0.2 |
| workspace member 境界 | v0.2 — v0.1 は root manifest 統合 |

## 3. 仕様との対応

### 3.1 CHK003 vs CHK004 優先（§10）

```text
import I from file F
  if I resolves to distribution D:
    if D declared directly in matching context → OK
    else if D in transitive closure of declared deps (lockfile) → CHK004
    else → CHK003
  else if I unresolved → CHK010 (Step 12)
```

lockfile なし: CHK004 を発行せず CHK003 に縮退。message に `no lockfile` を含める。

### 3.2 CHK002 confidence（§10）

| 条件 | confidence |
| --- | --- |
| 通常 unused | Certain |
| environment marker 付き宣言 | Likely（`--strict` で error） |
| opaque / setup.py 由来宣言 | Likely |
| types-* stub のみで runtime 未使用 | 別途 stub 規則 — runtime も未使用なら Certain |

### 3.3 CHK005 例

```text
src/ で pytest を import + pytest が [dependency-groups].dev のみ → misplaced
tests/ のみで requests + requests が [project.dependencies] のみ → OK（test は main 許容）
src/ で requests + requests が test group のみ → misplaced
```

**v0.1 簡略:** test ファイルからの runtime 依存使用は warning なし（test が main dep を使うのは許容）。

### 3.4 try-import（§10）

```python
try:
    import orjson
except ImportError:
    orjson = None
```

`ImportRef.optional = true` の場合:

```text
orjson が optional extra / main にある → OK
どこにもない → optional_missing（default: info, --strict: warning）
即 CHK003 にしない
```

`optional_missing` は `RuleId` 専用ではなく `IssueCandidate` + `Severity::Info` で表現（Step 12 で filter）。

### 3.5 CHK009

同一 PEP 508 `name` が `runtime` + `dev` 等に出現 → 重複。保持 context を message に列挙。

### 3.6 `DependencyReport`

```rust
// rules/types.rs — Step 10–12 共有。confidence は config::Confidence を re-export
use crate::config::Confidence;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DependencyReport {
    pub candidates: Vec<IssueCandidate>,
    pub used_distributions: IndexSet<String>,  // normalized names
    pub diagnostics: Vec<ReconcileDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCandidate {
    pub rule: RuleId,           // CHK002, ...
    pub subject: IssueSubject,  // Distribution | Binary
    pub severity: Severity,
    pub confidence: Confidence,
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
      unused.rs           # CHK002
      missing.rs          # CHK003, CHK004
      misplaced.rs        # CHK005
      binary.rs           # CHK008
      duplicate.rs        # CHK009
```

Step 11–12 も `rules/` 配下に置き、Step 10 は `rules/deps/` のみ実装。

## 5. API

```rust
pub fn reconcile_dependencies(
    manifest: &LoadedManifest,
    resolution: &ResolutionIndex,
    reachability: &ReachabilityReport,
    plugins: &PluginHints,
    config: &ChokkinConfig,
    strict: bool,
) -> DependencyReport;
```

`strict`: `RuntimeOverrides.strict` — marker unused を error、maybe confidence を表示。

## 6. テスト計画

fixture カテゴリ `tests/fixtures/deps/`:

| ケース | Rule |
| --- | --- |
| boto3 宣言のみ | CHK002 |
| import yaml, 宣言なし | CHK003 |
| urllib3 via requests lockfile | CHK004 |
| pytest in src | CHK005 |
| tox.ini pytest 未宣言 | CHK008 |
| requests in main + dev | CHK009 |
| pywin32 marker only | CHK002 Likely |

## 7. Exit criteria

- [x] CHK002–CHK005, CHK008–CHK009 が候補生成される
- [x] lockfile あり/なしで CHK003/CHK004 分岐
- [x] `ExplainData` に used/unused 根拠が入る
- [x] `make check` 通過

## 8. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| environment marker 評価 | 静的不可 | confidence 低下のみ |
| stub `types-*` 連動 | §10 複雑 | v0.1 は runtime 未使用なら stub も CHK002 |

## 9. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `step-10-dependency-reconciliation.md` | 本プラン |
| `docs/dev/spec.ja.md` §3, §10 | CHK002–005 / 008–009 と一致 |
| `src/manifest/types.rs` | `DeclaredDependency`, `LockfileGraph`, `DependencyContext` |
| `src/config/types.rs` | `Confidence`, `DependencyGroupsConfig` |
| `step-09` | `ReachabilityReport.used_modules` |
| `step-07` | `ResolutionIndex`, `TransitiveIndex` |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `rules/deps/` 分割。共有型は `rules/types.rs` |
| 静的解析制約 | 20 | 20 | lockfile 読み取りのみ。marker 非評価 |
| ルール / ポリシー | 20 | 20 | try-import / CHK003 vs 004 優先を明記 |
| エラー処理 | 20 | 19 | `reconcile_dependencies` は non-fatal（`DependencyReport`） |
| テスト容易性 | 20 | 19 | deps fixture 7 カテゴリ |
| **合計** | **100** | **97** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| CHK010 境界 | OK — unresolved は Step 11 が候補化 |
| `reconcile_dependencies` 戻り値 | OK — 集約ステップは diagnostic 継続 |
| lockfile なし縮退 | OK — §10 準拠 |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | try-import 未記載 | §3.4 追加済み |
| **P1** | `IssueConfidence` 重複 | `config::Confidence` に統一済み |
| **P2** | types-stub 規則 | §8 未決に委譲 |

### 確定判定

**合格 — 実装着手可。** Step 9 + Step 7 完了後。
