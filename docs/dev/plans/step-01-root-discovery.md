# Step 1: Project Root Discovery 設計

解析パイプライン §6 の **処理ステップ 1 (root discovery)** の実装設計。
Phase 0 の縦スライスとして、CLI から呼べる最小単位の最初の成果物とする。

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | `chokkin` がどの Python プロジェクトを解析対象とするかを、設定なしで決定する |
| 成果物 | `discover_project_root(start)` — 開始ディレクトリから上方向に marker を探索し、最初に見つかったディレクトリを project root として返す |
| Phase 0 との関係 | parser spike / graph core と並行可能。本ステップはファイルシステム走査のみで、新規依存は `thiserror` 1 crate に限定する |
| 後続ステップへの入力 | Step 2 (config load) および Step 3 (manifest extraction) が参照する `ProjectRoot` |

## 2. スコープ

### In scope

- current directory（または将来の CLI 引数で渡されたパス）から **上方向のみ** に marker を探索する（§4）
- marker の優先順位に従い、最初に一致したディレクトリを root として確定する
- 確定した root と、採用した marker 種別、探索開始点を構造化して返す
- 単体テスト可能な pure library API として `src/discovery/` に実装する

### Out of scope（後続ステップ）

| 項目 | 担当ステップ |
| --- | --- |
| workspace member の下方向スキャン | Step 2 (config load) 以降（§4 第 2 段、§5, §8） |
| `pyproject.toml` / `requirements.txt` の内容パース | Step 3 (manifest extraction) |
| `[tool.chokkin]` 設定の読み込み | Step 2 (config load) |
| ソースファイルの glob 探索 | Step 4 (source file discovery) |
| CLI の `[PATH]` 引数・`--project-root` フラグ | Step 1 完了後の CLI 拡張（本設計では API の `start: &Path` で受け口のみ定義） |

## 3. 仕様との対応

§4 の marker 優先順位（上から順に評価、最初のヒットで確定）:

```text
1. pyproject.toml   (file)
2. uv.lock          (file)
3. setup.cfg        (file)
4. setup.py         (file)
5. requirements.txt (file)
6. .git             (directory)
```

探索アルゴリズムの不変条件:

1. **上方向のみ** — `start` 自身を含め、`parent()` を辿って filesystem root まで走査する
2. **最初の marker で確定** — 同一ディレクトリに複数 marker があっても、優先順位の高いものを `RootMarker` として記録する（root パス自体は同じ）
3. **下方向は見ない** — monorepo の subdirectory で実行した場合、そこから上に最初に見つかった marker が root になる（§4 の 2 段階探索の第 1 段）

## 4. モジュール構成

```
src/
  lib.rs              # pub mod discovery; を追加
  discovery/
    mod.rs            # 公開 API と re-export
    root.rs           # discover_project_root 実装
    error.rs          # DiscoveryError
```

`main.rs` は Step 1 では触らない。CLI 統合は Step 1 の exit criteria 達成後に行う（Phase 0 exit の「空 project で動く」は Step 2–3 以降の縦スライスで満たす）。

## 5. データ型

### 5.1 `RootMarker`

採用された marker の種別。レポートや `--explain` で「なぜこの root か」を説明する際に使う。

```rust
/// Marker that determined the project root (§4 priority order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMarker {
    PyProjectToml,
    UvLock,
    SetupCfg,
    SetupPy,
    RequirementsTxt,
    Git,
}
```

各 variant は §4 の優先順位と 1:1 対応する。`Display` / `as_str()` を実装し、reporter 向けに安定した文字列を返す。

### 5.2 `ProjectRoot`

```rust
/// A discovered Python project root directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoot {
    /// Canonical or normalized absolute path to the project root.
    pub path: PathBuf,
    /// Which marker caused this directory to be selected.
    pub marker: RootMarker,
    /// Original `start` argument as passed by the caller (not canonicalized).
    pub start: PathBuf,
}
```

- `path` — 可能なら `std::fs::canonicalize` した絶対パス。失敗時は `std::env::current_dir` 基準で絶対化した `start` を使う（`dunce` は Step 1 では導入しない）
- `start` — `discover_project_root` に渡された生の `PathBuf`（相対パスのまま保持可）。レポートや `--trace` で「どこから探索したか」を示す用途

