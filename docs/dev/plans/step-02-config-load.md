# Step 2: Config Load 設計

解析パイプライン §6 の **処理ステップ 2 (config load)** の実装設計。
Step 1 (`discover_project_root`) の直後に位置し、zero-config 実行のための既定値と
`[tool.yokei]` 設定を型安全に読み込む。

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | 解析ポリシー（entry / project glob / mode / plugin 有効化 / ignore / map 等）を、設定ファイルなしでも一貫した既定値で供給する |
| 成果物 | `load_config(&ProjectRoot) -> Result<LoadedConfig, ConfigError>` |
| Phase 0 との関係 | graph core / parser spike と並行可能。TOML 読み取りのみで Python 非実行を維持 |
| 後続ステップへの入力 | Step 3 (manifest extraction) 以降の全ステップが参照する `YokeiConfig` と、キャッシュ hash 用の `ConfigSources` |

## 2. スコープ

### In scope

- project root 直下の設定ソースを探索し、§5 の `[tool.yokei]` 相当を読み込む
- 設定が無い場合は **hardcoded defaults** のみで `LoadedConfig` を返す（zero-config）
- 複数ソースの **マージ規則** を固定し、どのファイルが寄与したかを記録する
- `[tool.yokei.workspaces.<id>]` の読み込み（明示 workspace override）
- root `pyproject.toml` 内の `[tool.uv.workspace]` を **参照用に** 読み取る（member パス一覧の保持。自動下方向スキャンは v0.2）
- 設定値の **構文検証**（enum・必須形式・相対パス制約）
- `src/config/` として単体テスト可能な library API を提供する

### Out of scope（後続ステップ）

| 項目 | 担当ステップ |
| --- | --- |
| `[project]` / requirements / lockfile の依存宣言抽出 | Step 3 (manifest extraction) |
| `mode = "auto"` の app / library / workspace への解決 | Step 8 (entry root construction) 前の `resolve_mode` |
| `entry` / `project` glob のファイルシステム展開 | Step 4 (source file discovery) |
| plugin 設定ファイル（`pytest.ini` / Django settings 等）の解析 | Step 5 (config/plugin extraction) |
| CLI フラグ（`--production` / `--strict` 等）のパースとマージ | CLI 統合 PR（Step 2 API は `RuntimeOverrides` 受け口のみ定義） |
| `yokei --init` の雛形生成 | v0.1 CLI 機能（`toml_edit` 使用） |
| baseline file (`yokei-baseline.json`) | v0.2 |
| ネストした `pyproject.toml` の自動 workspace 検出 | v0.2（uv workspace 本格対応） |
| ignore pattern の glob マッチング | Step 12 (issue emission) |

## 3. 仕様との対応

### 3.1 設定ソースと優先順位

§5: `pyproject.toml` の `[tool.yokei]` を第一候補とし、`yokei.toml` / `.yokei.toml` も許容する。

**マージ順（低 → 高。後勝ちで上書き）:**

```text
1. DEFAULTS                    # コード内定数
2. <root>/.yokei.toml          # 存在すれば
3. <root>/yokei.toml           # 存在すれば
4. <root>/pyproject.toml の [tool.yokei]   # 存在すれば（最高優先）
```

不変条件:

1. **root 相対** — すべてのパス文字列は `ProjectRoot.path` 基準の相対パスとして保持する（絶対パス入力は `ConfigError::Validation`）
2. **欠落は既定値** — キー未指定はエラーにしない（zero-config）
3. **TOML のみ静的パース** — `pyproject.toml` も文字列として読み、`[tool.yokei]` サブツリーのみ deserialize する（`setup.py` 実行禁止）
4. **突はマージ** — 同一キーが複数ソースにある場合、優先順位の高いソースが勝つ。配列・map は **置換**（deep merge しない）

`pyproject.toml` が存在しない root（`requirements.txt` のみ等）では、ソース 2–3 のみが有効。

### 3.2 既定値（DEFAULTS）

§5 最小設定例と README の zero-config 記述に合わせる。

