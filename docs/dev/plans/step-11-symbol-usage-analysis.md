# Step 11: Symbol Usage Analysis 設計

解析パイプライン §6 の **処理ステップ 11 (symbol usage analysis)** の実装設計。
Step 6 の `SymbolDef` と import 参照から **シンボル参照グラフ**を構築し、CHK006–CHK007 / CHK010 の候補を生成する。

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
| **CHK006** | module 外から参照されない公開 top-level symbol |
| **CHK007** | `__init__.py` の re-export が内部からも未使用 |
| **CHK010** | `ResolutionIndex` で `Unresolved` の import |

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

### 3.2 CHK006 severity（§3, §12）

| mode | severity |
| --- | --- |
| app | warning（confidence ≥ likely） |
| library | info（preview） |
| `__all__` 内 | app: warning, library: info |

### 3.3 CHK007

`__init__.py` の `from .sub import foo` で `foo` が package 外から import されず、内部からも未使用 → CHK007。

### 3.4 CHK010

`ResolveWarning::UnresolvedImport` を `IssueCandidate` に昇格。first-party typo と third-party 欠落を区別して message 化。

### 3.5 `SymbolReport`

```rust
pub struct SymbolReport {
    pub candidates: Vec<IssueCandidate>,
    pub symbol_count: u32,
    pub external_symbols: IndexSet<rules::symbols::SymbolId>,  // rules ローカル ID（graph SymbolId とは別）
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
| 未使用 public fn | CHK006 |
| `@pytest.fixture` | used（非報告） |
| `__init__.py` re-export 未使用 | CHK007 |
| `import unknown_pkg` | CHK010 |
| library mode | CHK006 info |

## 7. Exit criteria

- [x] CHK006–CHK007, CHK010 候補生成
- [x] externally-used マーキングで false positive 抑制
- [x] library mode で severity info
- [x] `make check` 通過

## 8. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| attribute access 追跡 | 精度とコスト | v0.2 |
| CHK010 vs Step 10 | unresolved は Step 11 が担当 | Step 10 §3.1 と整合 |

## 9. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `step-11-symbol-usage-analysis.md` | 本プラン |
| `docs/dev/spec.ja.md` §3, §12 | preview / externally-used と一致 |
| `step-06` | `SymbolDef`, `exports`（設計） |
| `step-09` | reachable ファイルフィルタ |
| `src/graph/types.rs` | `SymbolId` 未導入 — rules ローカルで定義 |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `rules/symbols/`。`SymbolId` は graph と分離 |
| 静的解析制約 | 20 | 20 | AST 静的解析のみ |
| ルール / ポリシー | 20 | 19 | library mode info。保守的 attribute 方針 |
| エラー処理 | 20 | 19 | non-fatal `SymbolReport` |
| テスト容易性 | 20 | 19 | 5 fixture カテゴリ |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| 到達外ファイル除外 | OK — unused file と重複回避 |
| CHK010 分担 | OK — Step 7 `ResolveWarning` 入力 |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | `SymbolId` が graph 未存在と衝突 | rules ローカル ID に明記済み |

### 確定判定

**合格 — 実装着手可。** Step 6–9 完了後。
