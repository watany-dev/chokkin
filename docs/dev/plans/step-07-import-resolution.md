# Step 7: Import Resolution 設計

解析パイプライン §6 の **処理ステップ 7 (import resolution)** の実装設計。
Step 6 が抽出した `ImportRef` / `DynamicImport` を **stdlib / first-party / third-party** に分類し、
import 名と **PEP 508 distribution 名**の対応を解決する。Phase 0 exit の **bundled maps** も本ステップで供給する。

> **関連プラン**
>
> - [`step-06-python-parse.md`](./step-06-python-parse.md) — parse 出力を入力とする
> - [`phase-0-parser-spike-graph-core.md`](./phase-0-parser-spike-graph-core.md) — `ModuleOrigin` 分類の本実装
> - [`step-05-config-plugin-extraction.md`](./step-05-config-plugin-extraction.md) — `ModuleReference` も同じ resolver を通す

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | `import yaml` と `PyYAML`、`PIL` と `Pillow` のような **名前不一致**を多層戦略で解決し、CHK002–CHK004 / CHK010 の前提データを作る |
| 成果物 | `resolve_imports(...) -> Result<ResolutionIndex, ResolveError>` + bundled map データ |
| Phase 0 / 1 との関係 | Phase 0 exit の maps 初版を含む。resolver は v0.1 MVP 必須 |
| 後続ステップへの入力 | Step 9–10 (reachability / dependency reconciliation)、Step 12 (`CHK010`) |

## 2. スコープ

### In scope（v0.1）

- §7 の解決戦略 **1–8** を実装（workspace は v0.1 簡易版: root manifest のみ）
- **`.venv` metadata 読み取り**（存在時のみ）: `dist-info/METADATA`, `top_level.txt`, `entry_points.txt`, `RECORD`
- **bundled package-module-map 初版**（PyPI download 上位 ~500 → コンパイル時埋め込み）
- **bundled binary map 初版**（主要 dev tool ~50）
- `[tool.chokkin].package_module_map` / `binary_map` のマージ（user > bundled）
- `LockfileGraph` を使った **transitive 判定**入力（CHK004 — Step 10 が消費）
- graph の `ModuleOrigin` 更新 + `Distribution provides Module` 辺
- **stdlib リスト**（`target_version` 別、Rust 側静的テーブル）
- **first-party** 判定（layout packages + namespace 考慮の簡易版）
- **canonicalize フォールバック**（`python-dotenv` → `dotenv` 推定）— confidence `maybe` フラグ付き

### Out of scope（後続ステップ）

| 項目 | 担当 |
| --- | --- |
| CHK002–CHK010 判定ロジック | Step 10–12 |
| uv workspace member ごとの resolver 境界 | v0.2 §16 |
| Core Metadata `Import-Name` 優先（普及後） | v0.2 — 読み取りフックのみ v0.1 で用意 |
| Poetry / PDM lock の transitive | v0.2 |
| map 自動生成 pipeline（CI） | 横断 work §17 |
| `.venv` 必須化 | 禁止 — §7 |

## 3. 仕様との対応

### 3.1 解決パイプライン（§7）

各 **top-level import 名**（`yaml`, `acme.models` → top `acme`）に対し:

```text
1. stdlib テーブル（target_version）
2. first-party: layout.packages プレフィックス一致
3. workspace member（v0.1: uv_workspace.members があれば member 名のみ）
4. .venv/dist-info/* — Import-Name / top_level.txt / METADATA Name
5. PEP 794 Import-Name / Import-Namespace（フィールドがあれば）
6. bundled package-module-map（distribution → [import names] の逆引きインデックス）
7. user package_module_map
8. canonicalize: dist_name.replace('-', '_') == import_root → maybe
```

**出力:** `ResolvedImport { import_root, origin: ModuleOrigin, distribution: Option<DistributionId>, confidence }`

### 3.2 bundled package-module-map（Phase 0 exit）

**形式（ビルド時生成）:**

```rust
// resolver/bundled/package_modules.rs (generated)
pub static PACKAGE_TO_IMPORTS: &[(&str, &[&str])] = &[
    ("PyYAML", &["yaml"]),
    ("Pillow", &["PIL"]),
    ("python-dotenv", &["dotenv"]),
    // ...
];
```

**逆引きインデックス**（起動時 1 回構築）:

```text
import_root "yaml" → candidates: [PyYAML (certain)]
import_root "PIL" → candidates: [Pillow (certain)]
```

複数 candidate は `confidence: Maybe` + `ResolveWarning::AmbiguousImport`。

**データソース（v0.1）:**