パス操作はすべて `std::path::{Path, PathBuf}` を使い、手動の `/` 連結やバックスラッシュ前提を避ける（AGENTS.md の cross-platform 制約）。

### 5.3 `DiscoveryError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// No project marker found walking upward from `start`.
    #[error("no project root found from {start}")]
    NotFound { start: PathBuf },

    /// `start` does not exist or is not a directory.
    #[error("invalid start path: {path}")]
    InvalidStart { path: PathBuf },

    /// Filesystem I/O failure during marker probe.
    #[error("failed to read {path}")]
    Io { path: PathBuf, source: std::io::Error },
}
```

- `NotFound` → 将来 CLI では `ExitStatus::UsageError` (2)
- `Io` → 将来 CLI では `ExitStatus::InternalError` (3)（permission エラー等）
- `InvalidStart` → `ExitStatus::UsageError` (2)

`thiserror` は Step 1 で初めて依存に加える。`Display` 実装の手書きを避け、エラーメッセージの一貫性を保つ。

## 6. 公開 API

```rust
// src/discovery/mod.rs

pub use error::DiscoveryError;
pub use root::{ProjectRoot, RootMarker, discover_project_root};

/// Discover the project root by walking upward from `start`.
///
/// Checks markers in §4 priority order at each ancestor directory.
/// Returns [`DiscoveryError::NotFound`] if the filesystem root is reached
/// without a match.
pub fn discover_project_root(start: &Path) -> Result<ProjectRoot, DiscoveryError>;
```

### 内部ヘルパー（`root.rs`、非公開）

```rust
/// Ordered marker probes for a single directory.
fn probe_markers(dir: &Path) -> Result<Option<RootMarker>, DiscoveryError>;

/// Returns true if `path` exists and is a readable directory.
fn is_directory(path: &Path) -> Result<bool, DiscoveryError>;

/// Returns true if `path` exists and is a readable file.
fn is_file(path: &Path) -> Result<bool, DiscoveryError>;
```

`probe_markers` は優先順位どおりに存在チェックし、最初に見つかった `RootMarker` を返す。いずれも存在しなければ `None`。

## 7. アルゴリズム

```text
discover_project_root(start):
  1. start が存在しディレクトリであることを検証。違えば InvalidStart
  2. current ← normalize(start)   # 絶対パス化。canonicalize はベストエフォート
  3. loop:
       a. marker ← probe_markers(current)
       b. if marker is Some(m):
            return ProjectRoot { path: current, marker: m, start: start.to_path_buf() }
       c. if current has no parent (filesystem root):
            return NotFound
       d. current ← parent(current)
```

### 7.1 marker 存在判定

| Marker | 判定 |
| --- | --- |
| `PyProjectToml` | `dir/pyproject.toml` が通常ファイル |
| `UvLock` | `dir/uv.lock` が通常ファイル |
| `SetupCfg` | `dir/setup.cfg` が通常ファイル |
| `SetupPy` | `dir/setup.py` が通常ファイル |
| `RequirementsTxt` | `dir/requirements.txt` が通常ファイル |
| `Git` | `dir/.git` が通常ファイル（gitfile）またはディレクトリ |

git worktree や submodule では `.git` が `gitdir: ...` を書いた **ファイル** になる。§4 はファイル/ディレクトリを区別しないため、どちらも marker として扱う。

### 7.2 パス正規化

```rust
fn normalize_start(start: &Path) -> Result<PathBuf, DiscoveryError> {
    if !start.is_dir() {
        return Err(DiscoveryError::InvalidStart { path: start.to_path_buf() });
    }
    // Best-effort canonicalize; fall back to absolute path
    std::fs::canonicalize(start)
        .or_else(|_| {
            if start.is_absolute() {
                Ok(start.to_path_buf())
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(start))
                    .map_err(|e| DiscoveryError::Io { path: start.to_path_buf(), source: e })
            }
        })
}
```

`std::env::current_dir` 失敗は `Io` として返す（`unwrap` 禁止）。

## 8. エッジケースと期待挙動

| シナリオ | 期待結果 |
| --- | --- |
| `start` = project root（`pyproject.toml` あり） | そのディレクトリ、`PyProjectToml` |
| `start` = `src/pkg/`、root に `pyproject.toml` | root のパス、`PyProjectToml` |
| root に `pyproject.toml` と `requirements.txt` 両方 | `PyProjectToml`（優先順位 1） |
| `requirements.txt` のみ（flat legacy project） | `RequirementsTxt` |
| marker なし（`/tmp` 等） | `NotFound` |
| `start` が存在しないパス | `InvalidStart`（`NotFound` ではない — 探索開始点自体が無効） |
| `start` がファイルパス | `InvalidStart` |
| monorepo `services/api/` に独自 `pyproject.toml` | `services/api/` が root（上位 monorepo root ではない） |
| 空ディレクトリに `uv init` 前 | `NotFound`（`.git` もなければ） |
| 読み取り permission なし | `Io` |

## 9. テスト計画

`src/discovery/root.rs` 内の `#[cfg(test)]` と、`tests/discovery_root.rs` の結合テストの 2 層。

