# Step 9: Reachability Analysis 設計

解析パイプライン §6 の **処理ステップ 9 (reachability analysis)** の実装設計。
Step 8 の entry roots から BFS で **到達可能ファイル集合**を構築し、YOK001 と `--trace` の根拠データを供給する。

> **関連プラン**
>
> - [`step-08-entry-root-construction.md`](./step-08-entry-root-construction.md) — `EntryPlan` 入力
> - [`step-07-import-resolution.md`](./step-07-import-resolution.md) — `FileImportsModule` / first-party 解決
> - [`step-05-config-plugin-extraction.md`](./step-05-config-plugin-extraction.md) — `ModuleReference`, `FrameworkUsedGlob`
> - [`step-12-issue-emission.md`](./step-12-issue-emission.md) — YOK001 判定・confidence

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | entry から import / config 参照を辿り **どの `.py` が実行経路上にあるか**を静的に決定する |
| 成果物 | `analyze_reachability(...) -> Result<ReachabilityReport, ReachabilityError>` |
| Phase 0 / 1 との関係 | v0.1 MVP 必須。unused files の中核 |
| 後続ステップへの入力 | Step 10–12（used distributions / YOK001 / trace） |

## 2. スコープ

### In scope（v0.1）

- **BFS**（幅優先）で entry → file の到達閉包
- 辺の種類（§6 内部モデル）:

```text
Entry reaches File          # Step 8
File imports Module         # Step 6–7
Module resolves File        # first-party module → .py path（本ステップで解決）
ConfigReference uses Module # Step 5 plugin module refs
File reaches File           # 同一 package 内の暗黙参照（__init__.py 経由は v0.1 簡易）
```

- `ModuleReference`（plugin）からの **module → file** 到達
- `FrameworkUsedGlob` 一致ファイルを **reachable 扱い**（unused file 候補から除外）
- **到達パス記録**（`--trace` 用。ノード列を保持）
- `production` 時は test context ファイルを project files から除外（Step 4 と二重 — ここでは reachable 計算対象外）
- dynamic import（literal）経由の module もキューに追加
- `ReachabilityReport`: reachable / unreachable / framework-used / excluded

### Out of scope

| 項目 | 担当 |
| --- | --- |
| YOK001 issue 生成 | Step 12 |
| `petgraph` 導入 | v0.1 は自前 BFS + `HashMap` 隣接リスト |
| wildcard / `import *` の展開 | 非対応 — confidence 低下は Step 12 |
| workspace member 別 BFS | v0.2 |
| parallel BFS | §19 — 単一スレッドで十分なら後回し |

## 3. 仕様との対応

### 3.1 first-party module → file 解決

`DiscoveredSources` + `LayoutInfo` からインデックスを構築:

```text
acme              → src/acme/__init__.py
acme.api          → src/acme/api/__init__.py  or  src/acme/api.py
acme.api.routes   → src/acme/api/routes.py
```

**規則（優先順）:**

1. `<pkg>/<mod path>.py`
2. `<pkg>/<mod path>/__init__.py`
3. namespace fragment（`pkg/sub` がディレクトリのみ）→ マッチなしは `ModuleResolution::NamespaceFragment`

flat layout も同様（package root 直下）。

### 3.2 BFS アルゴリズム

```text
queue ← entry root files（EntryPlan.roots → path → FileId）
reachable ← ∅
predecessors ← map FileId → (from, edge_kind)

while queue not empty:
  f ← pop
  if f ∈ reachable: continue
  reachable.add(f)

  for each FileImportsModule(f, m):
    if m is first-party:
      f2 ← resolve_module_to_file(m)
      if f2: enqueue f2; record File reaches File via import
    # third-party / stdlib: ファイル到達は追加しない（dependency 使用は Step 10）

  for each ConfigReference uses Module(m) from plugins:
    f2 ← resolve_module_to_file(m)
    if f2: enqueue f2

  for each DynamicImport literal in file f:
    同上（first-party のみファイルキュー）
```

