# yokei

[English](./README.md)

Pythonプロジェクトの余計なファイル・余計な依存・余計な公開シンボルを検出する。

`yokei` は、Pythonプロジェクト全体を対象とする reachability analyzer — Python 版の [Knip](https://knip.dev/) 体験を目指すツールです。manifest・ソースコード・各種ツール設定からプロジェクト全体のグラフを構築し、どこからも到達しないものを報告します。`uvx yokei` で設定なしに実行でき、必要に応じて精密な設定と CI 運用に移行できます。

> [!WARNING]
> **Status: pre-alpha。** 処理ステップ 1–2（`src/discovery/` の project root discovery と `src/config/` の `[tool.yokei]` config load）はライブラリ API として実装済みです。CLI 解析と issue 報告は未接続です。この README は設計済みの挙動を記述しており、仕様の全文は [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) にあります。以下のコマンドと出力は目指す UX を示すものです。

## なぜ yokei か

既存ツールはそれぞれ問題の一部をカバーしています。

```text
Ruff     : ファイル内・構文単位のlint
Vulture  : Python ASTベースのdead code検出
deptry   : Python dependency manifestとimportの整合性検出
yokei    : project graph全体から未使用ファイル・依存・公開シンボルを検出
```

`yokei` は style/lint ツールではありません。entry point から実際に到達できるものは何か — そして何がただ置いてあるだけなのか、という別の問いに答えます。そのために `pyproject.toml`、requirements 系、uv/Poetry の lockfile、framework/ツール設定(Django、FastAPI、pytest、tox、nox、pre-commit、GitHub Actions など)を読みます。

## Quick start

```bash
uvx yokei
```

設定は不要です。初回実行で manifest(`pyproject.toml` / `setup.cfg` / `setup.py` / `requirements*.txt` / `uv.lock`)を探索し、layout(src/flat、tests、scripts、docs)と entry point を推定し、import graph を構築して宣言済み依存と照合します。

```text
yokei 0.1.0

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
|`YOK001`|`unused_file`          |entry pointから到達しないPython file                     |warning                     |
|`YOK002`|`unused_dependency`    |manifestにあるが、import/config/binaryから利用が確認できない依存    |error                       |
|`YOK003`|`missing_dependency`   |importしているがmanifestに直接宣言されていない依存                  |error                       |
|`YOK004`|`transitive_dependency`|直接importしているが直接依存ではなく、他依存経由に依存している                |error                       |
|`YOK005`|`misplaced_dependency` |runtime codeで使う依存がdev groupにある、またはtest専用依存がmainにある|warning                     |
|`YOK006`|`unused_export`        |module外から参照されない公開シンボル                             |warning                     |
|`YOK007`|`unused_reexport`      |`__init__.py` などの再exportが内部から参照されない               |library: info / app: warning|
|`YOK008`|`unlisted_binary`      |tox/nox/pre-commit/CI等で使うCLIが依存宣言されていない           |warning                     |
|`YOK009`|`duplicate_dependency` |main/dev/optionalに重複宣言されている                       |warning                     |
|`YOK010`|`unresolved_import`    |first-party/third-party/stdlibのいずれにも解決できないimport  |warning                     |

Pythonではmodule top-levelの名前が原則import可能なため、`unused_export` は当初preview rule(library modeではinfo扱い)として提供します。

## CLI

```bash
uvx yokei
uvx yokei --production
uvx yokei --strict
uvx yokei --fix
uvx yokei --fix --allow-remove-files
uvx yokei --include dependencies,files
uvx yokei --exclude exports
uvx yokei --reporter json
uvx yokei --reporter sarif   # v0.2
uvx yokei --no-exit-code
uvx yokei --explain YOK002:boto3
uvx yokei --trace src/acme/legacy.py
uvx yokei --init
```

主要なflag:

- `--production` — dev/test/docs/lint/type contextを解析から外し、runtime contextの到達性だけで判定します。dev専用のファイル・依存は報告対象外になり、逆に「productionで未使用」が厳密に出ます。
- `--strict` — transitive依存の直接importを常にerror、workspace memberごとに直接依存宣言を要求、environment marker付き依存のunusedもerror扱い、confidence `maybe` のissueも表示します。
- `--no-exit-code` — issueがあってもexit codeを0にします(config/CLI errorの2、internal errorの3は維持)。導入初期やGitHub Actions summary用に。
- `--explain` / `--trace` — issueが報告された理由・ファイルが到達可能と判定された経路を表示します。誤検知の調査・報告のための導線です。

exit codeはCI向けに固定です。

```text
0: reportable issueなし
1: issueあり
2: CLI/config error
3: internal error
```

## 設定

デフォルトはzero config。精密さが必要になったら `pyproject.toml` の `[tool.yokei]` で設定します(`yokei.toml` / `.yokei.toml` も使用可)。`yokei --init` はauto discoveryの結果を反映した `[tool.yokei]` の雛形を追記します。

```toml
[tool.yokei]
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

[tool.yokei.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution名 -> import名。bundled mapでカバーされない場合に
[tool.yokei.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]

# CLI名 -> distribution名。YOK008/YOK002のbinary usage判定に使う
[tool.yokei.binary_map]
"sphinx-build" = "Sphinx"

[tool.yokei.plugins]
pytest = true
django = true
fastapi = true
```

### モード

`mode = "auto"` は次のいずれかを選択します。

- **app mode** — 明確なentry(`console_scripts` / `manage.py` / `asgi.py` / `wsgi.py` / `app.py`)がある場合。unused filesを積極的に報告します。
- **library mode** — `[project] name` とpackageがあり、明確なentryがない場合。public moduleは外部から利用され得るため、unused files/exportsは低confidence(またはinfo)で報告します。libraryで本気のunused file検出をしたい場合は `entry` を明示してください。
- **workspace mode** — 複数の `pyproject.toml` または `tool.uv.workspace.members` がある場合。workspace全体のlockfileを共有しつつ、各memberを個別に解析します(`[tool.yokei.workspaces.<name>]` でmemberごとの設定が可能)。

### dependency context

依存とファイルの両方にcontext(runtime / dev / test / docs / lint / type / optional extras)を割り当てます。これが `YOK005` の判定根拠です: `tests/` での `import pytest`(pytestがdev groupにある)はOK、`src/` での同じimportはmisplaced dependencyです。`TYPE_CHECKING` 配下のimportはtype context、`try: import orjson / except ImportError` はmissingではなくoptional扱いになります。

## Plugin

frameworkは文字列やdecoratorでmoduleを参照するため、純粋なimport解析では見えません。pluginがentry file・string/module reference・binary usageを追加してこのギャップを埋めます。

- **v0.1**: pytest, django, fastapi/uvicorn
- **v0.2以降**: flask, celery, tox, nox, pre-commit, github-actions, sphinx, mkdocs, alembic

例えばDjango pluginは `INSTALLED_APPS` / `MIDDLEWARE` / `ROOT_URLCONF` の文字列をmodule referenceとして扱い、`migrations/**` をframework-used扱いにします。FastAPI pluginは `@router.get` 等で修飾されたhandlerをexternally used扱いにします。

## issueの抑制

inline / file-level ignore:

```python
from legacy import old_api  # yokei: ignore[YOK003]

# yokei: file-ignore[YOK006]   (ファイル先頭のcomment blockで)
```

config ignore(rule code単位。distribution名glob / path glob / `path:symbol` glob):

```toml
[tool.yokei.ignore]
YOK001 = ["src/acme/generated/**/*.py"]
YOK002 = ["boto3", "google-cloud-*"]
YOK006 = ["src/acme/public_api.py:*"]
```

大規模な既存projectには、既存issueを凍結して新規issueだけCIで落とすbaselineを使います(v0.2)。

```bash
uvx yokei --update-baseline
uvx yokei --baseline yokei-baseline.json
```

## インストール

`yokei` はPython wheelに同梱された単一のRust binaryです(Linux/macOS/Windows向けprebuilt wheel)。Rust toolchainなしで以下のすべてが動きます。

```bash
uvx yokei        # インストールを意識せず実行
pipx run yokei
pip install yokei
```

yokeiは解析対象projectのコードを実行しません — 解析は完全にstaticです。projectの仮想環境も必須ではありません: `.venv` があればmetadataを読み、なければmanifest・lockfile・bundled mapで解析します。

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md) を参照してください。設計仕様の全文(解析エンジン、import resolution戦略、ロードマップ)は [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) にあります。

## License

[MIT](./LICENSE)
