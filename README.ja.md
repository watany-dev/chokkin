# chokkin

[English](./README.md)

Pythonプロジェクトの余計なファイル・余計な依存・余計な公開シンボルを検出する。

`chokkin` は、Pythonプロジェクト全体を対象とする reachability analyzer — Python 版の [Knip](https://knip.dev/) 体験を目指すツールです。manifest・ソースコード・各種ツール設定からプロジェクト全体のグラフを構築し、どこからも到達しないものを報告します。`uvx chokkin` で設定なしに実行でき、必要に応じて精密な設定と CI 運用に移行できます。

> [!NOTE]
> **Status: v0.2 development。** デフォルトで **フル分析パイプライン**（ステップ 1–13）が動き、未使用ファイル・依存・シンボルを built-in reporter（`default` / `compact` / `json` / `markdown` / `github` / `sarif`）で報告します。`--explain` / `--trace` / `--fix` / baseline filtering も利用可能です。ステップ 1–4 の概要だけ見る場合は `--probe` を使い、解決済み workspace member 数も確認できます。resolver は member 由来 import に印を付け、cross-member import を first-party として扱います。strict mode は member ごとの依存宣言を要求し、reporter は workspace finding に member id を出します。§17 の CHK002 誤検知ゲートは Phase 1.5 完了後に合格済み（`make oss-metrics ARGS=--gate`）で、**v0.1.0 はリリース済み**です。

## なぜ chokkin か

既存ツールはそれぞれ問題の一部をカバーしています。

```text
Ruff     : ファイル内・構文単位のlint
Vulture  : Python ASTベースのdead code検出
deptry   : Python dependency manifestとimportの整合性検出
chokkin    : project graph全体から未使用ファイル・依存・公開シンボルを検出
```

`chokkin` は style/lint ツールではありません。entry point から実際に到達できるものは何か — そして何がただ置いてあるだけなのか、という別の問いに答えます。そのために `pyproject.toml`、requirements 系、uv/Poetry の lockfile、framework/ツール設定(Django、FastAPI、pytest、tox、nox、pre-commit、GitHub Actions など)を読みます。

## Quick start

```bash
uvx chokkin
```

設定は不要です。初回実行で manifest(`pyproject.toml` / `setup.cfg` / `setup.py` / `requirements*.txt` / `uv.lock`)を探索し、layout(src/flat、tests、scripts、docs)と entry point を推定し、import graph を構築して宣言済み依存と照合します。

```text
chokkin 0.1.0

Project: acme-api
Config : pyproject.toml
Mode   : auto, production=false

Unused dependencies  3
  boto3          pyproject.toml:18  declared in [project.dependencies], no reachable import found
  rich           pyproject.toml:25  only used by scripts/dev.py; move to dependency-groups.dev
  python-dotenv  pyproject.toml:29  no import/config/binary usage found

Missing dependencies  1
  yaml -> PyYAML  src/acme/config.py:3  imported but not declared

Unused files  2
  src/acme/legacy.py        no path from any entry point
  src/acme/old_handlers.py  no path from any entry point

Unused exports  4
  src/acme/utils.py:12  function legacy_slugify
  src/acme/auth.py:44   class OldTokenBackend

Summary: 10 issues
```

## チェック内容

|Code    |種別                     |内容                                                |初期severity                  |
|--------|-----------------------|--------------------------------------------------|----------------------------|
|`CHK001`|`unused_file`          |entry pointから到達しないPython file                     |warning                     |
|`CHK002`|`unused_dependency`    |manifestにあるが、import/config/binaryから利用が確認できない依存    |error                       |
|`CHK003`|`missing_dependency`   |importしているがmanifestに直接宣言されていない依存                  |error                       |
|`CHK004`|`transitive_dependency`|直接importしているが直接依存ではなく、他依存経由に依存している                |error                       |
|`CHK005`|`misplaced_dependency` |runtime codeで使う依存がdev groupにある、またはtest専用依存がmainにある|warning                     |
|`CHK006`|`unused_export`        |module外から参照されない公開シンボル                             |warning                     |
|`CHK007`|`unused_reexport`      |`__init__.py` などの再exportが内部から参照されない               |library: info / app: warning|
|`CHK008`|`unlisted_binary`      |tox/nox/pre-commit/CI等で使うCLIが依存宣言されていない           |warning                     |
|`CHK009`|`duplicate_dependency` |main/dev/optionalに重複宣言されている                       |warning                     |
|`CHK010`|`unresolved_import`    |first-party/third-party/stdlibのいずれにも解決できないimport  |warning                     |

Pythonではmodule top-levelの名前が原則import可能なため、`unused_export` は当初preview rule(library modeではinfo扱い)として提供します。

## CLI

```bash
uvx chokkin
uvx chokkin --production
uvx chokkin --strict
uvx chokkin --no-exit-code
uvx chokkin --include CHK002,CHK003
uvx chokkin --exclude CHK006
uvx chokkin --reporter json
uvx chokkin --reporter markdown
uvx chokkin --reporter github
uvx chokkin --reporter sarif
uvx chokkin --confidence likely
uvx chokkin --fix
uvx chokkin --fix --dry-run
uvx chokkin --baseline chokkin-baseline.json
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --no-cache
uvx chokkin --explain CHK002:boto3
uvx chokkin --trace src/acme/legacy.py
uvx chokkin --probe              # ステップ 1–4 の概要のみ
uvx chokkin --init                # v0.2
```

主要なflag:

- `--production` — dev/test/docs/lint/type contextを解析から外し、runtime contextの到達性だけで判定します。dev専用のファイル・依存は報告対象外になり、逆に「productionで未使用」が厳密に出ます。
- `--strict` — transitive依存の直接importを常にerror、workspace memberごとに直接依存宣言を要求、environment marker付き依存のunusedもerror扱い、confidence `maybe` のissueも表示します。
- `--no-exit-code` — issueがあってもexit codeを0にします(config/CLI errorの2、internal errorの3は維持)。導入初期やGitHub Actions summary用に。
- `--baseline PATH` / `--update-baseline` — 現在のissueをbaseline fileに凍結し、以後の実行では一致するissueを抑制して新規issueだけCIで落とします。
- `--no-cache` — Phase 2 cache の read/write を無効化します。cache policy plumbing は実装済みですが、parse/manifest cache unit は draft です。
- `--reporter github` / `--reporter sarif` — GitHub Actions annotation、または code scanning 用の SARIF 2.1.0 subset を出力します。
- `--probe` — uv / chokkin workspace が検出された場合、解決済み・inventory済み workspace member 数も表示します。
- `--explain` / `--trace` — issueが報告された理由・ファイルが到達可能と判定された経路を表示します。誤検知の調査・報告のための導線です。

exit codeはCI向けに固定です。

```text
0: reportable issueなし
1: issueあり
2: CLI/config error
3: internal error
```

## 設定

デフォルトはzero config。精密さが必要になったら `pyproject.toml` の `[tool.chokkin]` で設定します(`chokkin.toml` / `.chokkin.toml` も使用可)。`chokkin --init` はauto discoveryの結果を反映した `[tool.chokkin]` の雛形を追記します。

```toml
[tool.chokkin]
entry = [
  "src/acme/__main__.py",
  "src/acme/asgi.py:application",
  "manage.py",
]
project = [
  "src/**/*.py",
  "tests/**/*.py",
  "scripts/**/*.py",
]
mode = "auto"             # auto | app | library
production = false
target_version = "py311"  # 解析対象projectのPythonバージョン
respect_gitignore = true
confidence = "likely"     # certain | likely | maybe
exclude = [
  ".venv/**",
  "build/**",
  "dist/**",
  "**/__pycache__/**",
]

[tool.chokkin.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution名 -> import名。bundled mapでカバーされない場合に
[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]

# CLI名 -> distribution名。CHK008/CHK002のbinary usage判定に使う
[tool.chokkin.binary_map]
"sphinx-build" = "Sphinx"

[tool.chokkin.plugins]
pytest = true
django = true
fastapi = true
```

### モード

`mode = "auto"` は次のいずれかを選択します。

- **app mode** — 明確なentry(`console_scripts` / `manage.py` / `asgi.py` / `wsgi.py` / `app.py`)がある場合。unused filesを積極的に報告します。
- **library mode** — `[project] name` とpackageがあり、明確なentryがない場合。public moduleは外部から利用され得るため、unused files/exportsは低confidence(またはinfo)で報告します。libraryで本気のunused file検出をしたい場合は `entry` を明示してください。
- **workspace mode** — 複数の `pyproject.toml` または `tool.uv.workspace.members` がある場合。workspace全体のlockfileを共有しつつ、各memberを個別に解析します(`[tool.chokkin.workspaces.<name>]` でmemberごとの設定が可能)。

### dependency context

依存とファイルの両方にcontext(runtime / dev / test / docs / lint / type / optional extras)を割り当てます。これが `CHK005` の判定根拠です: `tests/` での `import pytest`(pytestがdev groupにある)はOK、`src/` での同じimportはmisplaced dependencyです。`TYPE_CHECKING` 配下のimportはtype context、`try: import orjson / except ImportError` はmissingではなくoptional扱いになります。

## Plugin

frameworkは文字列やdecoratorでmoduleを参照するため、純粋なimport解析では見えません。pluginがentry file・string/module reference・binary usageを追加してこのギャップを埋めます。

- **v0.1**: pytest, django, fastapi/uvicorn
- **v0.2以降**: tox/nox/pre-commit/GitHub Actions の binary usage 検出と Flask/Celery の static app reference 検出は実装中。sphinx, mkdocs, alembic は計画中です。

例えばDjango pluginは `INSTALLED_APPS` / `MIDDLEWARE` / `ROOT_URLCONF` の文字列をmodule referenceとして扱い、`migrations/**` をframework-used扱いにします。FastAPI pluginは `@router.get` 等で修飾されたhandlerをexternally used扱いにします。

## issueの抑制

inline / file-level ignore:

```python
from legacy import old_api  # chokkin: ignore[CHK003]

# chokkin: file-ignore[CHK006]   (ファイル先頭のcomment blockで)
```

config ignore(rule code単位。distribution名glob / path glob / `path:symbol` glob):

```toml
[tool.chokkin.ignore]
CHK001 = ["src/acme/generated/**/*.py"]
CHK002 = ["boto3", "google-cloud-*"]
CHK006 = ["src/acme/public_api.py:*"]
```

大規模な既存projectには、既存issueを凍結して新規issueだけCIで落とすbaselineを使います(v0.2)。

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --baseline chokkin-baseline.json
```

## インストール

`chokkin` はPython wheelに同梱された単一のRust binaryです(Linux/macOS/Windows向けprebuilt wheel)。Rust toolchainなしで以下のすべてが動きます。

```bash
uvx chokkin        # インストールを意識せず実行
pipx run chokkin
pip install chokkin
```

chokkinは解析対象projectのコードを実行しません — 解析は完全にstaticです。projectの仮想環境も必須ではありません: `.venv` があれば dist-info metadata（`METADATA` / `top_level.txt` / `RECORD` / `entry_points.txt`）を読み、なければmanifest・lockfile・bundled mapで解析します。

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md) を参照してください。設計仕様の全文(解析エンジン、import resolution戦略、ロードマップ)は [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) にあります。

## License

[MIT](./LICENSE)
