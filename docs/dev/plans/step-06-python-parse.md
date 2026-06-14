# Step 6: Python Parse 設計

解析パイプライン §6 の **処理ステップ 6 (Python parse)** の本実装設計。
Phase 0 spike（`parse_file` + トップレベル `import` 抽出）を拡張し、到達性・依存・symbol 解析が
消費する **`ParsedModule` 完全形** を供給する。

> **関連プラン**
>
> - [`phase-0-parser-spike-graph-core.md`](./phase-0-parser-spike-graph-core.md) — spike 完了 ✅（ADR 0001: `rustpython-parser`）
> - [`step-05-config-plugin-extraction.md`](./step-05-config-plugin-extraction.md) — `pytest_plugins` 等は本ステップで AST 抽出
> - [`step-07-import-resolution.md`](./step-07-import-resolution.md) — `ImportRef` の解決先分類

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | Python ソースから **import・動的参照・公開シンボル・ignore 指令・装飾子** を静的抽出し、graph / rules の入力を揃える |
| 成果物 | `parse_project_sources(...) -> Result<ParseSummary, ParseError>` + 拡張 `parse_file` |
| Phase 0 / 1 との関係 | v0.1 MVP の解析中核。Phase 0 spike の置換・拡張 |
| 後続ステップへの入力 | Step 7 (resolver)、Step 9 (reachability)、Step 10–12 (rules / issues)、Step 5 補完 (`pytest_plugins`) |

## 2. スコープ

### In scope（v0.1）

- 全 `DiscoveredSources` の `.py` ファイルを parse（`.pyi` は **スキップ**。型 stub は import 辺なし）
- **import 抽出（完全版）**
  - トップレベル + ネストブロック内（関数・class・`if` / `try` / `with` / `match`）
  - 相対 import（`from . import x`, `from ..pkg import y`）→ **絶対 module 名**へ正規化
  - `TYPE_CHECKING` ブロック内 import → `ImportContext::Type`
  - `if TYPE_CHECKING:` 以外の条件付き import → `ImportContext::Runtime`（confidence 低下は Step 12）
- **動的 import**（リテラルのみ）
  - `importlib.import_module("pkg")`
  - `__import__("pkg")`
- **try-import** パターン検出 → `ImportRef::optional: true`
- **`__all__`** リスト抽出（リテラル / 文字列連結の単純形）
- **`# chokkin: ignore[...]`** / `# chokkin: file-ignore[...]`（§18）
- **トップレベル symbol 定義**（function / class / assign）→ Step 11 入力
- **装飾子名**の収集（`route` / `get` / `fixture` / `task` 等 — 完全一致リスト）
- **`target_version`** による構文 gate（未対応構文は diagnostic、可能なら部分抽出継続）
- 構文エラーは **ファイル単位で diagnostic**、プロジェクト parse は継続
- `ParseSummary` に集計（成功 / エラー / スキップ件数）

### Out of scope（後続ステップ）

| 項目 | 担当 |
| --- | --- |
| import 名 → distribution 解決 | Step 7 |
| graph への辺追加（本格） | Step 7–9（Phase 0 の `add_parsed_imports` は spike 用に維持・拡張） |
| decorator 引数からの URL path 解析 | v0.2 |
| notebook (`.ipynb`) | v0.2 §16 |
| `setup.py` の AST 化 | Step 3 限定パーサからの移行 — 別 PR |
| 全 string literal の module 参照化 | plugin + Step 5 が担当。parse は **呼び出しパターン限定** |
| type annotation 内の forward ref 解決 | Step 11 — 名前収集のみ |
| 並列 parse / cache | §19 — v0.1 後半 |

## 3. 仕様との対応

### 3.1 import context（§10）

| `ImportContext` | 意味 | 依存判定 |
| --- | --- | --- |
| `Runtime` | 通常 import | runtime dependency |
| `Type` | `TYPE_CHECKING` 内 | type dependency（`type_groups`） |
| `Test` | test ファイル内の import | test context（file context と併用） |

`FileContext` は Step 4 由来。parse は **import 文の文脈**を追加する。

### 3.2 相対 import 正規化

入力: `path = "src/acme/api/routes.py"`, `from ..models import User`

```text
1. layout から package root を推定（`src/acme/` → package `acme`）
2. ファイルの module 名: `acme.api.routes`
3. level=2, module=`models` → `acme.models`
```

flat layout / namespace / `__init__.py` 欠落は `ParseDiagnostic` + `ImportRef.module` 空で記録（Step 7 で `CHK010`）。

### 3.3 動的 import（§14）

| パターン | 抽出 |
| --- | --- |
| `importlib.import_module("acme.plugins")` | `DynamicImport { module: "acme.plugins", line }` |
| `importlib.import_module(name)` | 記録しない（confidence 低下用に `OpaqueDynamicImport` フラグのみ） |
| `getattr(importlib, "import_module")("x")` | 非対応（v0.1） |

### 3.4 ignore 指令（§18）

