# Step 11: Symbol Usage Analysis 設計

解析パイプライン §6 の **処理ステップ 11 (symbol usage analysis)** の実装設計。
Step 6 の `SymbolDef` と import 参照から **シンボル参照グラフ**を構築し、YOK006–YOK007 / YOK010 の候補を生成する。

> **関連プラン**
>
> - [`step-06-python-parse.md`](./step-06-python-parse.md) — `SymbolDef`, `exports`
> - [`step-09-reachability-analysis.md`](./step-09-reachability-analysis.md) — reachable ファイルのみ対象
> - [`step-12-issue-emission.md`](./step-12-issue-emission.md) — preview 扱い・severity 調整

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | module 外から参照されない **公開シンボル**と未解決 import を検出する |
| 成果物 | `analyze_symbols(...) -> SymbolReport` |
| Phase 0 / 1 との関係 | v0.1 — `unused_export` は **preview**（library mode は info） |
| 後続ステップへの入力 | Step 12 |

## 2. スコープ

### In scope（v0.1）

| Rule | 内容 |
| --- | --- |
| **YOK006** | module 外から参照されない公開 top-level symbol |
| **YOK007** | `__init__.py` の re-export が内部からも未使用 |
| **YOK010** | `ResolutionIndex` で `Unresolved` の import |

**公開 symbol の定義（§12）:**

```text
top-level function / class / constant
__all__ に列挙された名前
__init__.py での re-export（from .x import y as y）
_  prefix は除外（__all__ 明示時を除く）
```

**externally-used 扱い（参照がなくても used）:**

```text
decorator リスト一致（Step 6）
entry point の symbol 指定（EntrySpec.symbol）
manifest entry_points ターゲット
plugin SymbolReference（uvicorn app:application）
pytest fixture 装飾子
```

### Out of scope

| 項目 | 担当 |
| --- | --- |
| 関数本体削除 fix | Step 13 — v1 まで非対応 |
| cross-module 型注釈の深い解決 | 名前の文字列一致のみ |
| method / ネスト関数 | 対象外 |

## 3. 仕様との対応

### 3.1 参照グラフ

```text
SymbolNode(module, name)
  ← FileReferencesSymbol（同一 file 内使用）
  ← ImportBinding（from pkg import name）
  ← AttributeAccess（pkg.sym — v0.1 は from-import のみ厳密）
```

v0.1 は **保守的**:

- `from acme.utils import helper` → `acme.utils.helper` used
- `import acme.utils; acme.utils.helper()` → `acme.utils` のみ used（helper は未検出 — confidence Likely 上限）

### 3.2 YOK006 severity（§3, §12）

| mode | severity |
| --- | --- |
| app | warning（confidence ≥ likely） |
| library | info（preview） |
| `__all__` 内 | app: warning, library: info |

### 3.3 YOK007

`__init__.py` の `from .sub import foo` で `foo` が package 外から import されず、内部からも未使用 → YOK007。

### 3.4 YOK010

`ResolveWarning::UnresolvedImport` を `IssueCandidate` に昇格。first-party typo と third-party 欠落を区別して message 化。

### 3.5 `SymbolReport`

```rust
pub struct SymbolReport {
    pub candidates: Vec<IssueCandidate>,
    pub symbol_count: u32,
    pub external_symbols: IndexSet<SymbolId>,
}
```

## 4. モジュール構成

```
src/rules/symbols/
  mod.rs
  analyze.rs
  graph.rs          # SymbolId, 参照辺
  exports.rs        # __all__ / re-export
  external.rs       # decorator / entry マーキング
```

## 5. API

```rust
pub fn analyze_symbols(
    parse: &ParseSummary,
    resolution: &ResolutionIndex,
    reachability: &ReachabilityReport,
    entry: &EntryPlan,
    plugins: &PluginHints,
    mode: &ResolvedMode,
) -> SymbolReport;
```

**到達外ファイル**の symbol は解析しない（unused file と重複を避ける）。

## 6. テスト計画

| fixture | Rule |
| --- | --- |
| 未使用 public fn | YOK006 |
| `@pytest.fixture` | used（非報告） |
| `__init__.py` re-export 未使用 | YOK007 |
| `import unknown_pkg` | YOK010 |
| library mode | YOK006 info |

## 7. Exit criteria

- [ ] YOK006–YOK007, YOK010 候補生成
- [ ] externally-used マーキングで false positive 抑制
- [ ] library mode で severity info
- [ ] `make check` 通過

## 8. update-plan 検証サマリ（確定）

| **合計** | **96 — 合格** |

**合格 — 実装着手可。** Step 6–9 完了後。
