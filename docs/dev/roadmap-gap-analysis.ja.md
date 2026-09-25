# ロードマップ ギャップ分析 (2026-09, v0.4.1 時点)

`docs/dev/spec.ja.md` §16 / §17 のロードマップを更新するための棚卸し。
元ネタである [Knip](https://knip.dev/) の機能セットと、2026 年時点のモダンな
Python エコシステム (uv / PEP 735 / PEP 723 / PEP 751 / Python 3.14 など) を基準に、
chokkin v0.4.1 に「何があり、何が足りないか」を整理し、優先度を付ける。

優先度の定義:

- **P0** — `uvx chokkin` を 2026 年の典型的な uv プロジェクトで回したとき、誤検知・
  取りこぼしに直結するもの。次の minor で入れる。
- **P1** — Knip 相当の運用性 (導入・抑制・説明・CI 連携) を完成させるもの。
- **P2** — 検出範囲の拡張や将来の契約。preview から始める。

各項目の ID (`R-xx`) は §17 のロードマップと issue 起票で参照する。

## 1. Knip との機能対応

### 1.1 issue 種別

| Knip issue type | chokkin | 状態 | メモ |
|---|---|---|---|
| `files` | CHK001 `unused_file` | ✅ | library mode は低 confidence |
| `dependencies` / `devDependencies` | CHK002 `unused_dependency` | ✅ | dev group は default 抑制、`--strict` で error |
| `optionalPeerDependencies` | CHK002 (optional-dependencies) | ✅ | extras / marker を区別 |
| `unlisted` | CHK003 `missing_dependency` / CHK004 `transitive_dependency` | ✅ | transitive 判定は `uv.lock` のみ (→ R-03) |
| `binaries` | CHK008 `unlisted_binary` | ✅ | 読む設定ファイルの網羅性に課題 (→ R-06) |
| `unresolved` | CHK010 `unresolved_import` | ✅ | |
| `exports` / `nsExports` | CHK006 `unused_export` / CHK007 `unused_reexport` | ✅ preview | public API 宣言手段が弱い (→ R-12) |
| `duplicates` | CHK009 `duplicate_dependency` | ✅ | |
| `types` / `nsTypes` | — | ❌ | Python では「型文脈でのみ使われる export」。CHK006 の細分として検討 (→ R-17) |
| `classMembers` / `enumMembers` | — | ❌ | Vulture 領域。app mode 限定 preview (→ R-16) |
| `catalog` (未使用 catalog entry) | — | ❌ | Python の類似物は `[tool.uv.sources]` / `constraint-dependencies` / `override-dependencies` の stale entry (→ R-15) |
| (Python 固有) stdlib を依存宣言 | — | ❌ | deptry DEP005 相当 (→ R-15) |
| (Knip) configuration hints | — | ❌ | 未使用 ignore・stale baseline・空振り entry glob (→ R-09) |

### 1.2 設定・CLI・運用

| Knip 機能 | chokkin | 状態 | メモ |
|---|---|---|---|
| zero-config 実行 (`npx knip`) | `uvx chokkin` | ✅ | |
| `entry` / `project` / `ignore*` 設定 | `[tool.chokkin]` | ✅ | |
| workspaces / `--isolate-workspaces` | uv workspace + `--strict` | ✅ | |
| `--workspace <name>` で member を絞る | — | ❌ | monorepo の CI 分割に必要 (→ R-10) |
| `--production` / `--strict` | 同名 | ✅ | |
| `--fix` / `--allow-remove-files` / `--fix-type` | `--fix` / `--allow-remove-files` / `--add-missing` | ✅ | fix 種別の選択は `--include` で代替 |
| `--include` / `--exclude` issue type | 同名 (rule code) | ✅ | |
| rules (severity) | `[tool.chokkin.severity]` | ✅ | |
| `--trace` / `--trace-file` / `--trace-export` | `--trace` / `--explain` | ✅ | symbol 単位の trace は未対応 (→ R-12) |
| `--cache` | default on / `--no-cache` | ✅ | |
| `--max-issues` | — | ❌ | baseline と併用した段階導入に有用 (→ R-10) |
| `--performance` / `--memory` / `--debug` | — | ❌ | phase 別 timing を出す (→ R-10) |
| `--include-entry-exports` | — | ❌ | entry file の export も CHK006 対象にする (→ R-12) |
| JSDoc tags (`@public` / `@internal` / `--tags`) | — | ❌ | Python では `__all__` と pragma で代替 (→ R-12) |
| reporters: symbols / compact / json / markdown / github-actions | default / compact / json / markdown / github | ✅ | SARIF は chokkin 独自の上乗せ |
| reporters: codeclimate / codeowners / disclosure | — | ❌ | GitLab Code Quality 向け codeclimate を優先 (→ R-11) |
| custom reporter / preprocessor | JSON schema 公開 | △ | JSON を後段で加工する運用で代替 |
| `--watch` | — | ❌ | LSP と同じ incremental 基盤で提供 (→ R-18) |
| plugin の自動有効化 (enablers) | 既定 on は pytest / django / fastapi のみ | △ | 依存宣言から自動 on にする (→ R-07) |
| 100 超の plugin | 12 plugin + config scanner | △ | framework/tool coverage を拡充 (→ R-08) |
| compilers (`.vue` / `.mdx` など) | `.ipynb` code cell | △ | marimo / Jupytext / Cython など (→ R-08) |
| 外部 plugin | ADR 0002 (RFC のみ) | △ | v1.0 で process boundary (→ R-19) |
| editor 拡張 / language server / MCP | — | ❌ | v1.0 目標 (→ R-18) |
| JSON Schema for config | — | ❌ | `[tool.chokkin]` の schema を SchemaStore に登録 (→ R-11) |

## 2. モダン Python エコシステムとの対応

| 領域 | 仕様 / ツール | chokkin v0.4.1 | 状態 | ID |
|---|---|---|---|---|
| 依存 group | PEP 735 `[dependency-groups]` | group 読み取り・context 判定 | ✅ | |
| | PEP 735 `{include-group = "..."}` | 未展開 | ❌ | R-01 |
| inline script | PEP 723 `# /// script` (`uv run script.py`, `uv add --script`) | 未対応。script の import が project の CHK003 になる | ❌ | R-02 |
| lockfile | `uv.lock` | transitive 判定に利用 | ✅ | |
| | PEP 751 `pylock.toml` / `poetry.lock` / `pdm.lock` | 未読 | ❌ | R-03 |
| uv 設定 | `[tool.uv.sources]` / `constraint-dependencies` / `override-dependencies` / `default-groups` / legacy `dev-dependencies` | workspace member 以外は未読 | ❌ | R-04 |
| build | `[build-system].requires`、`hatch-vcs` / `setuptools-scm` などの build plugin | build context が無い | ❌ | R-05 |
| | wheel に含まれる package (`[tool.hatch.build.targets.wheel]` / `[tool.setuptools.packages.find]`) | layout 推定のみ | △ | R-05 |
| task runner | tox / nox / pre-commit / GitHub Actions / poe / hatch envs | ✅ binary usage | ✅ | |
| | PDM scripts / Makefile / justfile / Dockerfile / Procfile / GitLab CI / compose | 未読 | ❌ | R-06 |
| test | pytest `addopts` の `-p` / `--cov`、`pytest11` entry points | 未読 | ❌ | R-06 |
| type checker | mypy `plugins = [...]`、ty / pyright / basedpyright 設定、`types-*` / `*-stubs` | stub 名判定のみ | △ | R-06 |
| metadata | Core Metadata `Import-Name` / `Import-Namespace` | resolver で参照、harvester は候補生成のみ | △ | R-13 |
| 構文 | PEP 695 generics (3.12)、PEP 750 t-string / PEP 758 (3.14)、PEP 810 `lazy import` (3.15) | rustpython-parser 0.4 は 3.12 以降の構文に追従できない (#140) | ❌ | R-14 |
| framework | Django (settings / apps / migrations)、FastAPI、Flask、Celery | ✅ | ✅ | |
| | Django templatetags / management commands / `admin.py` / `AppConfig.ready`、Typer / Click、Streamlit pages、Airflow / Dagster / Prefect、Scrapy、Litestar、pydantic-settings | 未対応 | ❌ | R-08 |
| notebook | `.ipynb` | code cell 抽出 | ✅ | |
| | marimo (`app = marimo.App()`) / Jupytext | 未対応 | ❌ | R-08 |
| 環境 | conda `environment.yml` / pixi `pixi.toml` | 未読 | ❌ | R-20 |
| 配布 | pre-commit hook (`.pre-commit-hooks.yaml`)、GitHub Action | 無い | ❌ | R-11 |

## 3. 機能バックログ (優先度順)

### P0 — モダン packaging 追従 (v0.5)

- **R-01 PEP 735 `include-group` 展開** — group 間 include を閉包として展開し、
  context 判定と CHK002 / CHK005 / CHK009 に反映する。循環 include は config warning。
- **R-02 PEP 723 inline script metadata** — `# /// script` block を持つ file を
  独立した entry + dependency scope として扱う。script 内 import は script の
  `dependencies` と照合し、project manifest の CHK003 から外す。未使用・不足は
  script 単位で CHK002 / CHK003 として報告する (subject に `script:` prefix)。
  `requires-python` は script ごとの target_version に使う。
- **R-03 lockfile 拡充** — `pylock.toml` (PEP 751)、`poetry.lock`、`pdm.lock` を読み、
  CHK004 の transitive 判定と「lock にあるが宣言にない」の区別に使う。lockfile は
  編集しない (§10 の方針を維持)。
- **R-04 `[tool.uv]` の読み取り** — `sources` (git / path / workspace)、
  `constraint-dependencies` / `override-dependencies`、`default-groups`、legacy
  `dev-dependencies` を manifest として取り込む。`sources` の path/editable 依存は
  first-party 判定の手がかりにする。
- **R-05 build context** — `[build-system].requires` を build context として inventory し、
  CHK002 / CHK003 の対象外にする。wheel target 設定から「配布される package」を
  求め、library mode の CHK001 / CHK006 の public surface 判定に使う。
- **R-06 binary / plugin usage の情報源拡充** — PDM scripts、Makefile / justfile、
  Dockerfile (`RUN` / `CMD` / `ENTRYPOINT`)、Procfile、`.gitlab-ci.yml`、pytest
  `addopts` (`-p plugin`、`--cov` → pytest-cov)、`pytest11` entry points、mypy
  `plugins`、ty / pyright 設定。いずれも静的な文字列抽出に限る。
- **R-07 plugin の自動有効化** — Knip の enabler と同じく、依存宣言
  (`flask` / `celery` / `sphinx` / `mkdocs` / `alembic` ...) や設定ファイルの存在から
  plugin を自動で on にする。明示の `[tool.chokkin.plugins] x = false` が常に優先。
  `--probe` に有効化理由を出す。

### P1 — Knip 相当の運用性 (v0.6)

- **R-08 framework / format plugin 拡充** — Django (templatetags / management
  commands / `admin.py` / `signals.py` / `AppConfig.ready`)、Typer / Click entry、
  Streamlit (`pages/`)、Airflow / Dagster / Prefect の DAG・definition 探索、Scrapy
  spiders、Litestar、marimo notebook、Jupytext paired file。plugin の出力は ADR 0002 の
  4 種の hint に限る。
- **R-09 configuration hints** — Knip の config hints 相当。発火しなかった inline /
  file ignore、baseline の stale fingerprint、使われない `package_module_map` /
  `binary_map` / `ignore` entry、何にもマッチしない `entry` / `project` glob を
  hint として出す。issue ではないので exit code には影響させず、
  `--treat-config-hints-as-errors` 相当のフラグで CI gate 化できるようにする。
- **R-10 monorepo / CI 運用フラグ** — `--workspace <member>` による member 絞り込み、
  `--max-issues N` (N 件以下なら exit 0)、`--performance` による step 別 timing 出力。
- **R-11 配布・連携** — codeclimate reporter (GitLab Code Quality)、chokkin 自身の
  pre-commit hook と GitHub Action、`[tool.chokkin]` の JSON Schema を公開し
  SchemaStore に登録する。
- **R-12 public API の宣言** — `__all__` を公開宣言として CHK006 / CHK007 に反映し、
  `# chokkin: public` pragma と `[tool.chokkin] public = ["pkg.api:*"]` を追加する。
  `--include-entry-exports` 相当と、`--trace pkg/mod.py:symbol` の symbol 単位 trace。
- **R-13 metadata 情報源の昇格** — Core Metadata `Import-Name` /
  `Import-Namespace` を持つ wheel が増えるので、harvester の出力を bundled map の
  更新 pipeline に組み込み、seed との差分をレビューで取り込む。

### P0' — parser 移行 (v0.5〜v0.6 に跨る前提作業)

- **R-14 parser の再選定と移行** — #140 の再評価のとおり、rustpython-parser 0.4 は
  2024-08 以降 release が止まり、PEP 695 generics が AST に届かない。Python 3.14
  (t-string、PEP 758) と 3.15 の `lazy import` (PEP 810) を扱うには移行が必須。
  `ruff_python_parser` へ移行する場合は cross wheel (musl / aarch64) の build 検証と
  lockstep 更新の運用を ADR 0001 の改訂として決める。`lazy import` は通常の import と
  同じ edge にし、import 時副作用の有無は区別しない。

### P2 — 検出範囲の拡張 (v0.7 preview 〜 v1.x)

- **R-15 依存宣言の整合性ルール** — stdlib を依存として宣言している (deptry
  DEP005 相当)、`[tool.uv.sources]` / constraint / override に宣言外の名前がある
  (Knip `catalog` 相当)。新 rule code を割り当て、preview から始める。
- **R-16 unused class / enum members** — app mode 限定の preview。dunder、
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

## 4. 検証 corpus の更新

OSS 20 件の corpus (`scripts/oss-clones.manifest`) は v0.1 時点の選定で、uv-native
project と PEP 723 script を含まない。v0.5 の exit criteria に入る前に次を足す。

- uv workspace + `[dependency-groups]` + `include-group` を使う project を 3 件以上
- PEP 723 script を含む repository を 2 件以上
- `pylock.toml` / `poetry.lock` / `pdm.lock` をそれぞれ持つ project を 1 件以上
- Python 3.12+ 構文 (PEP 695) を使う library を 1 件以上

recall sentinel (`scripts/oss-recall.manifest`) にも R-01〜R-05 の各機能について
「意図的な未使用依存」を持つ fixture を追加し、新しい情報源が全件抑制に退化して
いないことを保証する。

## 5. 参考

- Knip: <https://knip.dev/> (issue types / configuration hints / reporters / plugins)
- PEP 723 Inline script metadata: <https://peps.python.org/pep-0723/>
- PEP 735 Dependency Groups: <https://peps.python.org/pep-0735/>
- PEP 751 A file format to record Python dependencies for installation reproducibility: <https://peps.python.org/pep-0751/>
- PEP 750 Template Strings: <https://peps.python.org/pep-0750/>
- PEP 758 Allow `except` and `except*` expressions without parentheses: <https://peps.python.org/pep-0758/>
- PEP 810 Explicit lazy imports: <https://peps.python.org/pep-0810/>
- uv settings reference: <https://docs.astral.sh/uv/reference/settings/>
- deptry rules: <https://deptry.com/rules-violations/>
