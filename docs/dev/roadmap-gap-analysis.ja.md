# ロードマップ ギャップ分析 (2026-10, v0.7.2 時点)

> 初版は 2026-09 に v0.4.1 (v0.4.0 後の main。v0.4.1 はリリースされず v0.5.0 に含まれた) を
> 基準に作った。本版は v0.7.2 後の main (`8ebad05`) と 2026-10 時点の Knip
> リファレンス (<https://knip.dev/reference/issue-types>、<https://knip.dev/reference/cli>)
> を基準に更新した。
>
> v0.6 / v0.7 は実 OSS での精度と monorepo 対応に充てたため (spec §17 Phase 4.5)、P1 は v0.8、P2 は v0.9 preview に繰り下げた。

`docs/dev/spec.ja.md` §16 / §17 のロードマップを更新するための棚卸し。
元ネタである [Knip](https://knip.dev/) の機能セットと、2026 年時点のモダンな
Python エコシステム (uv / PEP 735 / PEP 723 / PEP 751 / Python 3.14 など) を基準に、
chokkin に「何があり、何が足りないか」を整理し、優先度を付ける。

優先度の定義:

- **P0** — `uvx chokkin` を 2026 年の典型的な uv プロジェクトで回したとき、誤検知・
  取りこぼしに直結するもの。次の minor で入れる。
- **P1** — Knip 相当の運用性 (導入・抑制・説明・CI 連携) を完成させるもの。
- **P2** — 検出範囲の拡張や将来の契約。preview から始める。

各項目の ID (`R-xx`) は §17 のロードマップと issue 起票で参照する。

## 0. 前回 (v0.4.1) からの変化

- P0 (R-01〜R-07) と parser 移行 (R-14) は v0.5.0 で完了した (spec §17 Phase 4)。
- v0.6 / v0.7 は backlog 外の精度改善に使った: auto-workspace (#488)、library mode の
  公開 API 判定 (#489, #525)、in-package tests / vendored (#490)、静的 `setup.py`
  評価 (#539)、`--fix` の stale 行検出 (#436-#438)、alembic `script_location` (#667) など。
- 残りは P1 (R-08〜R-13 のうち未完分) と P2 (R-15〜R-20)。
- Knip 側の issue 種別が変わった。`duplicates` は「同じものを複数回 export している」
  (duplicate exports) であり、初版で CHK009 (duplicate dependency) に対応付けていたのは
  誤り。`catalogReferences` / `namespaceMembers` / `cycles` が追加され、リファレンス
  から `classMembers` が消えた。sarif reporter も Knip 本体に入った。

## 1. Knip との機能対応

### 1.1 issue 種別

| Knip issue type (既定) | chokkin | 状態 | メモ |
|---|---|---|---|
| `files` (on) | CHK001 `unused_file` | ✅ | library mode / library 扱いの workspace member は `maybe` |
| `dependencies` / devDependencies (on) | CHK002 `unused_dependency` / CHK005 `misplaced_dependency` | ✅ | dev group は default 抑制、`--strict` で error。context の取り違えは CHK005 |
| optional peerDependencies (on) | CHK002 (optional-dependencies) | ✅ | extras / marker を区別 |
| `unlisted` (on) | CHK003 `missing_dependency` / CHK004 `transitive_dependency` | ✅ | transitive 判定は `uv.lock` / `pylock.toml` / `poetry.lock` / `pdm.lock`。workspace member は自身の lockfile (#653) |
| `binaries` (on) | CHK008 `unlisted_binary` | ✅ | 情報源は R-06 で拡充済み |
| `unresolved` (on) | CHK010 `unresolved_import` | ✅ | `TYPE_CHECKING` / optional import は info (#654) |
| `exports` / `nsExports` (on / off) | CHK006 `unused_export` / CHK007 `unused_reexport` | ✅ preview | `__all__` と wheel target は公開宣言として扱う。pragma / 設定での宣言は未対応 (→ R-12) |
| `types` / `nsTypes` (on / off) | — | ❌ | Python では「型文脈でのみ使われる export」。CHK006 の細分として検討 (→ R-17) |
| `enumMembers` (on) | — | ❌ | Vulture 領域。app mode 限定 preview (→ R-16) |
| `namespaceMembers` (on) | — | 対象外 | TypeScript の `namespace` 宣言。Python に対応する構文がない |
| `duplicates` (on) | — | ❌ | duplicate exports。Python では同じ object を別名で再 export している `__init__.py` (`from .a import X` と `from .a import X as Y`) が該当 (→ R-21) |
| `catalog` (on) | — | ❌ | `[tool.uv.sources]` / `constraint-dependencies` / `override-dependencies` の stale entry (→ R-15) |
| `catalogReferences` (on) | — | ❌ | `{ workspace = true }` が存在しない member を指す、など。R-15 に含める |
| `cycles` (off) | — | ❌ | 循環 import。Python では実行時に壊れる場合とそうでない場合がある (→ R-22) |
| (Python 固有) 依存の重複宣言 | CHK009 `duplicate_dependency` | ✅ | Knip に相当なし (Knip の `duplicates` とは別物) |
| (Python 固有) stdlib を依存宣言 | — | ❌ | deptry DEP005 相当 (→ R-15) |
| (Knip) configuration hints | — | ❌ | 未使用 ignore・stale baseline・空振り entry glob (→ R-09) |

前回の版にあった `classMembers` は、2026-10 時点の Knip リファレンスに載っていない。
R-16 は enum members を主にし、class members は Vulture との棲み分けを見て判断する。

### 1.2 設定・CLI・運用

| Knip 機能 | chokkin | 状態 | メモ |
|---|---|---|---|
| zero-config 実行 (`npx knip`) | `uvx chokkin` | ✅ | |
| `entry` / `project` / `ignore*` 設定 | `[tool.chokkin]` | ✅ | vendored code の除外 (#490) も持つ |
| workspaces / `--isolate-workspaces` | uv workspace / auto-workspace (#488) + `--strict` | ✅ | `--no-auto-workspace` で単一 project 解析 |
| `--workspace <filter>` で member を絞る | — | ❌ | monorepo の CI 分割に必要 (→ R-10) |
| `--directory` | 位置引数 `PATH` | ✅ | |
| `--production` / `--strict` | 同名 | ✅ | |
| `--fix` / `--allow-remove-files` / `--fix-type` | `--fix` / `--allow-remove-files` / `--add-missing` / `--dry-run` | ✅ | fix 種別の選択は `--include` で代替 |
| `--format` (fix 後に formatter を実行) | — | 対象外 | chokkin は元の書式を保って編集する |
| `--include` / `--exclude` / `--dependencies` / `--exports` / `--files` | `--include` / `--exclude` (rule code) | ✅ | shortcut flag は無いが rule code の列挙で足りる |
| rules (severity) | `[tool.chokkin.severity]` | ✅ | |
| `--trace` / `--trace-file` / `--trace-dependency` | `--trace` / `--explain` | ✅ | |
| `--trace-export` | — | ❌ | symbol 単位の trace (→ R-12) |
| `--cache` | default on / `--no-cache` | ✅ | |
| `--max-issues` / `--max-show-issues` / `--no-exit-code` | — | ❌ | baseline と併用した段階導入に有用 (→ R-10) |
| `--performance` / `--memory` / `--duration` / `--debug` | — | ❌ | phase 別 timing を出す (→ R-10) |
| `--include-entry-exports` | — | ❌ | entry file の export も CHK006 対象にする (→ R-12) |
| JSDoc tags (`@public` / `@internal` / `--tags`) | `__all__` | △ | `# chokkin: public` pragma と `public` 設定は未対応 (→ R-12) |
| config hints / tag hints (`--treat-config-hints-as-errors`) | — | ❌ | (→ R-09) |
| reporters: symbols / compact / json / markdown / github-actions / sarif | default / compact / json / markdown / github / sarif | ✅ | |
| reporters: codeclimate / codeowners / disclosure | — | ❌ | GitLab Code Quality 向け codeclimate を優先 (→ R-11) |
| custom reporter / preprocessor | JSON schema 公開 | △ | JSON を後段で加工する運用で代替 |
| `--watch` | — | ❌ | LSP と同じ incremental 基盤で提供 (→ R-18) |
| plugin の自動有効化 (enablers) | 依存宣言・設定ファイルから自動 on (R-07) | ✅ | `--probe` に有効化理由を出す |
| 100 超の plugin | 11 plugin (pytest / django / fastapi / flask / celery / tox / nox / pre-commit / sphinx / mkdocs / alembic) + config scanner | △ | framework coverage を拡充 (→ R-08) |
| compilers (`.vue` / `.mdx` など) | `.ipynb` code cell | △ | marimo / Jupytext / Cython など (→ R-08) |
| 外部 plugin | ADR 0002 (RFC のみ) | △ | v1.0 で process boundary (→ R-19) |
| editor 拡張 / language server / MCP | — | ❌ | v1.0 目標 (→ R-18) |
| CI 連携 | GitHub Action (`action.yml`) | △ | chokkin 自身の pre-commit hook (`.pre-commit-hooks.yaml`) は無い (→ R-11) |
| JSON Schema for config | — | ❌ | report / baseline の schema はあるが `[tool.chokkin]` の schema は無い (→ R-11) |

## 2. モダン Python エコシステムとの対応

| 領域 | 仕様 / ツール | chokkin v0.7.2 | 状態 | ID |
|---|---|---|---|---|
| 依存 group | PEP 735 `[dependency-groups]` / `{include-group = "..."}` | group 読み取り・include 展開・context 判定 | ✅ | R-01 |
| inline script | PEP 723 `# /// script` | script 単位の CHK002 / CHK003 (subject `script:`) | ✅ | R-02 |
| lockfile | `uv.lock` / PEP 751 `pylock.toml` / `poetry.lock` / `pdm.lock` | transitive 判定に利用 (優先順 uv.lock > pylock > poetry > pdm)。member は自身の lockfile | ✅ | R-03 |
| uv 設定 | `[tool.uv.sources]` / `constraint-dependencies` / `override-dependencies` / `default-groups` / legacy `dev-dependencies` / `[tool.uv.workspace] exclude` | manifest として読む。path source は workspace member として扱う | ✅ | R-04 |
| build | `[build-system].requires`、build plugin、wheel target | build context と public surface (`hatch` / `setuptools` / build-backend の package dir) | ✅ | R-05 |
| | `[project].dynamic` の依存 (`uv-dynamic-versioning`)、動的 `setup.py` | 静的評価で読む (#539, #590) | ✅ | |
| task runner | tox / nox / pre-commit / GitHub Actions / poe / hatch envs / PDM scripts / Makefile / justfile / Dockerfile / Procfile / GitLab CI | binary usage | ✅ | R-06 |
| test | pytest `addopts` の `-p` / `--cov`、`pytest11`、`[tool.pytest]` (pytest ≥ 9)、`testpaths` / `python_files` | 読む (#544, #603) | ✅ | R-06 |
| type checker | mypy `plugins`、ty / pyright / basedpyright 設定、`types-*` / `*-stubs` | 読む | ✅ | R-06 |
| metadata | Core Metadata `Import-Name` / `Import-Namespace` | resolver で参照、harvester は候補生成のみ | △ | R-13 |
| 構文 | PEP 695 (3.12)、PEP 750 t-string / PEP 758 (3.14)、PEP 810 `lazy import` (3.15) | `ruff_python_parser =0.0.15` で parse。`lazy import` は通常の import edge | ✅ | R-14 |
| framework | Django (settings / `INSTALLED_APPS` / `AppConfig` / migrations)、FastAPI、Flask、Celery、Alembic (`script_location` / revision) | ✅ | ✅ | |
| | Click / Typer | `@command` decorator を外部使用として扱う | △ | R-08 |
| | Django templatetags / management commands / `admin.py` / `signals.py`、Streamlit pages、Airflow / Dagster / Prefect、Scrapy、Litestar、pydantic-settings | 専用 plugin は無い | ❌ | R-08 |
| notebook | `.ipynb` | code cell 抽出・entry root | ✅ | |
| | marimo (`app = marimo.App()`) / Jupytext | 未対応 | ❌ | R-08 |
| 環境 | conda `environment.yml` / pixi `pixi.toml` | 未読 | ❌ | R-20 |
| 配布 | GitHub Action | `action.yml` | ✅ | R-11 |
| | pre-commit hook (`.pre-commit-hooks.yaml`) | 無い | ❌ | R-11 |

## 3. 機能バックログ (優先度順)

### 完了

| ID | 内容 | リリース |
|---|---|---|
| R-01 | PEP 735 `include-group` 展開 | v0.5.0 |
| R-02 | PEP 723 inline script metadata | v0.5.0 |
| R-03 | `pylock.toml` / `poetry.lock` / `pdm.lock` | v0.5.0 |
| R-04 | `[tool.uv]` の読み取り | v0.5.0 |
| R-05 | build context と wheel target の public surface | v0.5.0 |
| R-06 | binary / plugin usage の情報源拡充 | v0.5.0 |
| R-07 | plugin の自動有効化 | v0.5.0 |
| R-14 | parser の `ruff_python_parser` への移行 (ADR 0001 改訂) | v0.5.0 |

各項目の詳細は spec §17 Phase 4 と CHANGELOG を参照。

### P1 — Knip 相当の運用性 (v0.8)

- **R-08 framework / format plugin 拡充** — Django (templatetags / management
  commands / `admin.py` / `signals.py`)、Typer / Click の entry、
  Streamlit (`pages/`)、Airflow / Dagster / Prefect の DAG・definition 探索、Scrapy
  spiders、Litestar、marimo notebook、Jupytext paired file。plugin の出力は ADR 0002 の
  4 種の hint に限る。Django `AppConfig` と alembic は v0.7 系で対応済み。
- **R-09 configuration hints** — Knip の config hints 相当。発火しなかった inline /
  file ignore、baseline の stale fingerprint、使われない `package_module_map` /
  `binary_map` / `ignore` entry、何にもマッチしない `entry` / `project` glob を
  hint として出す。issue ではないので exit code には影響させず、
  `--treat-config-hints-as-errors` 相当のフラグで CI gate 化できるようにする。
  ignore が増え続けるのを防ぐ機能なので、P1 の中で最優先にする。
- **R-10 monorepo / CI 運用フラグ** — `--workspace <member>` による member 絞り込み、
  `--max-issues N` (N 件以下なら exit 0)、`--performance` による step 別 timing 出力。
  Knip の `--max-show-issues` / `--no-exit-code` も同じ枠で検討する。
- **R-11 配布・連携** — codeclimate reporter (GitLab Code Quality)、chokkin 自身の
  pre-commit hook、`[tool.chokkin]` の JSON Schema を公開し SchemaStore に登録する。
  GitHub Action は v0.7.1 までに提供済み。
- **R-12 public API の宣言** — `__all__` は CHK006 / CHK007 に反映済み (#489)。
  残りは `# chokkin: public` pragma、`[tool.chokkin] public = ["pkg.api:*"]`、
  `--include-entry-exports` 相当、`--trace pkg/mod.py:symbol` の symbol 単位 trace。
  CHK006 を preview から外す条件にする。
- **R-13 metadata 情報源の昇格** — Core Metadata `Import-Name` /
  `Import-Namespace` を持つ wheel が増えるので、harvester の出力を bundled map の
  更新 pipeline に組み込み、seed との差分をレビューで取り込む。

### P2 — 検出範囲の拡張 (v0.9 preview 〜 v1.x)

- **R-15 依存宣言の整合性ルール** — stdlib を依存として宣言している (deptry
  DEP005 相当)、`[tool.uv.sources]` / constraint / override に宣言外の名前がある
  (Knip `catalog` 相当)、`{ workspace = true }` などが存在しない対象を指す
  (Knip `catalogReferences` 相当)。新 rule code を割り当て、preview から始める。
  stdlib 宣言は誤検知の余地が小さいので、v1.0 で rule 一覧を凍結する前に入れるか
  決める。
- **R-16 unused enum / class members** — app mode 限定の preview。dunder、
  decorator 付き、framework hook (Django model methods など) は除外する。
- **R-17 型文脈専用 export** — `TYPE_CHECKING` 下や annotation でしか使われない
  export を CHK006 から区別する (Knip `types` 相当)。
- **R-18 editor 連携** — `--watch`、LSP (diagnostics + code action で `--fix` を適用)、
  AI エージェント向けの MCP server。いずれも incremental 解析 (cache 基盤の再利用) を
  前提とする。
- **R-19 外部 plugin loading** — ADR 0002 の process boundary (JSON stdin/stdout) を
  実装する。WASM は需要を見て判断する。
- **R-20 conda / pixi** — `environment.yml` / `pixi.toml` の pypi 依存を manifest として
  読む。conda 専用 package は resolver で unknown 扱いにする。
- **R-21 duplicate exports (新規)** — Knip `duplicates` 相当。同じ module の
  `__init__.py` が同じ object を複数の名前で再 export しているものを報告する。
  後方互換のための alias が多いので、library mode では info から始める。
- **R-22 循環 import (新規)** — Knip `cycles` 相当 (Knip でも既定 off)。
  `TYPE_CHECKING` 下・関数内・`lazy import` の edge を除いた、module top-level の
  import だけで閉じる cycle を報告する。既定 off の preview にする。

## 4. 検証 corpus の更新

初版で挙げた追加 (uv workspace + `include-group`、PEP 723 script、各 lockfile、
PEP 695 を使う library、R-01〜R-05 の recall fixture) は v0.5.0 までに入った
(`docs/dev/oss-validation-report.md`)。v0.7.1 の release validation は
`scripts/oss-clones.manifest` の 35 project と `scripts/oss-recall.manifest` の
recall fixture 10 件 (計 45) で測った (`docs/dev/v0.7.1-release-validation.md`)。

v0.8 の exit criteria に入る前に次を足す。

- R-08 の対象 framework (Django management commands / templatetags、Typer / Click、
  Streamlit、Airflow / Dagster / Prefect、Scrapy、marimo) を使う project を各 1 件以上
- R-09 の hint を検証するため、ignore / baseline / `package_module_map` を実際に
  使っている project を 2 件以上
- R-12 の pragma / `public` 設定を入れた recall fixture (宣言した symbol が CHK006 に
  出ないこと、宣言していない未使用 symbol は出ること)

## 5. 参考

- Knip: <https://knip.dev/> (issue types / configuration hints / reporters / plugins)
- Knip issue types: <https://knip.dev/reference/issue-types>
- Knip CLI: <https://knip.dev/reference/cli>
- PEP 723 Inline script metadata: <https://peps.python.org/pep-0723/>
- PEP 735 Dependency Groups: <https://peps.python.org/pep-0735/>
- PEP 751 A file format to record Python dependencies for installation reproducibility: <https://peps.python.org/pep-0751/>
- PEP 750 Template Strings: <https://peps.python.org/pep-0750/>
- PEP 758 Allow `except` and `except*` expressions without parentheses: <https://peps.python.org/pep-0758/>
- PEP 810 Explicit lazy imports: <https://peps.python.org/pep-0810/>
- uv settings reference: <https://docs.astral.sh/uv/reference/settings/>
- deptry rules: <https://deptry.com/rules-violations/>