third-party import は **distribution 使用**として `UsedDependencyIndex`（Step 10）へ渡すが、ファイルキューには入れない。

### 3.3 除外・低 confidence 入力（§11）

到達しなかったファイルでも、以下は Step 12 へ **除外理由**を渡す:

| 条件 | 扱い |
| --- | --- |
| `__init__.py` のみ | 除外（YOK001 対象外） |
| `*.pyi` | 除外 |
| `migrations/**` | framework-used（plugin）または除外 |
| `tests/**` | test context — app mode のみ候補 |
| `FrameworkUsedGlob` 一致 | reachable 扱い |
| `has_opaque_dynamic_import` かつ module 名不一致 | confidence Likely 上限 |
| library mode | confidence Maybe 上限 |

### 3.4 `ReachabilityReport`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityReport {
    pub reachable: IndexSet<FileId>,
    pub unreachable: Vec<UnreachableFile>,
    pub used_modules: Vec<UsedModule>,      // third-party/stdlib 含む — Step 10 入力
    pub framework_used: IndexSet<FileId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableFile {
    pub file: FileId,
    pub path: String,
    pub reasons: Vec<UnreachableReason>,   // ExcludedInit, NotReachable, ...
    pub max_confidence: IssueConfidence,   // Step 12 へ
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracePath {
    pub target: FileId,
    pub steps: Vec<TraceStep>,             // entry → … → target
}
```

`trace_to_file(report, file_id) -> Option<TracePath>` を同モジュールで提供（Step 12 / CLI）。

## 4. モジュール構成

```
src/
  reachability/
    mod.rs
    types.rs
    error.rs
    module_index.rs    # module dotted name → FileId
    build.rs           # analyze_reachability
    bfs.rs             # キュー・predecessor
    trace.rs           # trace_to_file
```

graph 型拡張（Step 9）:

```rust
FileReachesFile { from: FileId, to: FileId, via: FileReachVia }
ConfigReferenceUsesModule { origin: ReferenceOrigin, module: ModuleId }
```

## 5. API

```rust
pub fn analyze_reachability(
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
    entry: &EntryPlan,
    plugins: &PluginHints,
    parse: &ParseSummary,
    mode: &ResolvedMode,
) -> Result<ReachabilityReport, ReachabilityError>;
```

## 6. テスト計画

| fixture | 期待 |
| --- | --- |
| chain import `main → a → b` | a, b reachable |
| orphan `legacy.py` | unreachable |
| plugin `INSTALLED_APPS` ref | settings 未到達でも app module reachable |
| Django migrations glob | framework-used |
| dynamic `import_module("acme.plugins")` | plugin module reachable |
| library mode | orphan に Maybe confidence |

## 7. Exit criteria

- [ ] BFS が entry から全 first-party 連鎖を辿る
- [ ] `trace_to_file` が最短経路を返す
- [ ] `FrameworkUsedGlob` が反映される
- [ ] `make check` 通過
- [ ] `update-docs` で `reachability/` を §15 に追記

## 8. 実装順序

```text
1. reachability/module_index.rs
2. reachability/bfs.rs
3. graph edge 拡張
4. reachability/build.rs
5. reachability/trace.rs
6. fixtures + tests
7. make check
```

## 9. update-plan 検証サマリ（確定）

| カテゴリ | 得点 | 所見 |
| --- | ---: | --- |
| モジュール設計 | 19 | BFS 単独 crate。graph は辺の SoT |
| 静的解析制約 | 20 | 実行なし |
| ルール / ポリシー | 19 | YOK001 は Step 12。confidence 入力を渡す |
| エラー処理 | 20 | 未解決 module は skip + diagnostic |
| テスト容易性 | 19 | chain / orphan / plugin fixture |
| **合計** | **97** | **合格** |

**合格 — 実装着手可。** Step 7–8 完了後。