```text
scripts/generate-package-map.py  # PyPI bigquery / top-download 静的 JSON を入力
data/package-map.seed.json       # 手動キュレーション最小セット（CI でも可）
```

生成物は **リポジトリにコミット**（ビルド時ネットワーク不要）。

### 3.3 bundled binary map

```rust
pub static BINARY_TO_DISTRIBUTION: &[(&str, &str)] = &[
    ("pytest", "pytest"),
    ("uvicorn", "uvicorn"),
    ("ruff", "ruff"),
    ("mypy", "mypy"),
    // ~50 entries
];
```

消費: Step 5 `BinaryUsage` + `.venv/entry_points.txt` + user `binary_map`。

### 3.4 `.venv` 探索

```text
候補（順）:
  <root>/.venv
  <root>/venv
  $VIRTUAL_ENV（chokkin プロセスの env — project の venv と混同しないよう root 配下のみ採用）
```

`dist-info` ディレクトリ名: `{name}-{version}.dist-info` を parse。

### 3.5 lockfile と transitive（Step 10 入力）

`ResolutionIndex` は判定せず、**参照データ**を整える:

```rust
pub struct TransitiveIndex {
    /// distribution name → direct dependency names (from uv.lock)
    pub edges: BTreeMap<String, Vec<String>>,
    /// precomputed transitive closure (optional, medium projects)
    pub closure: BTreeMap<String, BTreeSet<String>>,
}
```

lockfile なし → `TransitiveIndex::empty()` + Step 10 で CHK004 縮退（§10）。

## 4. モジュール構成

```
src/
  resolver/
    mod.rs
    types.rs            # ResolvedImport, ResolutionIndex, ModuleOrigin 更新
    error.rs            # ResolveError, ResolveWarning
    resolve.rs          # resolve_imports オーケストレーション
    stdlib.rs           # versioned stdlib セット
    first_party.rs      # layout / path → module
    venv.rs             # dist-info 読み取り
    bundled/
      mod.rs
      package_modules.rs  # generated
      binaries.rs         # generated or hand-written
    maps.rs             # user + bundled マージ、逆引き
    metadata.rs         # PEP 794 Import-Name パース
    transitive.rs       # LockfileGraph → TransitiveIndex
    apply.rs            # ProjectGraph への辺追加
data/
  package-map.seed.json
  binary-map.seed.json
scripts/
  generate-package-map.py   # 開発者向け。CI optional
```

## 5. データ型

### 5.1 `ResolvedImport`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImport {
    pub import_root: String,
    pub full_module: String,          // 元の dotted name
    pub file: String,
    pub line: u32,
    pub context: ImportContext,
    pub origin: ModuleOrigin,
    pub distribution: Option<String>, // normalized PEP 508 name
    pub confidence: ResolveConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveConfidence {
    Certain,   // stdlib / first-party / venv metadata / bundled 1:1
    Likely,    // user map
    Maybe,     // canonicalize fallback / ambiguous
}
```

### 5.2 `ResolutionIndex`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionIndex {
    pub imports: Vec<ResolvedImport>,
    pub warnings: Vec<ResolveWarning>,
    pub transitive: TransitiveIndex,
    pub binary_resolutions: BTreeMap<String, String>, // merged binary map
}
```

### 5.3 `ResolveWarning`

```rust
pub enum ResolveWarning {
    AmbiguousImport { import: String, candidates: Vec<String> },
    UnresolvedImport { import: String, file: String, line: u32 },
    VenvUnreadable { path: String, reason: String },
    MissingBundledMapEntry { import: String }, // 情報用。CHK010 は Step 12
}
```

## 6. API

### 6.1 `resolve_imports`

```rust
pub fn resolve_imports(
    root: &ProjectRoot,
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    parse: &ParseSummary,
    plugin_refs: &[ModuleReference],  // Step 5 出力も解決
) -> Result<ResolutionIndex, ResolveError>
```

### 6.2 `apply_resolution_to_graph`

```rust
pub fn apply_resolution_to_graph(
    graph: &mut ProjectGraph,
    index: &ResolutionIndex,
) -> Result<(), GraphError>
```

追加する辺:

```text
Distribution provides Module   (third-party)
File imports Module            (既存 — origin 付きで module node 更新)
ConfigReference uses Module    (plugin module refs — Step 8/9 で entry 統合時)
```

### 6.3 graph 型拡張

`GraphEdge` に追加（Step 7）:

```rust
DistributionProvidesModule {
    distribution: DistributionId,
    module: ModuleId,
}
```

`ModuleNode.origin` を `Unknown` から更新。