```python
# chokkin: file-ignore[CHK001,CHK006]
from legacy import old  # chokkin: ignore[CHK003]
```

- **file-ignore**: ファイル先頭コメントブロック（最初の stmt より前）のみ
- **inline**: 同一物理行の末尾コメント
- パース: `ignore[CODE]` / `ignore[CODE,CODE]` — `CODE` は `YOK` + 3 桁
- 格納: `ParsedModule::ignores: Vec<IgnoreDirective>`

適用（抑制）は Step 12。Step 6 は **抽出のみ**。

### 3.5 `__all__`（§12）

```python
__all__ = ["foo", "bar"]
__all__ = ["foo", "bar"]  # type: ignore
__all__ = ("foo", "bar")
```

- リテラル list/tuple の string 要素のみ
- `__all__ += ["baz"]` は v0.1 非対応（warning）
- 出力: `pub exports: Vec<String>`

### 3.6 装飾子（externally-used ヒント）

Step 11 / plugin が消費する **名前リスト**（v0.1 は完全一致）:

```text
app.get, app.post, app.put, app.delete, app.patch, app.route
router.get, router.post, ...
pytest.fixture, pytest.mark.*
shared_task, app.task
click.command, typer.command
```

格納: `SymbolDef::decorators: Vec<String>`（ドット区切り正規化）

## 4. モジュール構成

```
src/
  parser/
    mod.rs
    types.rs          # 拡張 ParsedModule, ImportRef, SymbolDef, ...
    error.rs          # 既存 + ParseProjectError
    parse.rs          # parse_file（拡張）
    visit.rs          # Stmt 再帰 visitor（import / symbol / dynamic）
    relative.rs       # 相対 import → 絶対 module
    ignores.rs        # comment / ignore 指令抽出
    dynamic.rs        # importlib パターン
    type_checking.rs  # TYPE_CHECKING ブロック検出
    exports.rs        # __all__ 抽出
    project.rs        # parse_project_sources オーケストレーション
    decorators.rs     # 装飾子名正規化
```

`visit.rs` に AST walk を集約し、`parse.rs` はエントリと IO のみ。

## 5. データ型（拡張）

### 5.1 `ImportRef`（拡張）

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRef {
    pub module: String,           // 正規化済み dotted name（空 = 未解決相対）
    pub name: Option<String>,     // from-import の import 名（import pkg as x の x は None で module 側）
    pub alias: Option<String>,
    pub line: u32,
    pub kind: ImportKind,
    pub context: ImportContext,
    pub optional: bool,           // try-import 内
    pub relative_level: u8,       // 0 = absolute
}
```

### 5.2 `ParsedModule`（拡張）

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModule {
    pub path: String,
    pub imports: Vec<ImportRef>,
    pub dynamic_imports: Vec<DynamicImport>,
    pub symbols: Vec<SymbolDef>,
    pub exports: Vec<String>,           // __all__
    pub ignores: Vec<IgnoreDirective>,
    pub has_opaque_dynamic_import: bool,
    pub diagnostics: Vec<ParseDiagnostic>,
}
```

### 5.3 `SymbolDef`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub name: String,
    pub kind: SymbolKind,         // Function | Class | Variable
    pub line: u32,
    pub is_public: bool,          // not _prefix unless in __all__
    pub decorators: Vec<String>,
    pub in_type_checking: bool,
}
```

### 5.4 `ParseSummary`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSummary {
    pub modules: Vec<ParsedModule>,
    pub parsed_count: u32,
    pub error_count: u32,
    pub skipped_count: u32,       // .pyi 等
}
```

## 6. API

### 6.1 `parse_file`（拡張シグネチャ）

```rust
pub fn parse_file(
    root: &ProjectRoot,
    path: &str,
    layout: &LayoutInfo,
    target: TargetVersion,
) -> Result<ParsedModule, ParseError>
```

`layout` 追加は **相対 import 正規化**に必要。既存呼び出しは `parse_file_legacy` を 1 リリースだけ残すか、テストを一括更新。

### 6.2 `parse_project_sources`

```rust
pub fn parse_project_sources(
    root: &ProjectRoot,
    sources: &DiscoveredSources,
    target: TargetVersion,
) -> Result<ParseSummary, ParseError>
```

- `.py` のみ iterate
- ファイルごとに `parse_file` — **順次**（v0.1）。`rayon` は §19 達成時
- IO エラーは `ParseError::Io` で **全体中断**（disk 障害）。構文エラーは継続

### 6.3 graph 接続（spike 拡張）

既存 `add_parsed_imports(graph, parsed)` を拡張:

- `ImportRef.module` が空でなければ `FileImportsModule` 辺を追加
- `DynamicImport` も同様（literal のみ）
- `ModuleOrigin` は Step 7 まで `Unknown`

## 7. `target_version` gate

```rust
fn supports_syntax(target: TargetVersion, feature: SyntaxFeature) -> bool { ... }
```

