# Step 8: Entry Root Construction 設計

解析パイプライン §6 の **処理ステップ 8 (entry root construction)** の実装設計。
config / manifest / plugin / 自動推定から **到達性解析の起点（entry roots）** を構築し、
`mode = auto` の **app / library 解決**を行う。

> **関連プラン**
>
> - [`step-05-config-plugin-extraction.md`](./step-05-config-plugin-extraction.md) — `PluginEntry` 供給
> - [`step-06-python-parse.md`](./step-06-python-parse.md) — `SymbolReference` / parse 由来 entry
> - [`step-07-import-resolution.md`](./step-07-import-resolution.md) — `AppConfig` 正規化
> - Step 9（reachability）— 本ステップの出力を BFS 入力とする（別プラン）

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | zero-config で **どのファイルから到達性を辿るか**を決定し、library / app で unused file 厳しさを切り替える |
| 成果物 | `build_entry_roots(...) -> Result<EntryPlan, EntryError>` |
| Phase 0 / 1 との関係 | v0.1 MVP 必須。YOK001 の前提 |
| 後続ステップへの入力 | Step 9 (reachability)、Step 12 (confidence / mode 表示) |

## 2. スコープ

### In scope（v0.1）

- §8 の **自動 entry 推定**（path 規則・深度制限込み）
- entry ソースのマージ（§13 step-05 予告）

```text
entry_roots = config.entry
            ∪ manifest.entry_points (scripts / gui / entry-points)
            ∪ plugin_hints.entries
            ∪ auto_detected_entries
```

- **path dedup**（同一 `EntrySpec.path` は origin をマージして 1 件）
- `resolve_project_mode` — `mode = auto` 時の app / library 判定（§8）
- `Entry` graph ノード型の導入 + `Entry reaches File` 辺
- `SymbolReference`（`module:symbol`）の **ファイル解決**（Step 7 + layout）
- 欠落 entry パスは `EntryWarning`（Step 4 と重複するが entry 側でも記録）
- `production = true` 時は **test context entry を除外**

### Out of scope

| 項目 | 担当 |
| --- | --- |
| BFS 到達性 | Step 9 |
| `@router` decorator からの implicit entry | Step 6/11 — symbol externally-used |
| workspace member 別 entry 集合 | v0.2 |
| CLI `--trace` 表示 | Phase 1 CLI |

## 3. 仕様との対応

### 3.1 自動 entry 検出（§8）

| パターン | 対象 depth | 備考 |
| --- | --- | --- |
| `__main__.py` | 全階層 | |
| `conftest.py` | 全階層 | test context |
| `main.py`, `app.py`, `manage.py`, `asgi.py`, `wsgi.py`, `noxfile.py` | root 直下 + src layout package 直下のみ | |
| `docs/conf.py` | `docs/conf.py` のみ | |
| `alembic/env.py` | `alembic/env.py` のみ | |
| `scripts/**/*.py` | glob | dev context |
| manifest `[project.scripts]` 等 | 解決先スクリプト path | |

`DiscoveredSources.files` に **存在する path のみ** entry 化。存在しなければ warning。

### 3.2 `resolve_project_mode`（§8）

```text
console_scripts / gui_scripts / manage.py / asgi.py / wsgi.py / app.py がある → App
[project].name + package __init__.py があり明確な app entry がない → Library
uv workspace members 複数 → Workspace（v0.1 は App 相当で扱い warning）
いずれでもない → App（unused_file confidence 上限 Likely）
```

出力: `ResolvedMode { mode: ProjectMode, confidence: ResolveConfidence }`

### 3.3 `EntryPlan`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPlan {
    pub mode: ResolvedMode,
    pub roots: Vec<EntryRoot>,
    pub warnings: Vec<EntryWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRoot {
    pub spec: EntrySpec,
    pub context: FileContext,
    pub origins: Vec<EntryOrigin>,   // Config | Manifest | Plugin | Auto
}
```

### 3.4 graph 統合

新規 node（Step 8）:

```rust
pub struct EntryNode {
    pub label: String,       // e.g. "script:acme-cli" or "auto:manage.py"
    pub context: FileContext,
}
```

辺:

```text
Entry reaches File { entry, file }
```

`apply_entry_plan(graph, &plan)` で一括追加。

## 4. モジュール構成

```
src/
  entry/
    mod.rs
    types.rs
    error.rs
    auto.rs           # 自動検出規則
    mode.rs           # resolve_project_mode
    merge.rs          # 4 ソースのマージ・dedup
    build.rs          # build_entry_roots
    apply.rs          # graph 辺追加
```

## 5. API

```rust
pub fn build_entry_roots(
    config: &YokeiConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    plugins: &PluginHints,
    production: bool,
) -> Result<EntryPlan, EntryError>;
```

**依存:** Step 5 `PluginHints`、Step 4 `DiscoveredSources`、Step 3 entry points。

`production` は config + CLI override 適用後の値。

## 6. テスト計画

| fixture | 期待 |
| --- | --- |
| Django layout (`manage.py` + settings) | manage.py entry + plugin entries マージ |
| FastAPI (`src/pkg/asgi.py`) | asgi entry |
| library only (`src/acme/__init__.py`) | Library mode、auto entry 最小 |
| explicit `config.entry` | auto より優先表示（origins に両方） |
| missing `config.entry` path | warning |

## 7. Exit criteria

- [ ] `build_entry_roots` が §8 規則を満たす
- [ ] `resolve_project_mode` が app / library を正しく判定
- [ ] `Entry reaches File` 辺が graph に追加される
- [ ] `production` で test entry 除外
- [ ] `make check` 通過、production 無 panic
- [ ] `update-docs` で `entry/` を §15 に追記

## 8. 実装順序

```text
1. entry/types.rs
2. entry/auto.rs + entry/mode.rs
3. entry/merge.rs
4. entry/build.rs
5. graph 拡張（EntryNode, Entry reaches File）
6. entry/apply.rs
7. tests/fixtures/entry/*
8. make check
```

## 9. update-plan 検証サマリ（確定）

| カテゴリ | 得点 | 判定 |
| --- | ---: | --- |
| モジュール設計 | 19 | |
| 静的解析制約 | 20 | |
| ルール / ポリシー | 19 | §8 深度制限を表形式で固定 |
| エラー処理 | 20 | |
| テスト容易性 | 18 | |
| **合計** | **96** | **合格** |

**合格 — 実装着手可。** Step 4–5 と manifest entry points に依存。Step 6–7 と並行可能（graph apply は Step 7 後でも可）。