| キー | 既定値 |
| --- | --- |
| `entry` | `[]` |
| `project` | `[]`（空 = Step 4 で layout 自動推定） |
| `mode` | `"auto"` |
| `production` | `false` |
| `target_version` | `"py311"` |
| `respect_gitignore` | `true` |
| `confidence` | `"likely"` |
| `exclude` | `[".venv/**", "build/**", "dist/**", "**/__pycache__/**"]` |
| `dependencies.dev_groups` | `["dev", "test", "tests", "lint", "docs"]` |
| `dependencies.runtime_groups` | `["server", "worker"]` |
| `dependencies.type_groups` | `["types", "typing", "mypy"]` |
| `package_module_map` | `{}` |
| `binary_map` | `{}` |
| `plugins.pytest` | `true` |
| `plugins.django` | `true` |
| `plugins.fastapi` | `true` |
| `plugins.celery` | `false` |
| `plugins.tox` | `false` |
| `plugins.nox` | `false` |
| `plugins.pre_commit` | `false` |
| `plugins.github_actions` | `false` |
| `ignore` | `{}`（rule code → pattern リスト） |
| `workspaces` | `{}` |

v0.1 MVP の plugin 3 つ（pytest / django / fastapi）のみ default `true`。§16 に合わせ、残りは default `false`。

### 3.3 workspace 関連（Step 2 での最小対応）

| 読み取り対象 | Step 2 の扱い |
| --- | --- |
| `[tool.yokei.workspaces.<id>]` | 完全に読み込み、`WorkspaceOverride` として保持 |
| `[tool.uv.workspace].members` | root `pyproject.toml` から member パターン文字列を抽出し `UvWorkspaceHint` として保持（パス未展開） |
| 下方向の `pyproject.toml` 自動検出 | **実装しない**（v0.2） |

`WorkspaceOverride` は `path`（必須）と、root 設定と同型の optional override（`entry` / `project` / `mode` 等）を持つ。member 実体の解決は Step 3 以降。

## 4. モジュール構成

```
src/
  lib.rs
  discovery/          # Step 1（既存）
  config/
    mod.rs            # 公開 API と re-export
    error.rs          # ConfigError
    types.rs          # YokeiConfig, LoadedConfig, EntrySpec, ...
    defaults.rs       # DEFAULTS 定数と apply_defaults
    source.rs         # ConfigSources, ファイル探索
    load.rs           # load_config 実装
    parse.rs          # TOML deserialize、個別サブセクション
```

`main.rs` は Step 2 では触らない。CLI 統合は Step 2 exit criteria 達成後の別 PR。

## 5. データ型

### 5.1 列挙型

```rust
/// Project analysis mode (§5, §8). `Auto` is resolved later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectMode {
    #[default]
    Auto,
    App,
    Library,
}

/// Minimum confidence for emitted issues (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Confidence {
    Certain,
    #[default]
    Likely,
    Maybe,
}

/// Known yokei plugins (§5, §9). v0.1 enables pytest/django/fastapi by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginId {
    Pytest,
    Django,
    Fastapi,
    Celery,
    Tox,
    Nox,
    PreCommit,
    GithubActions,
}
```

`ProjectMode` / `Confidence` は `Display` と `as_str()` を実装し、reporter / `--explain` 向けに安定文字列を返す。

### 5.2 `EntrySpec`

§5 の `src/acme/asgi.py:application` 形式。

```rust
/// Entry root: file path, optionally `path:symbol` for WSGI/ASGI callables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    /// Path relative to project root (no `:` suffix).
    pub path: String,
    /// Optional symbol after `:`, e.g. `application` in `asgi.py:application`.
    pub symbol: Option<String>,
}
```

パース規則:

- 最後の `:` の右側が `path` に `/` または `\` を含まない場合のみ symbol として解釈
- Windows ドライブレター `C:\foo.py` は Step 2 では **拒否**（相対パスのみ許可）
- 空文字・`.` / `..` のみの path は `Validation` エラー

### 5.3 `YokeiConfig`

マージ後の effective 設定。Step 3–13 が参照する単一の設定オブジェクト。

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YokeiConfig {
    pub entry: Vec<EntrySpec>,
    pub project: Vec<String>,           // glob patterns, unexpanded
    pub mode: ProjectMode,
    pub production: bool,
    pub target_version: TargetVersion,  // newtype over "py311" etc.
    pub respect_gitignore: bool,
    pub confidence: Confidence,
    pub exclude: Vec<String>,
    pub dependencies: DependencyGroupsConfig,
    pub package_module_map: BTreeMap<String, Vec<String>>,
    pub binary_map: BTreeMap<String, String>,
    pub plugins: BTreeMap<PluginId, bool>,
    pub ignore: BTreeMap<String, Vec<String>>,  // YOK001..YOK010 -> patterns
    pub workspaces: BTreeMap<String, WorkspaceOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGroupsConfig {
    pub dev_groups: Vec<String>,
    pub runtime_groups: Vec<String>,
    pub type_groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOverride {
    pub path: String,
    pub entry: Option<Vec<EntrySpec>>,
    pub project: Option<Vec<String>>,
    pub mode: Option<ProjectMode>,
    // 他フィールドは v0.2 で拡張。Step 2 は §5 例に列挙のあるもののみ
}
```

`TargetVersion` は `py` + 3 桁 minor（`py310`–`py314` 等）を検証する newtype。解析対象 Python バージョン（§5 `target_version`）。

### 5.4 `ConfigSources` と `LoadedConfig`