| feature | 最低 version |
| --- | --- |
| `match` stmt | py310 |
| `type` alias stmt | py312 |
| PEP 695 generics | py312 |

未対応構文でパーサが失敗した場合: diagnostic に `requires py3XX` を付与。  
パーサが成功する場合は抽出継続（RustPython が新構文を先にサポートするケース）。

## 8. テスト計画

### 8.1 fixture カテゴリ（`tests/fixtures/parse/`）

| ディレクトリ | 内容 |
| --- | --- |
| `imports/` | absolute / relative / nested / type_checking |
| `dynamic/` | importlib literal / opaque |
| `exports/` | __all__ 各形式 |
| `ignores/` | file-ignore / inline |
| `symbols/` | function / class / decorated |
| `syntax/` | version-gated constructs |
| `broken/` | 構文エラー継続 |

### 8.2 回帰

- 既存 `tests/parser_parse.rs` / `pipeline_phase0_spike.rs` を更新
- spike 成功率 ≥ 95% を維持（fixture 拡張後も）

### 8.3 property tests

- 相対 import 正規化: `proptest` で level + module + path → 期待 dotted name
- ignore パース: ランダム CODE 列

## 9. 依存

| Crate | 用途 | 備考 |
| --- | --- | --- |
| `rustpython-parser` | AST | 既存。ADR 0001 |
| `regex` | ignore 指令 | 既存（Step 5 と共有可） |

新規 crate 追加なし（v0.1）。

## 10. Exit criteria（Step 6 完了定義）

- [ ] `ParsedModule` 拡張フィールドが `make check` を通過
- [ ] 相対 import が src / flat layout fixture で正規化される
- [ ] `TYPE_CHECKING` import が `ImportContext::Type` になる
- [ ] `importlib.import_module("...")` literal が `dynamic_imports` に入る
- [ ] `__all__` と ignore 指令が抽出される
- [ ] 構文エラーファイルでも `ParseSummary` が返る（`error_count >= 1`）
- [ ] `parse_project_sources` が `lib.rs` から re-export される
- [ ] production コードに `unwrap` / `expect` / `panic` がない
- [ ] fixture 30 件以上で回帰テスト
- [ ] `docs/dev/spec.ja.md` §6 Step 6 の「spike」注記を削除（`update-docs`）

## 11. 実装順序（推奨）

```text
1. types.rs 拡張（後方互換に ImportRef フィールド追加）
2. type_checking.rs + visit.rs 骨格
3. relative.rs + layout 連携
4. visit.rs — import 完全版
5. dynamic.rs, exports.rs, ignores.rs
6. decorators.rs + symbol 収集
7. parse.rs リファクタ
8. project.rs — parse_project_sources
9. graph/edges.rs — dynamic import 辺
10. fixtures + tests
11. make check
12. update-docs
```

所要: 新規 Rust ファイル 8 前後、fixture 30、既存テスト更新。

## 12. パフォーマンス（§19 前提）

v0.1 cold 目標 2s（medium project）に向けた制約:

- 単一ファイル parse は 1ms 台を目標（p95）
- 全ファイル parse は Step 9 まで計測のみ（`--timings` は Phase 1 CLI）
- メモリ: `ParseSummary` は全 module を保持（cache は v0.2）

## 13. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| Ruff parser への切替 | ADR 0001 で rustpython 採用 | Step 6 fixture で品質不足なら ADR 更新 |
| `.pyi` parse | stub は import 辺不要 | v0.2 type stub 解析 |
| `match` 内 import | 稀少 | visit に含める（コスト低） |
| `__all__` 動的構築 | 静的不可 | diagnostic のみ |

## 14. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `docs/dev/plans/step-06-python-parse.md` | 本プラン |
| `docs/adr/0001-parser-selection.md` | rustpython-parser 採用 |
| `src/parser/parse.rs` | spike 実装 — 本プランで拡張 |
| `src/sources/types.rs` | `LayoutInfo`, `FileKind` |
| `step-05` | `pytest_plugins` は Step 6 AST へ |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | visitor 分割。型は Step 7/11 向け |
| 静的解析制約 | 20 | 20 | 実行禁止維持。literal のみ動的解決 |
| ルール / ポリシー | 20 | 18 | ignore は抽出のみ。判定は Step 12 |
| エラー処理 | 20 | 20 | 構文エラー継続。IO のみ中断 |
| テスト容易性 | 20 | 19 | fixture 30 + proptest |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| §6 処理順 Step 6 | OK |
| Phase 0 `add_parsed_imports` | OK — 拡張互換 |
| Step 4 `DiscoveredSources` | OK — layout 入力 |
| Step 7 境界 | OK — `ModuleOrigin` は Step 7 |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P0** | `parse_file` に `layout` 追加は破壊的 | §6.1 でテスト一括更新を明記 |
| **P1** | string literal 全収集は scope creep | Out of scope に固定 |

### 確定判定

**合格 — 実装着手可。** Step 4 完了と Phase 0 parser spike に依存。