### 9.1 フィクスチャ構成

```
tests/fixtures/discovery/
  pyproject_only/          # pyproject.toml
  uv_lock_only/            # uv.lock
  setup_cfg_only/          # setup.cfg
  setup_py_only/           # setup.py
  requirements_only/       # requirements.txt
  git_only/                # .git/ (git init in test setup)
  nested_src/              # pyproject.toml + src/pkg/module.py
  multi_marker/            # pyproject.toml + requirements.txt
  no_marker/               # empty dir
  monorepo_subdir/         # root/pyproject.toml + pkg/sub/pyproject.toml
```

`git_only` はテスト内で `git init` するか、`.git/HEAD` 等の最小ディレクトリを手動作成する（`git` バイナリ非依存）。

### 9.2 テストケース一覧

| # | テスト名 | 検証内容 |
| --- | --- | --- |
| T1 | `discovers_pyproject_at_start` | start = root |
| T2 | `discovers_pyproject_from_nested_dir` | 上方向走査 |
| T3 | `prefers_pyproject_over_requirements` | marker 優先順位 |
| T4 | `discovers_each_marker` | 6 種 `RootMarker` を `rstest` / table-driven で網羅 |
| T5 | `discovers_git_when_no_manifest` | `.git` fallback |
| T6 | `returns_not_found_for_empty_tree` | `NotFound` |
| T7 | `returns_invalid_start_for_file` | ファイルパス拒否 |
| T8 | `returns_invalid_start_for_missing_path` | 存在しないパス拒否 |
| T9 | `monorepo_subdir_uses_nearest_root` | subdirectory 独自 root |
| T10 | `start_path_preserved_in_result` | `ProjectRoot.start` が入力の生パスのまま保持される |

### 9.3 カバレッジ目標

- `discovery/root.rs` の行カバレッジ 100%（ロジックが単純なため）
- エラーパスは `tempfile` + permission mock（Unix のみ `chmod 000`）で 1 ケース

## 10. 依存関係

| Crate | 用途 | Step 1 で追加 |
| --- | --- | --- |
| `thiserror` (>=2, <3) | `DiscoveryError` | Yes — MIT/Apache-2.0、`deny.toml` allow リスト内 |
| `dunce` | Windows 向け canonicalize | No（Phase 1 で検討） |
| `walkdir` | 下方向スキャン | No（Step 4 以降） |
| `toml` / `toml_edit` | manifest パース | No（Step 3） |

## 11. 将来の CLI 統合（参考）

Step 1 マージ後の CLI 変更案（別 PR）:

```rust
// main.rs（将来。main.rs は print/exit の clippy 免除あり）
let start = args.path.as_deref().unwrap_or(Path::new("."));
match discover_project_root(start) {
    Ok(root) => { /* Step 2 へ */ }
    Err(DiscoveryError::NotFound { .. }) => {
        eprintln!("error: not inside a Python project");
        ExitCode::from(ExitStatus::UsageError.code())
    }
    // ...
}
```

`--version` / `--help` は現状どおり discovery を呼ばない。

## 12. Exit criteria（Step 1 完了定義）

- [x] `src/discovery/` が `make check` を通過する
- [x] §4 の 6 種 marker すべてに対するテストが存在する
- [x] `discover_project_root` が `pub` API として `lib.rs` から re-export される
- [x] production コードに `unwrap` / `expect` / `panic` がない
- [x] `docs/dev/spec.ja.md` §6 処理順 Step 1 と `AGENTS.md` の repository structure に `discovery/` が追記される（`update-docs`）
- [x] `cargo deny check` が `thiserror` 追加後も通過する

## 13. 実装順序（推奨）