## 7. stdlib テーブル

**方式:** `resolver/stdlib/py311.txt` 等を `include_str!` で埋め込み。

- ソース: Python 公式 `sys.stdlib_module_names`（3.10–3.13 各 1 ファイル）
- `target_version` で選択
- `sys` / `typing` 等は常に stdlib

## 8. テスト計画

### 8.1 fixture（`tests/fixtures/resolver/`）

| ケース | 期待 |
| --- | --- |
| `import yaml` + PyYAML declared | third-party → PyYAML |
| `import acme` (first-party) | FirstParty |
| `import os` | Stdlib |
| `.venv` with dist-info | metadata 優先 |
| user `package_module_map` | overrides bundled |
| ambiguous import | warning + Maybe |
| no lockfile | transitive empty |

### 8.2 bundled map テスト

- seed JSON の全エントリが逆引き可能
- `PyYAML`/`yaml` 回帰

### 8.3 統合

```text
tests/resolver_resolve.rs
discover → manifest → sources → parse → resolve_imports
```

## 9. 依存

| Crate | 用途 |
| --- | --- |
| `serde` + `serde_json` | map seed 読み込み（build.rs のみ、または生成スクリプト） |

**方針:** runtime に `serde_json` を増やさない。map は **Rust ソース生成**で埋め込み。

`build.rs` は v0.1 では使わず、`scripts/generate-package-map.py` → commit 生成物。

## 10. Exit criteria（Step 7 + Phase 0 maps 完了定義）

- [ ] `src/resolver/` が `make check` を通過
- [ ] bundled map に **≥ 200** distribution（v0.1 目標 500 に向け seed 拡張可能）
- [ ] bundled binary map に **≥ 30** entry
- [ ] `yaml`→`PyYAML`, `PIL`→`Pillow`, `dotenv`→`python-dotenv` が解決
- [ ] first-party / stdlib / third-party 分類がテストされている
- [ ] `.venv` fixture で dist-info 優先が動作
- [ ] `resolve_imports` + `apply_resolution_to_graph` が `lib.rs` から re-export
- [ ] `TransitiveIndex` が `uv.lock` から構築される
- [ ] production コードに `unwrap` / `expect` / `panic` がない
- [ ] `docs/dev/spec.ja.md` §15 に `resolver/` 追記（`update-docs`）

## 11. 実装順序（推奨）

```text
1. data/*.seed.json — 手動最小セット
2. scripts/generate-package-map.py
3. resolver/bundled/* — 生成コミット
4. resolver/stdlib.rs
5. resolver/maps.rs — 逆引き
6. resolver/venv.rs
7. resolver/first_party.rs
8. resolver/resolve.rs
9. resolver/transitive.rs
10. resolver/apply.rs + graph edge 拡張
11. tests + make check
12. update-docs
```

maps 初版は **resolver より先にマージ可能**（Phase 0 並行 work）。

## 12. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| map 500 件の網羅性 | 手動 seed から漸進 | 横断 work で自動生成 |
| namespace package (`google.cloud`) | 複数 dist | v0.1 は longest match + maybe |
| `types-*` stub と runtime の対応 | §10 stub 規則 | Step 10 |
| Windows `.venv` Scripts vs bin | path 差 | `venv.rs` で両対応 |

## 13. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `docs/dev/plans/step-07-import-resolution.md` | 本プラン |
| `docs/dev/spec.ja.md` §7, §10, §17 | 多層戦略・maps と一致 |
| `src/config/types.rs` | `package_module_map`, `binary_map` 確定 |
| `src/manifest/types.rs` | `LockfileGraph` 確定 |
| `step-06` | `ParseSummary` 入力 |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | resolver 単独。maps は bundled サブモジュール |
| 静的解析制約 | 20 | 20 | dist-info 読み取りのみ。Python 非実行 |
| ルール / ポリシー | 20 | 19 | YOK 判定は Step 10。confidence 付与 |
| エラー処理 | 20 | 19 | venv 失敗は warning 継続 |
| テスト容易性 | 20 | 19 | fixture + seed 回帰 |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| §7 解決順 | OK |
| Phase 0 exit maps | OK — 本 PR に含む |
| Step 6 境界 | OK |
| graph `ModuleOrigin` | OK — `apply` で更新 |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | 500 件を v0.1 必須にすると初 PR が巨大 | Exit criteria を ≥200 + 拡張可能に |
| **P2** | `build.rs` 要否 | 生成スクリプト + commit で回避 |

### 確定判定

**合格 — 実装着手可。** Step 6 と maps seed は部分並行可能。