キャッシュ hash（§19 `config hash`）と `--explain` 用。

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSources {
    /// True when hardcoded defaults contributed (always true).
    pub used_defaults: bool,
    pub dot_yokei_toml: Option<PathBuf>,
    pub yokei_toml: Option<PathBuf>,
    pub pyproject_tool_yokei: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub root: ProjectRoot,
    pub effective: YokeiConfig,
    pub sources: ConfigSources,
    /// Raw `[tool.uv.workspace]` from root pyproject, if present.
    pub uv_workspace: Option<UvWorkspaceHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvWorkspaceHint {
    pub members: Vec<String>,  // glob patterns as written in pyproject
}
```

### 5.5 `RuntimeOverrides`（CLI 統合用の受け口）

Step 2 では型定義のみ。マージ実装は CLI PR で行う。

```rust
/// CLI flags that override file config (§2). Unset fields do not override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    pub production: Option<bool>,
    pub strict: Option<bool>,
    pub confidence_floor: Option<Confidence>,
    // include/exclude/reporter は CLI 統合 PR で追加
}
```

`apply_overrides(config: &mut YokeiConfig, overrides: &RuntimeOverrides)` は Step 2 で **単体テスト可能な pure 関数**として実装する。

### 5.6 `ConfigError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}")]
    Io { path: PathBuf, #[source] source: std::io::Error },

    #[error("invalid TOML in {path}: {message}")]
    InvalidToml { path: PathBuf, message: String },

    #[error("invalid config at {path}.{field}: {message}")]
    Validation { path: PathBuf, field: &'static str, message: String },

    #[error("unknown key in [tool.yokei] at {path}: {key}")]
    UnknownKey { path: PathBuf, key: String },
}
```

exit code 対応（将来 CLI）:

| Variant | `ExitStatus` |
| --- | --- |
| `Io`（permission 等） | `InternalError` (3) |
| `InvalidToml` / `Validation` / `UnknownKey` | `UsageError` (2) |

`UnknownKey` は `serde(deny_unknown_fields)` 相当でトップレベル `[tool.yokei]` に適用。`[tool.yokei.ignore]` の rule code は `YOK001`–`YOK010` 形式を検証するが、未知 code は **warning ではなく Validation エラー**（typo 早期検出）。

## 6. 公開 API

```rust
// src/config/mod.rs

pub use error::ConfigError;
pub use load::{load_config, apply_overrides};
pub use types::{
    Confidence, ConfigSources, EntrySpec, LoadedConfig, PluginId,
    ProjectMode, RuntimeOverrides, TargetVersion, UvWorkspaceHint,
    WorkspaceOverride, YokeiConfig,
};

/// Load and merge yokei configuration for `root`.
///
/// Returns defaults when no config files exist. Never executes Python.
pub fn load_config(root: &ProjectRoot) -> Result<LoadedConfig, ConfigError>;
```

### 内部ヘルパー（非公開）

```rust
// source.rs
fn discover_config_files(root: &Path) -> ConfigFileSet;

// parse.rs
fn parse_dot_yokei_toml(path: &Path) -> Result<PartialConfig, ConfigError>;
fn parse_yokei_toml(path: &Path) -> Result<PartialConfig, ConfigError>;
fn parse_pyproject_tool_yokei(path: &Path) -> Result<(PartialConfig, Option<UvWorkspaceHint>), ConfigError>;

// defaults.rs
fn default_config() -> YokeiConfig;
fn merge_layers(layers: &[PartialConfig]) -> YokeiConfig;
```

`PartialConfig` は `Option` フィールドの struct で、マージ時に `Some` のみ上書きする（配列・map は丸ごと置換）。

## 7. アルゴリズム

```text
load_config(root):
  1. layers ← [default_config() を PartialConfig 化]
  2. if root/.yokei.toml exists:
       layers.push(parse_dot_yokei_toml(...))
  3. if root/yokei.toml exists:
       layers.push(parse_yokei_toml(...))
  4. uv_hint ← None
     if root/pyproject.toml exists:
       (partial, uv_hint) ← parse_pyproject_tool_yokei(...)
       if partial has any [tool.yokei] keys:
         layers.push(partial)
  5. effective ← merge_layers(layers)
  6. validate_effective(effective, root.path)   # 相対パス・enum・target_version
  7. return LoadedConfig { root: root.clone(), effective, sources, uv_workspace: uv_hint }
```

### 7.1 `pyproject.toml` の読み方

1. ファイル全体を `std::fs::read_to_string`
2. `toml::from_str::<toml::Table>` で parse
3. `table.get("tool").and_then(|t| t.get("yokei"))` のサブテーブルのみ `serde` deserialize
4. `[tool.yokei]` が **無い**場合は layer に追加しない（defaults のみ）
5. 同ファイルから `tool.uv.workspace` を別途読み、`UvWorkspaceHint` を構築（member が無ければ `None`）

`[project]` セクションは Step 2 では触らない。

### 7.2 バリデーション

| チェック | エラー |
| --- | --- |
| `mode` が auto / app / library 以外 | `Validation` |
| `confidence` が certain / likely / maybe 以外 | `Validation` |
| `target_version` が `py3\d{2,3}` に合致しない | `Validation` |
| `entry` / `project` / `exclude` / workspace `path` が絶対パス | `Validation` |
| `entry` に `:` 複数で symbol 解釈不能 | `Validation` |
| `ignore` の key が `YOK00[0-9]` / `YOK010` 以外 | `Validation` |
| `plugins` に未知キー | `UnknownKey` |
| 空の `workspace.path` | `Validation` |

存在しないファイルへの entry 指定は Step 2 では **許容**（Step 4 で warning または無視）。

## 8. エッジケースと期待挙動

| シナリオ | 期待結果 |
| --- | --- |
| 設定ファイルが一切無い | `DEFAULTS` の `YokeiConfig`、`sources.used_defaults = true` |
| `[tool.yokei]` が空テーブル `{}` | defaults と同等（空テーブルは layer 追加しない扱いでも可。挙動は defaults と同一であること） |
| `pyproject.toml` に `[tool.yokei]` も `[tool]` 他セクションのみ | `[tool.yokei]` 無し → standalone / defaults のみ |
| `yokei.toml` と `[tool.yokei]` の両方 | pyproject 側が `mode` 等を上書き |
| `mode = "library"` 明示 | `ProjectMode::Library` を保持。auto 解決は Step 8 |
| `entry = ["manage.py"]` | `EntrySpec { path: "manage.py", symbol: None }` |
| `entry = ["src/pkg/asgi.py:app"]` | symbol = `Some("app")` |
| 壊れた TOML | `InvalidToml`、path は該当ファイル |
| `pyproject.toml` 読み取り不可 | `Io` |
| root が `RequirementsTxt` marker のみ | `yokei.toml` / `.yokei.toml` は読む。`uv_workspace` は常に `None` |
| `[tool.yokei.ignore]` に `YOK099` | `Validation` |
| `[tool.yokei.plugins]` に `unknown = true` | `UnknownKey` |

## 9. テスト計画

`src/config/` 内 `#[cfg(test)]` と `tests/config_load.rs` の 2 層。

### 9.1 フィクスチャ構成

```
tests/fixtures/config/
  no_config/                 # marker のみ（pyproject 無し or 空）
  defaults_only/             # pyproject.toml に [project] のみ
  pyproject_full/            # [tool.yokei] 全フィールド例
  yokei_toml_only/           # yokei.toml のみ
  dot_yokei_only/            # .yokei.toml のみ
  merge_priority/            # 3 ソース併存、優先順位検証
  workspace_overrides/       # [tool.yokei.workspaces.*]
  uv_workspace_hint/         # [tool.uv.workspace]
  invalid_mode/              # mode = "bogus"
  invalid_entry/             # 絶対パス entry
  broken_toml/               # 構文エラー
```

各フィクスチャは `discover_project_root` 可能な最小 marker を含める（多くは `pyproject.toml`）。

### 9.2 テストケース一覧

| # | テスト名 | 検証内容 |
| --- | --- | --- |
| T1 | `defaults_when_no_config_files` | zero-config |
| T2 | `loads_pyproject_tool_yokei` | 全フィールド deserialize |
| T3 | `merge_priority_pyproject_wins` | §3.1 優先順位 |
| T4 | `parses_entry_with_symbol` | `path:symbol` |
| T5 | `parses_workspace_overrides` | workspaces マップ |
| T6 | `reads_uv_workspace_hint` | members 保持、未展開 |
| T7 | `rejects_invalid_mode` | Validation |
| T8 | `rejects_unknown_plugin_key` | UnknownKey |
| T9 | `rejects_invalid_ignore_rule` | YOK code 検証 |
| T10 | `apply_overrides_production` | RuntimeOverrides マージ |
| T11 | `invalid_toml_returns_error` | InvalidToml |
| T12 | `io_error_propagates` | permission / missing root file |

### 9.3 カバレッジ目標

- `config/load.rs` + `config/parse.rs` の分岐網羅
- マージロジックは property 的に「高優先ソースが常に勝つ」ことを 3 ソース併存で検証

## 10. 依存関係

| Crate | 用途 | Step 2 で追加 |
| --- | --- | --- |
| `thiserror` | `ConfigError` | 既存 |
| `toml` (>=0.8, <1) | TOML parse | Yes — MIT/Apache-2.0 |
| `serde` (>=1, features derive) | deserialize | Yes（`toml` 経由でも明示依存） |
| `toml_edit` | `--fix` / `--init` | No（Step 13 / CLI） |
| `globset` / `walkdir` | glob 展開 | No（Step 4） |
| `clap` | CLI | No（CLI 統合 PR） |

`serde` の `deny_unknown_fields` を `RawToolYokei` に付与し、未知トップレベルキーを `UnknownKey` に変換する。

## 11. 将来の CLI 統合（参考）

```rust
let root = discover_project_root(start)?;
let loaded = load_config(&root)?;
let mut config = loaded.effective;
apply_overrides(&mut config, &cli.overrides);
// Step 3: manifest extraction using &root, &config
```

`--production` は `RuntimeOverrides { production: Some(true), .. }` で `config.production` を上書き。

## 12. Exit criteria（Step 2 完了定義）

- [x] `src/config/` が `make check` を通過する
- [x] zero-config（設定ファイル無し）で `DEFAULTS` が返るテストがある
- [x] §3.1 の 4 層マージと `ConfigSources` 記録がテストされている
- [x] `[tool.yokei]` の §5 掲載フィールドを deserialize できる
- [x] `load_config` が `pub` API として `lib.rs` から re-export される
- [x] production コードに `unwrap` / `expect` / `panic` がない
- [x] `docs/dev/spec.ja.md` §6 処理順 Step 2 に `config/` モジュール名が追記される（`update-docs`）
- [x] `cargo deny check` が `toml` / `serde` 追加後も通過する

## 13. 実装順序（推奨）

```text
1. Cargo.toml に toml, serde を追加
2. config/error.rs — ConfigError
3. config/types.rs — YokeiConfig, EntrySpec, enums
4. config/defaults.rs — DEFAULTS, merge_layers
5. config/parse.rs — TOML → PartialConfig
6. config/source.rs — ConfigFileSet
7. config/load.rs — load_config, validate, apply_overrides
8. config/mod.rs — re-export
9. lib.rs — pub mod config
10. tests/fixtures + テスト
11. make check
12. update-docs（spec.ja.md §6, AGENTS.md）
```

所要: 新規 Rust ファイル 7、テストフィクスチャ 10 前後、依存 2 crate（`toml`, `serde`）。Step 3 (manifest) とは `pyproject.toml` を両方読むが、責務は `[tool.yokei]` vs `[project]` で分離する。

## 14. 未決事項（Step 2 では保留）

| 項目 | 理由 | 再検討タイミング |
| --- | --- | --- |
| `target_version` を `[project].requires-python` から推定 | manifest 責務 | Step 3 で `requires-python` 読み取り後、`resolve_target_version(config, manifest)` を検討 |
| ネスト `pyproject.toml` による workspace 自動検出 | v0.2 scope | Phase 2 |
| `yokei.toml` と `.yokei.toml` の優先順位をユーザー設定で入れ替え | 需要低 | issue 化 |
| `[tool.yokei]` 以外の Knip 互換 standalone のみ運用 | pyproject 第一候補で足りる | フィードバック後 |
| ~~git worktree での config path~~ | discovery 側で gitfile 対応済み（#6） | **解消** |

## 15. update-plan 検証サマリ

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `docs/dev/plans/step-02-config-load.md` | 本プラン |
| `docs/dev/spec.ja.md` §5, §6 Step 2, §18 ignore | 設定キー・処理順と一致 |
| `docs/dev/plans/step-01-root-discovery.md` | `ProjectRoot` を入力に使用。下方向スキャンは引き続き除外 |
| `AGENTS.md` | `config.rs` は future 記載 → 実装時 `config/` ディレクトリに更新 |
| `src/discovery/` | Step 1 完了、`discover_project_root` 利用可能 |
| `Cargo.toml` | `thiserror` のみ。`toml` / `serde` 追加予定 |
| `deny.toml` | MIT/Apache ライセンス crate は allow リスト内 |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `config/` 単一責務、`PartialConfig` マージで Step 3 と pyproject 読み取りを分離 |
| 静的解析制約 | 20 | 20 | TOML のみ、Python 非実行を維持 |
| ルール / ポリシー | 20 | 18 | ignore 読み込みのみ。マッチングは Step 12。plugin default は §16 準拠 |
| エラー処理 | 20 | 20 | Validation / InvalidToml / Io と exit code 対応が明確 |
| テスト容易性 | 20 | 19 | マージ優先・entry パース・workspace hint を具体化 |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| プランと `spec.ja.md` §5 設定キー | OK |
| プランと `spec.ja.md` §6 処理ステップ 2 | OK — config load のみ、glob 展開・mode 解決は除外 |
| Step 1 `ProjectRoot` との接続 | OK |
| `src/` 現行構成との衝突 | なし — 新規 `config/` 追加 |
| 実装順序の依存関係 | OK — error → types → defaults → parse → load → tests |
| Phase 0 exit（`uvx yokei` 動作） | Step 2 単体では未達。Step 3 縦スライスで manifest 表示まで |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | `AGENTS.md` は `config.rs` 単体記載 | 実装時 `config/` ディレクトリに更新（exit criteria に含む） |
| **P1** | `mode = auto` の解決タイミングが読者に伝わりにくい | §2 Out of scope と §5.1 に Step 8 明記済み |
| **P2** | `WorkspaceOverride` のフィールドが §5 例より少ない | v0.2 拡張として §14 に保留 |

### 確定判定

**合格 — 実装着手可。** Step 2 は Step 1 の `ProjectRoot` にのみ依存し、Step 3 以降へ `YokeiConfig` を渡す縦スライスの第 2 層として独立して並行実装可能。