```text
1. Cargo.toml に thiserror = "2" を追加
2. discovery/error.rs — DiscoveryError
3. discovery/root.rs — RootMarker, ProjectRoot, discover_project_root
4. discovery/mod.rs — re-export
5. lib.rs — pub mod discovery
6. tests/fixtures + テスト
7. make check
8. update-docs（spec.ja.md §6 に discovery モジュール名を追記）
```

所要: 新規 Rust ファイル 3、テストフィクスチャ 10 前後、依存 1 crate。graph core / parser spike とは独立して並行実装可能。

## 14. 未決事項（Step 1 では保留）

| 項目 | 理由 | 再検討タイミング |
| --- | --- | --- |
| ~~git worktree（`.git` がファイル）~~ | ~~v0.2 保留~~ | **対応済み**（#6） |
| `pyproject.toml` が `[project]` を持たない tool-only ファイル | root としては有効（§4: linters 設定の置き場） | Step 3 で manifest 妥当性を別途検証 |
| 複数 `requirements*.txt` のうち `requirements-dev.txt` を root marker にするか | §4 は `requirements.txt` のみ列挙 | 需要があれば ADR |

## 15. update-plan 検証サマリ

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `docs/dev/plans/step-01-root-discovery.md` | 本プラン |
| `docs/dev/spec.ja.md` §4, §6 | marker 優先順位・処理ステップ 1 と一致 |
| `AGENTS.md` | `discovery/` 未記載 → 実装時 `update-docs` で追記（exit criteria に含む） |
| `Cargo.toml` / `pyproject.toml` | 現状 `[dependencies]` 空。`ExitStatus` は `lib.rs` に定義済み |
| `src/lib.rs` / `src/main.rs` | analyzer 未実装。`main.rs` は `--help` / `--version` のみ |
| `deny.toml` | `thiserror` (MIT/Apache-2.0) は allow リスト内 |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 20 | `discovery/` 単一責務、`ProjectRoot.start` のセマンティクスを明確化 |
| 静的解析制約 | 20 | 20 | ファイル存在チェックのみ、Python 非実行を維持 |
| ルール / ポリシー | 20 | 19 | YOK ルール未着手。§4 marker 優先順位は忠実（gitfile 除外は文書化済み） |
| エラー処理 | 20 | 20 | `InvalidStart` / `NotFound` / `Io` の分岐と exit code 対応が明確 |
| テスト容易性 | 20 | 20 | 6 marker 網羅・monorepo・permission エラーを具体化 |
| **合計** | **100** | **99** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| プランと `spec.ja.md` §4 marker 順序 | OK |
| プランと `spec.ja.md` §6 処理ステップ 1 | OK — root discovery のみ、下方向スキャンは除外 |
| シンボル名と既存 `ExitStatus` | OK — `UsageError`(2) / `InternalError`(3) と対応表一致 |
| `src/` 現行構成との衝突 | なし — 新規 `discovery/` 追加のみ |
| 実装順序の依存関係 | OK — error → root → mod → lib → tests → docs |
| Phase 0 exit（`uvx chokkin` 動作） | Step 1 単体では未達。Step 2–3 縦スライスで達成（§4 明記済み） |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P0** | §1「外部 crate 依存を増やさない」と `thiserror` 追加が矛盾 | §1 を「`thiserror` 1 crate に限定」に修正 |
| **P0** | §7.1 `.git` marker 判定の記述矛盾（directory / gitfile） | ファイル/ディレクトリ両対応に修正（#6） |
| **P1** | `ProjectRoot.start` が正規化前か後か曖昧 | 呼び出し元の生パスを保持と明記、アルゴリズム疑似コード修正 |
| **P1** | テスト表 T4 の番号・網羅性が不明確 | T4 を 6 marker parametrize、T8/T10 を追加 |
| **P1** | `thiserror` バージョン・`deny` 通過が未記載 | `>=2, <3` と exit criteria に `cargo deny` を追加 |
| **P1** | `update-docs` 対象が `spec.ja.md` のみ | `AGENTS.md` を exit criteria に追加 |
| **P2** | CLI 統合例の `unwrap` が clippy 方針と誤解されうる | `main.rs` 免除をコメントで明記 |

### 確定判定

**合格 — 実装着手可。** 残る未決事項（§14）はすべて v0.2 以降または後続ステップに委譲され、Step 1 のスコープを侵食しない。
