# chokkin

[English](./README.md)

[![PyPI](https://img.shields.io/pypi/v/chokkin)](https://pypi.org/project/chokkin/)
[![CI](https://github.com/watany-dev/chokkin/actions/workflows/ci.yml/badge.svg)](https://github.com/watany-dev/chokkin/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](./LICENSE)

**Pythonプロジェクトの余計なファイル・余計な依存・余計な公開シンボルを検出する。**

```bash
uvx chokkin
```

セットアップはこれだけです。chokkin は `pyproject.toml`、requirements 系ファイル、lockfile、各種ツール設定を読み、entry point からプロジェクト全体の到達性グラフを組み立てて、どこからも到達しないものを報告します。Python 版の [Knip](https://knip.dev/) です。コマンド1つ、設定ゼロ、必要になったら CI gate まで一直線に進めます。

- **設定ゼロ。** layout、entry point、dependency group、framework は自動で検出します。`[tool.chokkin]` を書くのは精度を詰めたくなってからで十分です。
- **プロジェクト全体で判定。** 「この import はこのファイル内で使われているか」ではなく「このファイル・パッケージ・シンボルは、実際に動くものから到達できるか」を見ます。
- **対象コードを実行しない。** 解析は完全に static です。Django settings も `setup.py` も notebook も、parse するだけで import しません。
- **速くて自己完結。** Linux / macOS / Windows 向けの Python wheel に同梱された単一の Rust binary です。Rust toolchain も、対象 project の仮想環境も不要です。
- **CI 前提の設計。** baseline、GitHub annotation、SARIF、固定の exit code、同梱の GitHub Action を備えています。

## 何が出るか

```text
chokkin 0.7.3

Project: acme-api
Config : pyproject.toml
Mode   : app, production=false

Unused files  2
  src/acme/legacy.py       src/acme/legacy.py     file `src/acme/legacy.py` is not reachable from any entry root
  src/acme/old_handlers.py src/acme/old_handlers.py file `src/acme/old_handlers.py` is not reachable from any entry root

Unused dependencies  3
  boto3                    pyproject.toml         declared in project.dependencies[1], no reachable import, config, or binary usage found
  python-dotenv            pyproject.toml         declared in project.dependencies[3], no reachable import, config, or binary usage found
  rich                     pyproject.toml         declared in project.dependencies[2], no reachable import, config, or binary usage found

Missing dependencies  1
  src/acme/config.py:3 yaml src/acme/config.py:3   imported pyyaml in src/acme/config.py:3 (no lockfile — transitive check skipped) but not declared in matching dependency context

Unused exports  2
  acme.auth:OldTokenBackend src/acme/auth.py:5     public class `OldTokenBackend` in `acme.auth` is not referenced from outside the module
  acme.utils:legacy_slugify src/acme/utils.py:5    public function `legacy_slugify` in `acme.utils` is not referenced from outside the module

Summary: 8 issues
```

すべての finding に rule code・場所・理由が付きます。納得できない finding があれば、`--explain` と `--trace` で判定の根拠を確認できます。

## Ruff / Vulture / deptry とどう違うか

どれも良いツールですが、答えている問いが違います。

| ツール   | 対象                                                   |
|---------|--------------------------------------------------------|
| Ruff    | ファイル内・構文単位の lint(未使用 import、未使用ローカル変数) |
| Vulture | Python AST ベースの、ファイル内 dead code 検出            |
| deptry  | 宣言済み依存と import の整合性                           |
| chokkin | project graph 全体から見た未使用ファイル・依存・公開シンボル |

chokkin は entry point(`console_scripts`、`manage.py`、`asgi.py`、テストスイート、notebook、CI のコマンドなど)から出発して、何に到達できるかを問います。誰も import しない module、誰も使わない package、他の module から参照されない class。それが finding です。framework やツールの設定も読むので、Django の `INSTALLED_APPS` のような文字列参照や、`pre-commit` hook が実行する `mypy` も「使われている」と数えられ、誤検知になりません。

## インストール

```bash
uvx chokkin          # インストールせずに実行
pipx run chokkin
pip install chokkin
```

wheel のインストールには Python 3.10 以上が必要です。解析対象の project 側はどの Python バージョンでも構いません(設定の `target_version`)。

対象 project の仮想環境は不要です。`.venv` があれば dist-info metadata を読み、なければ manifest・lockfile・同梱の distribution → module map で解析します。

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
|`CHK009`|`duplicate_dependency` |同じcontext内、またはruntimeとgroup/extraに重複宣言されている|同じcontext内: warning / runtimeとgroup/extra: info|
|`CHK010`|`unresolved_import`    |first-party/third-party/stdlibのいずれにも解決できないimport  |`TYPE_CHECKING`・optional (`try` / `suppress(ImportError)` / `find_spec` / `is_*_available()`): info / それ以外: warning|

各 finding には confidence(`certain` / `likely` / `maybe`)も付きます。`maybe` はデフォルトでは表示されず、`--confidence` と `--strict` で変えられます。

Python では module top-level の名前が原則 import 可能なので、`unused_export` は意図的に控えめです。library mode では info 扱いになり、定義元 module 自身が読んでいる名前(TypeVar、型 alias、helper など)は報告しません。

## 何を理解するか

**manifest と lockfile:** `pyproject.toml`(PEP 621、Poetry、PDM、Hatch、`[tool.uv]` の sources / constraints)、`include-group` を含む PEP 735 dependency group、PEP 723 inline script metadata、`setup.cfg`、静的に評価できる `setup.py`、`requirements*.txt` / `*.in`、そして transitive 判定用の `uv.lock` / `pylock.toml` / `poetry.lock` / `pdm.lock`。

**layout:** src layout と flat layout、tests、scripts、docs、examples、Jupyter notebook、build backend 設定による package directory、uv workspace や自動検出した monorepo member(それぞれ自身の manifest で解析)。

**framework とツール:** pytest、Django、FastAPI / uvicorn、Flask、Celery、Sphinx、MkDocs、Alembic、tox、nox、pre-commit、GitHub Actions。plugin は純粋な import 解析では見えない entry file・文字列による module 参照・binary usage を追加し、その framework が宣言済み依存にあれば自動で有効になります。例えば Django plugin は `INSTALLED_APPS` / `MIDDLEWARE` / `ROOT_URLCONF` の文字列を module 参照として扱い、`migrations/**` を framework-used にします。FastAPI と Flask の route handler は外部から使われているものとして扱います。

**dependency context:** 依存とファイルの両方に context(runtime / dev / test / docs / lint / type / optional extras)を割り当てます。これが `CHK005` の判定根拠です。`tests/` での `import pytest`(pytest が dev group にある)は OK、`src/` での同じ import は misplaced dependency です。`TYPE_CHECKING` 配下だけの import は type context、guard 付きの import(`try: import orjson`、`find_spec(...)`、platform 判定)は info 止まりです。

## 日常の使い方

```bash
uvx chokkin                          # カレントディレクトリを解析
uvx chokkin path/to/project
uvx chokkin --reporter compact       # finding 1件 = 1行
uvx chokkin --include CHK002,CHK003  # 依存関係の finding だけ
uvx chokkin --exclude CHK006
uvx chokkin --production             # runtime context だけで判定
uvx chokkin --strict                 # 厳しい policy、`maybe` も表示
```

**finding を信じる前に調べる:**

```bash
uvx chokkin --explain CHK002:boto3        # なぜ boto3 が未使用? どの import を見た?
uvx chokkin --trace src/acme/legacy.py    # このファイルはどう到達する(しない)?
```

`--trace` は到達可能なファイルには entry root からの import 連鎖を、未到達のファイルには理由・entry root・incoming import を出します。誤検知を報告するときはこの出力を添えてください。

**納得したら直す:**

```bash
uvx chokkin --fix --dry-run               # プレビュー
uvx chokkin --fix                         # certain な未使用依存を manifest から削除
uvx chokkin --fix --add-missing           # certain な missing dependency も宣言に追加
uvx chokkin --fix --allow-remove-files    # certain な未到達ファイルも削除
```

`--fix` は設計上保守的です。`certain` な finding だけを対象にし、行番号が合わなくなった manifest は編集を拒否し、改行コードを保ち、skip したものは stderr に理由付きで報告します。

**主要な flag:**

- `--production` は dev/test/docs/lint/type context を外し、runtime code だけで到達性を判定します。dev 専用のファイル・依存は消え、「production で未使用」が厳密に出ます。
- `--strict` は transitive 依存の直接 import を error にし、workspace member ごとの依存宣言を要求し、未宣言の type/test/docs/dev import も `CHK003` にし、environment marker 付き依存の unused も error にし、`maybe` の finding も表示します。
- `--reporter default|compact|json|markdown|github|sarif` で出力を選びます。`github` は workflow annotation、`sarif` は code scanning 用の SARIF 2.1.0 subset です。
- `--no-exit-code` は finding があっても 0 を返します。導入期間や summary 用です。
- `--no-cache` は `.chokkin/` 配下の parse cache を無効化します。cache は file stat をキーにし、stale や破損は miss として扱います。
- `--no-auto-workspace` は入れ子の `pyproject.toml` を workspace member にしたり、別 project として除外したりしません。
- `--init` は auto discovery の結果を反映した `[tool.chokkin]` の雛形を追記します。

exit code は CI 向けに固定です。

```text
0: reportable issueなし
1: issueあり
2: CLI/config error
3: internal error
```

## 既存プロジェクトへの導入

大きな project が最初からきれいなことは稀です。今ある finding を baseline に凍結して、CI は新規 finding だけで落とすようにします。

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
git add chokkin-baseline.json
```

その後、同梱の GitHub Action で pull request を gate します。annotation を出し、code scanning 用に SARIF を書き、baseline にない finding だけで失敗します。

```yaml
name: chokkin

on:
  pull_request:

permissions:
  contents: read
  security-events: write

jobs:
  chokkin:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: watany-dev/chokkin@f6d097a5595fa4c1d6a7d3b67154d9b30e5c1d46 # v0.7.3
        with:
          baseline: chokkin-baseline.json
          sarif-file: chokkin.sarif
      - uses: github/codeql-action/upload-sarif@2892aa5e19bbd11bc0cff5427e3b750a04d9e3c2 # v4.38.2
        if: always()
        with:
          sarif_file: chokkin.sarif
```

上の例のように、action はタグをコメントに残してフルコミット SHA で固定してください。タグは付け替えられますが SHA は変わらず、Dependabot は両方を更新します。

| Input | Default | 説明 |
| --- | --- | --- |
| `version` | action の ref と同じリリース | PyPI から実行する chokkin のバージョン。`latest` も可 |
| `working-directory` | `.` | 解析する project root |
| `baseline` | — | baseline ファイル。新規 finding だけで失敗 |
| `reporter` | `github` | 判定用 run の reporter |
| `sarif-file` | — | SARIF の出力先(判定 run が失敗しても書き出す) |
| `args` | — | 追加の CLI 引数(例: `--production --confidence likely`) |
| `cache` | `false` | `true` で `.chokkin/` キャッシュを読み書きする。PR に含まれるキャッシュで結果が変わらないよう既定は無効 |

action を使わない場合は `uvx` でバージョンを固定します。

```yaml
      - uses: astral-sh/setup-uv@c18668ad3cf93ea998bef934396af7bb5c839dc7 # v10.2.0
      - run: uvx chokkin@0.7.3 --baseline chokkin-baseline.json --reporter github
```

baseline と `--reporter json` の出力には `schema_version: "1"` が含まれ、[`docs/schema/`](./docs/schema/) の公開 JSON Schema に従います。

## issue の抑制

inline / file-level のコメント:

```python
from legacy import old_api  # chokkin: ignore[CHK003]

# chokkin: file-ignore[CHK006]   (ファイル先頭のcomment blockで)
```

config での ignore(rule code 単位。distribution 名 glob / path glob / `path:symbol` glob):

```toml
[tool.chokkin.ignore]
CHK001 = ["src/acme/generated/**/*.py"]
CHK002 = ["boto3", "google-cloud-*"]
CHK006 = ["src/acme/public_api.py:*"]
```

rule 別の severity(off で無効化):

```toml
[tool.chokkin.severity]
CHK001 = "off"
CHK006 = "info"
```

## 設定

デフォルトは zero config です。精密さが必要になったら `pyproject.toml` に `[tool.chokkin]` を追加します(単独の `chokkin.toml` / `.chokkin.toml` も使えます)。`chokkin --init` で雛形を書き出せます。以下はすべて省略可能です。

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
vendored = [             # 解析・到達性の追跡はするがissueは出さない
  "**/_vendor/**",
  "**/third_party/**",
]

[tool.chokkin.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

# distribution名 -> import名。bundled mapでカバーされない場合に
[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]
"protobuf" = ["google.protobuf"]  # namespace package 配下は dotted name で1つの distribution に絞れる

# CLI名 -> distribution名。CHK008/CHK002のbinary usage判定に使う
[tool.chokkin.binary_map]
"sphinx-build" = "Sphinx"

[tool.chokkin.plugins]
pytest = true
django = true
fastapi = true

[tool.chokkin.severity]
CHK001 = "off"
CHK006 = "info"
```

ルートの `.chokkin/` は解析データ専用で、常に探索から除外されます。

### モード

`mode = "auto"` は次のいずれかを選びます。

- **app** — 明確な entry(`console_scripts` / `manage.py` / `asgi.py` / `wsgi.py` / `app.py`)がある場合。unused file を積極的に報告します。
- **library** — `[project] name` と package があり、明確な entry がない場合。public module は見えない利用者から import され得るので、unused file / export は低 confidence か info で報告します。library で本気の unused file 検出をしたい場合は `entry` を明示してください。
- **workspace** — 複数の `pyproject.toml` または `tool.uv.workspace.members` がある場合。共有 lockfile を使いつつ member ごとに解析し、`[tool.chokkin.workspaces.<name>]` で member 別の設定ができます。workspace 宣言のない repository では、`[project]` name を持つ入れ子の `pyproject.toml`(4 階層まで)を自動で member にします。member でない入れ子 project(独自の `[project]` を持つ example app など)は warning を出して解析対象から外します。

## 限界

Python は動的で、chokkin は static です。見えないものがあります。

- 実行時データから名前で読み込まれる module(DB から読む plugin registry、非リテラルな名前の `importlib.import_module(f"{pkg}.{name}")`)。`entry` として宣言するか、その path の rule を ignore してください。
- chokkin に plugin がない framework やツール経由でのみ到達するコード。`--trace` が「なぜ未到達と判定したか」を出すので、そこから適切な `entry` や `ignore` を選べます。
- library の public API を使う利用者。library mode が `CHK001` / `CHK006` を error にせず格下げするのはこのためです。

finding がおかしく見えて、`--explain` / `--trace` でも決着しない場合は、その出力を添えて [issue](https://github.com/watany-dev/chokkin/issues) を立ててください。

## Contributing

[CONTRIBUTING.md](./CONTRIBUTING.md) を参照してください。設計仕様の全文(解析エンジン、import resolution 戦略、ロードマップ)は [`docs/dev/spec.ja.md`](./docs/dev/spec.ja.md) に、設計判断の記録は [`docs/adr/`](./docs/adr/) にあります。

## License

[MIT](./LICENSE)
