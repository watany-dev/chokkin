# Phase 0: CLI Vertical Slice 設計

解析パイプライン §6 の **Steps 1–4** を CLI から呼び出し、Phase 0 exit criteria
（§17）の「空 project と小規模 sample project で `uvx chokkin` が動く」を満たす縦スライス。
issue 報告・到達性解析は **含めない** — 解析対象の存在を人間可読に示し、exit code を固定する。

> **関連プラン**
>
> - [`step-01-root-discovery.md`](./step-01-root-discovery.md) – Step 1 ✅
> - [`step-02-config-load.md`](./step-02-config-load.md) – Step 2 ✅
> - [`step-03-manifest-extraction.md`](./step-03-manifest-extraction.md) – Step 3 ✅
> - [`step-04-source-file-discovery.md`](./step-04-source-file-discovery.md) – Step 4 ✅
> - [`phase-0-parser-spike-graph-core.md`](./phase-0-parser-spike-graph-core.md) — 任意拡張（parse 件数）

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | `main.rs` が「未実装」で終了する現状を解消し、**zero-config で pipeline 前半が動く**ことを実証する |
| 成果物 | `probe_project(...) -> Result<ProbeReport, ProbeError>` + CLI 統合（`chokkin` / `chokkin --version` / `chokkin --help`） |
| Phase 0 との関係 | §17 exit の **CLI 部分**。wheel / bundled maps は別 PR |
| 後続ステップへの入力 | Phase 1 CLI PR が `ProbeReport` を拡張して `AnalysisReport` に置き換える |

## 2. スコープ

### In scope

- Steps 1–4 のオーケストレーション（`discover_project_root` → `load_config` → `extract_manifest` → `discover_sources`）
- `resolve_target_version` の適用（manifest 後）
- **人間可読サマリ** stdout 出力（§2 UX の簡易版）
- **warnings** の stderr 出力（manifest / sources / config warnings。解析は継続）
- exit code 固定（§2）: 成功 0、設定・CLI エラー 2、内部エラー 3
- `[PATH]` 引数（省略時は `std::env::current_dir()`）
- `--project-root` フラグ（Step 1 プラン §4 の受け口）
- `RuntimeOverrides` の **最小マージ**: `production` のみ（Step 2 で型定義済み）
- `src/pipeline/` モジュール（probe 専用。将来 `analyze` へ拡張）
- `src/cli.rs` — 引数パース（**clap 不使用**。Phase 0 は手動パースで dependency 追加を避ける）

### Out of scope（後続ステップ）

| 項目 | 担当 |
| --- | --- |
| Steps 5–13（plugins / parse 本格 / rules / issues） | Step 5–13 各プラン |
| `--strict` / `--include` / `--reporter` 等のフル CLI | Phase 1 CLI PR |
| `clap` 導入 | Phase 1 CLI PR（flag 数が増えた時点） |
| graph skeleton / parse spike の表示 | 任意拡張（§8.3）。本 PR の必須ではない |
| PyPI タグ付きリリース | Phase 0 wheel PR |
| JSON reporter | Phase 1 |

## 3. 仕様との対応

### 3.1 出力フォーマット（Phase 0 probe）

README の最終 UX ではなく、**解析準備完了**を示す中間フォーマット。

```text
chokkin 0.1.0 (probe)

Project : acme-api
Root    : /path/to/acme-api  (pyproject.toml)
Config  : pyproject.toml [tool.chokkin]
Mode    : auto (unresolved)
Layout  : src (packages: acme)
Target  : py311

Manifest
  dependencies     : 12
  entry points     : 2
  lockfile         : uv.lock (48 nodes)

Sources
  python files      : 34
  stub files (.pyi) : 1
  notebooks (.ipynb): 0
  contexts         : runtime 28, test 5, dev 1

Warnings: 1 (see stderr)

Summary: probe complete — analyzer not run yet
```

**空 project**（依存 0・`.py` 0）でも同フォーマットで表示し、exit 0 とする。

### 3.2 exit code マッピング

| 条件 | `ExitStatus` | code |
| --- | --- | --- |
| probe 成功（issue 未実装のため常にここ） | `Success` | 0 |
| 引数不正・相互排他 | `UsageError` | 2 |
| `DiscoveryError` / `ConfigError` / `ManifestError` / `SourcesError` | `UsageError` | 2 |
| 想定外 panic 以外の内部失敗 | `InternalError` | 3 |

`IssuesFound` (1) は Phase 1 で issue emission 接続後に使用。

### 3.3 warnings の扱い

```text
許可: warning を stderr に列挙し、probe を継続
禁止: warning だけで exit 1 にする（Phase 0）
```

warning 源: `ManifestWarning`, `SourcesWarning`, `ConfigSources` の補足（将来 `PluginsWarning`）。

## 4. モジュール構成

```
src/
  lib.rs              # pub mod pipeline; re-export probe API
  pipeline/
    mod.rs
    probe.rs          # probe_project, ProbeReport
    error.rs          # ProbeError（各 step error の enum ラップ）
    warnings.rs       # 統合 warning 表示ヘルパ
  cli.rs              # parse_cli_args -> CliArgs
  main.rs             # parse → probe → print → exit
```

**方針:** `main.rs` は dispatch のみ。ロジックは `pipeline::probe_project` に集約。

## 5. データ型

### 5.1 `CliArgs`

```rust
/// Parsed CLI invocation (Phase 0 subset).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliArgs {
    /// Path to analyze; `None` means current directory.
    pub path: Option<std::path::PathBuf>,
    /// Explicit project root override (Step 1).
    pub project_root: Option<std::path::PathBuf>,
    /// Runtime overrides from flags.
    pub overrides: RuntimeOverrides,
    /// Print help and exit.
    pub help: bool,
    /// Print version and exit.
    pub version: bool,
}
```

Phase 0 で認識する flag:

```text
-h, --help
-V, --version
--production          -> overrides.production = Some(true)
--project-root <PATH>
[PATH]                -> 位置引数 1 つまで
```

未知の flag は `UsageError`（exit 2）とし、メッセージに `--help` を案内。

### 5.2 `ProbeReport`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub version: &'static str,
    pub root: ProjectRoot,
    pub config_sources: ConfigSources,
    pub effective_config: ChokkinConfig,
    pub manifest: LoadedManifest,
    pub sources: DiscoveredSources,
    pub warnings: Vec<ProbeWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeWarning {
    Manifest(ManifestWarning),
    Sources(SourcesWarning),
    // Config-level soft issues (e.g. missing optional section) — extend as needed
}
```

### 5.3 `ProbeError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Sources(#[from] SourcesError),
    #[error("invalid CLI: {0}")]
    Usage(String),
    #[error("failed to resolve start path: {0}")]
    StartPath(std::io::Error),
}
```

## 6. API

### 6.1 `probe_project`

```rust
/// Run pipeline steps 1–4 and collect a probe report.
///
/// # Errors
///
/// Returns [`ProbeError`] when a pipeline step fails fatally.
pub fn probe_project(start: &Path, overrides: &RuntimeOverrides) -> Result<ProbeReport, ProbeError>
```

**処理順（固定）:**

```text
1. start_path = canonicalize(start)  ※失敗は ProbeError::StartPath
2. root = discover_project_root(&start_path)?
3. loaded = load_config(&root)?
4. config = loaded.effective.clone(); apply_overrides(&mut config, overrides)
5. manifest = extract_manifest(&root, &loaded)?
6. config.target_version = resolve_target_version(&config, &manifest)
7. sources = discover_sources(&root, &loaded, &manifest)?
8. warnings = collect_warnings(&manifest, &sources)
9. Ok(ProbeReport { ... })
```

`--project-root` 指定時は Step 1 の `discover_project_root` 入力をその path に差し替え（Step 1 プラン §4）。

### 6.2 表示関数

```rust
/// Write human-readable probe summary to stdout.
pub fn write_probe_report(report: &ProbeReport, out: &mut impl Write) -> std::io::Result<()>;

/// Write warnings to stderr.
pub fn write_probe_warnings(warnings: &[ProbeWarning], err: &mut impl Write) -> std::io::Result<()>;
```

テストでは `Vec<u8>` writer で golden 比較。

## 7. `main.rs` 統合

```rust
fn main() -> ExitCode {
    let args = match cli::parse_cli_args(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(msg) => { eprintln!("{msg}"); return ExitCode::from(ExitStatus::UsageError.code()); }
    };
    if args.help { println!("{USAGE}"); return ...; }
    if args.version { println!("chokkin {}", chokkin::VERSION); return ...; }

    let start = args.path.as_deref().unwrap_or_else(|| Path::new("."));
    match pipeline::probe_project(start, &args.overrides) {
        Ok(report) => {
            let _ = pipeline::write_probe_report(&report, &mut std::io::stdout());
            let _ = pipeline::write_probe_warnings(&report.warnings, &mut std::io::stderr());
            ExitCode::from(ExitStatus::Success.code())
        }
        Err(ProbeError::Usage(msg)) => { eprintln!("{msg}"); ... UsageError }
        Err(e) => { eprintln!("{e}"); ... UsageError or InternalError }
    }
}
```

`USAGE` 文字列を probe 対応に更新。「analyzer not implemented」文言は削除。

## 8. テスト計画

### 8.1 単体

| テスト | 内容 |
| --- | --- |
| `cli_parse_flags` | `--production`, `--project-root`, 未知 flag |
| `probe_empty_project` | 依存 0 / py 0 fixture |
| `probe_sample_project` | `tests/fixtures/sources/` 相当の小規模 fixture |
| `probe_report_golden` | stdout フォーマット安定 |
| `probe_manifest_error` | 壊れた TOML → `ProbeError::Manifest` |

### 8.2 統合

```text
tests/cli_probe.rs
  - cargo run -- <fixture> が exit 0
  - 壊れた pyproject で exit 2
```

### 8.3 任意拡張（同一 PR または follow-up）

Phase 0 parser/graph がマージ済みの場合、probe 末尾に追記可能:

```text
Graph
  file nodes       : 34
  distributions    : 12
  import edges     : 87  (spike parse)
```

`build_graph_skeleton` + 全 `.py` に `parse_file` は **ベンチ対象外**のため、
`--probe-parse` hidden flag で opt-in にする（デフォルト off）。

## 9. Exit criteria（Phase 0 CLI 完了定義）

- [ ] `src/pipeline/` と `src/cli.rs` が `make check` を通過する
- [ ] `probe_project` が `lib.rs` から re-export される
- [ ] `chokkin`（引数なし）が空 fixture で exit 0 し、Project / Manifest / Sources を表示する
- [ ] `chokkin --help` / `chokkin --version` が動作する
- [ ] 壊れた `pyproject.toml` で exit 2
- [ ] production コードに `unwrap` / `expect` / `panic` がない
- [ ] `main.rs` は dispatch のみ（probe ロジックを含まない）
- [ ] `docs/dev/spec.ja.md` §15 に `pipeline/` / `cli.rs` を追記（`update-docs`）
- [ ] `AGENTS.md` の pre-alpha 説明を「probe まで動作」に更新

## 10. 実装順序（推奨）

```text
1. pipeline/error.rs, warnings.rs
2. pipeline/probe.rs — probe_project
3. pipeline/mod.rs + lib.rs re-export
4. cli.rs — 手動パース
5. main.rs 配線 + USAGE 更新
6. tests/fixtures/probe/* + tests/cli_probe.rs
7. make check
8. update-docs
```

所要: 新規 Rust ファイル 5 前後、fixture 3、依存追加なし。

## 11. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| `clap` 導入時期 | flag 爆発前は手動で十分 | Phase 1 CLI |
| `mode = auto` 解決表示 | Step 8 で `resolve_mode` | probe は `unresolved` 固定 |
| stderr vs stdout for warnings | Knip は issue を stdout | Phase 0 は stderr で区別 |
| `--probe-parse` デフォルト | 大 repo で遅い | opt-in 維持 |

## 12. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `docs/dev/plans/phase-0-cli-vertical-slice.md` | 本プラン |
| `docs/dev/spec.ja.md` §2, §6, §17 | exit criteria と一致 |
| `step-01` – `step-04` | API 確定済み |
| `src/main.rs` | 未実装スタブ — 本 PR で置換 |
| `src/config/types.rs` | `RuntimeOverrides` 確定済み |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `pipeline/` で CLI と分析を分離 |
| 静的解析制約 | 20 | 20 | Steps 1–4 のみ。Python 非実行維持 |
| ルール / ポリシー | 20 | 18 | issue 未実装。exit code 契約は §2 準拠 |
| エラー処理 | 20 | 20 | `ProbeError` で step 別にラップ |
| テスト容易性 | 20 | 19 | golden + fixture 3 |
| **合計** | **100** | **96** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| Steps 1–4 の処理順 | OK |
| `resolve_target_version` の位置 | OK — manifest 直後 |
| Phase 0 exit §17 | 部分 — CLI 本 PR、wheel/maps は別 PR |
| `src/` 衝突 | なし — 新規 `pipeline/`, `cli.rs` |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P1** | parse spike 表示が Phase 0 必須か曖昧 | §8.3 opt-in に分離 |
| **P2** | `mode` 未解決表示 | §11 で Step 8 委譲 |

### 確定判定

**合格 — 実装着手可。** Steps 1–4 のみに依存。parser/graph と並行可能。
