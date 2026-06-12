# yokei 仕様案

`yokei` は、Pythonプロジェクト全体の余計な依存・余計なファイル・余計な公開シンボルを検出する reachability analyzer として設計する。`uvx yokei` で `npx knip` に近い体験を提供し、設定なしで実行でき、必要に応じて精密な設定とCI運用に移行できることを重視する。

Knipは、`package.json`、ソースコード、各種ツール設定を横断して未使用依存・不足依存を検出する。Python版でも同じ思想を持ち込み、`pyproject.toml`、requirements系、uv/Poetry/PDM、Django/FastAPI/pytest/tox/nox/pre-commit/GitHub Actionsなどを読む設計にする。

`uvx yokei` の体験は成立する。`uvx` は `uv tool run` のエイリアスで、コマンド名と同名のPython packageを一時的な仮想環境に入れて実行できる。PyPI上のpackage名を `yokei`、実行ファイル名も `yokei` にすれば、`npx knip` に近い「インストールを意識しない実行」になる。Rust実装をpip/uvxで配る場合は、maturinの `bin` bindings を使い、Rust製バイナリをPython wheelに同梱して配布する。

## 1. コンセプト

一文説明は次の通り。

```text
Find unused files, dependencies, and public symbols in Python projects.
```

日本語では「Pythonプロジェクトの余計なファイル・余計な依存・余計な公開APIを検出する」。名前が `余計` 由来なら意味が通る。

`yokei` はRuffのようなstyle/lint toolではない。Ruffは未使用importやfunction scope内の未使用変数を高速に検出できるが、プロジェクト全体の依存関係宣言、未到達ファイル、framework設定由来の暗黙参照までは主目的ではない。

VultureはPythonのdead code検出ツールだが、Pythonの動的性により、暗黙的に呼ばれるコードが未使用として報告され得る。deptryは未使用・不足・transitive dependency検出に強いが、主対象はdependencyであり、Knip的なunused files/exportsまで含む「プロジェクト到達性グラフ」ではない。

立ち位置は次の通り。

```text
Ruff     : ファイル内・構文単位のlint
Vulture  : Python ASTベースのdead code検出
deptry   : Python dependency manifestとimportの整合性検出
yokei    : project graph全体から未使用ファイル・依存・公開シンボルを検出
```

## 2. 目指すUX

最重要の体験は次の通り。

```bash
uvx yokei
```

初回実行で、設定なしでも以下を行う。

```text
1. pyproject.toml / setup.cfg / setup.py / requirements*.txt / uv.lock を探索
2. src layout / flat layout / tests / scripts / docs / config files を推定
3. entry points と framework entry を推定
4. Python import graph を構築
5. dependency graph と照合
6. unused dependencies / missing dependencies / unused files / unused exports を表示
```

出力例。

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

CLIはKnip寄せでよい。

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

exit codeはCI向けに固定する。

```text
0: reportable issueなし
1: issueあり
2: CLI/config error
3: internal error
```

主要flagの意味は次の通りに固定する。

```text
--production   : dev/test/docs/lint/type contextを解析から外し、runtime contextの
                 到達性だけで判定する。dev専用のファイル・依存は報告対象外になり、
                 逆に「productionで未使用」が厳密に出る。
--strict       : (1) transitive依存の直接importを常にerror
                 (2) workspace memberごとに直接依存宣言を要求
                 (3) environment marker付き依存のunusedもerror扱い
                 (4) confidence maybe のissueも表示
--no-exit-code : issueがあってもexit codeを0にする。config/CLI errorの2、
                 internal errorの3はこのflagでも維持する。
```

`--no-exit-code` は導入初期やGitHub Actions summary用に必須。reporterはv0.1でdefault(human)/compact/JSON/Markdownを持ち、SARIF/GitHub reporterはv0.2で追加する(§16)。`--explain` と `--trace` は誤検知報告の導線としてv0.1から提供する(§20)。

## 3. issue種別

MVPでは以下のrule IDを固定する。

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

`unused_export` はPythonでは危険。JavaScript/TypeScriptの `export` と違い、Pythonではmodule top-levelの名前が原則import可能になる。最初は `unused_export` をpreview ruleまたはlibrary modeではinfo扱いにする。

## 4. project discovery仕様

`yokei` はcurrent directoryから上に向かってproject rootを探索する。優先順は次の通り。

```text
1. pyproject.toml
2. uv.lock
3. setup.cfg
4. setup.py
5. requirements.txt
6. .git
```

探索方向は2段階に分ける。root探索はcurrent directoryから**上方向のみ**で、最初にmarkerが見つかったdirectoryをproject rootとして確定する。workspace member探索は、確定したrootから**下方向**にスキャンする(§5、§8)。この区別を曖昧にすると、monorepoのsubdirectoryで実行した場合の挙動が定義できない。

`pyproject.toml` はPython packagingだけでなくlinters/type checkersなど各種tool設定の置き場でもあるため、`[project]`、`[dependency-groups]`、`[tool.*]` を読む前提にする。

dependency sourceは少なくとも以下を読む。

```text
[project.dependencies]
[project.optional-dependencies]
[dependency-groups]
[tool.poetry.dependencies]
[tool.poetry.group.*.dependencies]
requirements.txt
requirements-dev.txt
dev-requirements.txt
constraints.txt
setup.cfg
setup.py  # static parseのみ。実行しない。
uv.lock
```

requirements系filesのパース規則を定める。`-r other.txt` は再帰的に追跡する。`-c constraints.txt` はversion制約の情報源としてのみ読み、依存宣言とは扱わない。`-e ./path` とlocal path指定はworkspace/first-party候補として扱う。VCS URL・direct URL指定は `name @ url` 形式または `#egg=` からdistribution名を抽出し、抽出できない場合はopaque依存としてunused判定の対象外にする。environment markerは保持し、§10の判定で使う。

`setup.py` が静的に解析できない場合(動的な `install_requires` 構築など)は、warningを出してそのsourceをskipし、他のsourceで解析を継続する。`[project]` の `dynamic = ["dependencies"]` が指定されている場合は、setuptoolsの慣習に従い `requirements*.txt` 側を依存宣言の実体として読む。

`[dependency-groups]` は、build metadataには含めない開発用途の依存を `pyproject.toml` に格納する標準仕様。lint/test/docs用の依存を扱うため、`yokei` ではmain dependencyとは別contextとして扱う。

Python packagingのentry pointsは、distributionが提供するcomponentを他のコードやinstallerに知らせる仕組みで、`console_scripts` はインストール時にCLI wrapperを作るために使われる。`yokei` は `[project.scripts]`、`[project.gui-scripts]`、`[project.entry-points]` をentry rootとして扱う。

## 5. 設定ファイル仕様

設定は `pyproject.toml` の `[tool.yokei]` を第一候補にする。Knip体験に寄せるなら、別ファイルとして `yokei.toml` と `.yokei.toml` も許容するが、Pythonでは `pyproject.toml` 集約が自然。複数ソースは `.yokei.toml` → `yokei.toml` → `pyproject.toml` の順にマージし後勝ち。配列・mapは置換だが、`plugins` と `dependencies` のみキー単位マージ（`celery = true` だけ指定しても既定有効のpluginは維持される）。

最小設定例。

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
target_version = "py311"  # 解析対象projectのPython。stdlib判定と構文解析の基準
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

[tool.yokei.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]
"python-dotenv" = ["dotenv"]
"scikit-learn" = ["sklearn"]

[tool.yokei.binary_map]
# CLI名 -> distribution名。YOK008/YOK002のbinary usage判定に使う
"pytest" = "pytest"
"alembic" = "alembic"
"sphinx-build" = "Sphinx"

[tool.yokei.plugins]
pytest = true
django = true
fastapi = true
celery = true
tox = true
nox = true
pre_commit = true
github_actions = true
```

workspace設定。

```toml
[tool.yokei.workspaces.api]
path = "services/api"
entry = ["src/api/main.py"]
project = ["src/**/*.py", "tests/**/*.py"]

[tool.yokei.workspaces.worker]
path = "services/worker"
entry = ["src/worker/__main__.py"]
project = ["src/**/*.py", "tests/**/*.py"]
```

uv workspaceも読む。uvのworkspaceは複数packageをまとめて管理する仕組みで、各memberが自分の `pyproject.toml` を持ち、workspace全体で単一lockfileを共有する。`tool.uv.workspace.members` がある場合は自動でworkspace modeに入り、各memberの依存・source・entryを別々に解析しつつ、root dependencyとの関係を判断する。

`yokei --init` は、auto discoveryで検出したlayout・entry・dependency groupを反映した `[tool.yokei]` の雛形を `pyproject.toml` に追記する。既存の `[tool.yokei]` がある場合は上書きせずexit code 2で終了する。

## 6. 解析エンジン仕様

内部モデルはgraphにする。

```text
Project
  ├── Workspace
  ├── Manifest
  ├── File
  ├── Module
  ├── Symbol
  ├── Distribution
  ├── Binary
  └── ConfigReference
```

edgeは次の程度で足りる。

```text
File imports Module
Module defines Symbol
Module reexports Symbol
Distribution provides Module
Manifest declares Distribution
ConfigReference uses Module
ConfigReference uses Binary
Entry reaches File
File reaches File
```

処理順は固定する。

```text
1. root discovery          # 実装済み: src/discovery/ (`discover_project_root`)
2. config load             # 実装済み: src/config/ (`load_config`)
3. manifest extraction
4. source file discovery
5. config/plugin extraction
6. Python parse
7. import resolution
8. entry root construction
9. reachability analysis
10. dependency reconciliation
11. symbol usage analysis
12. issue emission
13. optional fix
```

Python parserはRust実装でよい。Ruff ecosystemのparserを使うか、RustPython parserを使うかはライセンス・保守性・Python新構文対応速度で選ぶ。ただし、Ruffのparser crate群をAstralが安定APIとして公開し続ける保証はないため、採用する場合はversion固定またはvendoring前提のリスクを織り込む。重要なのは、ASTだけではなくtoken位置・comments・string literalを保持すること。`# yokei: ignore[...]`、`__all__`、`TYPE_CHECKING`、`importlib.import_module("...")`、framework設定のstring literalを拾う必要がある。

## 7. import resolution仕様

Pythonの依存解析で最大の罠は、distribution名とimport名が一致しないこと。Python標準の `importlib.metadata` でも、distribution package名とtop-level import package名は必ずしも1:1対応しない。1つのdistributionが複数import packageを持つことも、namespace packageで1つのimport名に複数distributionが対応することもある。

解決戦略は多層にする。

```text
1. stdlib判定
2. first-party module判定
3. workspace member判定
4. local .venv の dist-info / METADATA / top_level.txt / RECORD を読む
5. Core Metadata の Import-Name / Import-Namespace を読む
6. bundled package-module-map を使う
7. user-defined package_module_map を使う
8. 最後に canonicalize(name).replace("-", "_") で推定
```

Core Metadata 2.5(PEP 794、2025年9月承認)には `Import-Name` と `Import-Namespace` が定義されており、distributionが提供するimport名を表す。ただし承認直後でエコシステムへの普及はこれからであり、当面はこのfieldを持つpackageがほぼ存在しない。したがって現時点ではbundled package-module-mapと `.venv` metadataを主情報源とし、`Import-Name` は普及に応じて優先度を上げていく将来の主情報源と位置付ける。

binary名からdistribution名への逆引き(YOK008、binary usage判定)も同じ多層戦略にする。`.venv` があれば `entry_points.txt` / `RECORD` のscriptsを読み、なければbundled binary map、最後にuser定義の `[tool.yokei.binary_map]` を使う。import名問題と同型の課題であり、専用のfallback dataが必要になる。

重要なのは、`uvx yokei` はprojectの仮想環境ではなく、`yokei` 自身の一時環境で動く点。project venv必須にしてはいけない。`.venv` があれば読む、なければmanifest/lockfile/bundled map/user mapだけで解析する設計にする。

## 8. entry point自動推定

zero-configにはentry推定が必要。デフォルトでentry扱いにするものは次の通り。

```text
[project.scripts]
[project.gui-scripts]
[project.entry-points.*]
__main__.py
main.py
app.py
manage.py
asgi.py
wsgi.py
noxfile.py
conftest.py
docs/conf.py
alembic/env.py
scripts/**/*.py
```

単独ファイル名でのentry推定(`main.py` / `app.py` / `manage.py` / `asgi.py` / `wsgi.py` / `noxfile.py`)は**project root直下、およびsrc layoutのpackage直下のみ**を対象にする。任意の深さで同名ファイルをentry扱いすると、unused file検出が事実上無効化されるため。`__main__.py` と `conftest.py` は全階層で有効。`docs/conf.py` と `alembic/env.py` は記載のpathに限定する。

ただし、library projectではaggressiveにunused filesを出すと誤検知が増える。`mode = "auto"` の判定は次にする。

```text
console_scripts / manage.py / asgi.py / wsgi.py / app.py がある
  -> app mode

[project] name があり、src/<package>/__init__.py または
root直下の <package>/__init__.py (flat layout) があり、明確なentryがない
  -> library mode

複数 pyproject.toml / tool.uv.workspace.members がある
  -> workspace mode

いずれにも該当しない
  -> app mode。ただしunused_fileのconfidence上限をlikelyに落とす
```

`app mode` ではunused filesを積極的に出す。`library mode` では、public moduleは外部利用され得るため、unused filesは `maybe` confidenceに落とし、デフォルトでは表示しないかinfo扱いにする。libraryで本気のunused file検出をしたい場合は、ユーザーに `entry` を明示させる。

## 9. plugin仕様

Knip相当の体験にするにはpluginが中核。Pythonはframeworkの暗黙参照が多いため、pluginなしではfalse positiveが多くなる。

pluginの実装優先順は次の通り。v0.1に入れるのは pytest / django / fastapi の3つに絞り、残りはv0.2以降にする(§16)。

```text
pytest
django
fastapi / uvicorn
flask
celery
tox
nox
pre-commit
github-actions
sphinx
mkdocs
alembic
```

pluginの責務は3つだけにする。

```text
1. entry filesを追加する
2. string/module referencesを追加する
3. binary usageを追加する
```

Django pluginの例。

```text
manage.py をentryにする
settings.py をentryにする
INSTALLED_APPS の文字列をmodule referenceにする
MIDDLEWARE の文字列をsymbol/module referenceにする
ROOT_URLCONF をmodule referenceにする
migrations/**/*.py はframework-used扱いにする
```

pytest pluginの例。

```text
tests/**/test_*.py / tests/**/*_test.py をtest contextのentry rootにする
  (pytestのtest discoveryに相当。conftest.pyはtest filesをimportしないため、
   test file自体をrootにしないとtest内のimportが依存使用として数えられない)
conftest.py をentryにする
pytest_plugins = ["..."] をmodule referenceにする
[tool.pytest.ini_options] を読む (testpaths / python_files があれば上記globを上書き)
pytest command usageをbinary usageにする
```

FastAPI/Uvicorn pluginの例。

```text
uvicorn acme.api:app を module:symbol reference として読む
@router.get / @app.post decorated functionをexternally used扱いにする
```

Celery pluginの例。

```text
@shared_task / @app.task decorated functionをexternally used扱いにする
autodiscover_tasks() の対象を推定する
```

CI/config pluginの例。

```text
.github/workflows/*.yml の run: から python -m, uv run, pytest, mypy, ruff, alembic 等を拾う
.pre-commit-config.yaml の hook id / entry を拾う
tox.ini / pyproject [tool.tox] の deps と commands を拾う
noxfile.py の @nox.session をentry扱いにする
```

## 10. dependency判定仕様

dependencyはcontextで分ける。

```text
runtime
dev
test
docs
lint
type
optional-extra:<name>
workspace
```

contextは依存だけでなく**file側にも割り当てる**。YOK005(misplaced)はこのfile contextに依存するため、割当規則を固定する。

```text
src/** / flat layoutのpackage/** / [project.scripts]到達file -> runtime
tests/** / conftest.py / *_test.py / test_*.py             -> test
docs/**                                                     -> docs
noxfile.py / 各tool設定が参照するscript                      -> dev
scripts/**                                                  -> dev (設定で変更可)
plugin / [tool.yokei] のcontext指定が上記を上書きする
```

判定例。

```text
runtime codeで import requests
  [project.dependencies] に requests がある
    -> OK

runtime codeで import yaml
  PyYAML がどこにもない
    -> YOK003 missing_dependency

tests/ だけで import pytest
  dependency-groups.dev/test に pytest がある
    -> OK

src/ で import pytest
  dependency-groups.dev にしか pytest がない
    -> YOK005 misplaced_dependency

src/ で import urllib3
  requests のtransitive dependencyとして入っているだけ
    -> YOK004 transitive_dependency

[project.dependencies] に boto3 がある
  import/config/binary usageがどこにもない
    -> YOK002 unused_dependency
```

判定の優先順位を固定する。宣言されていないimportは、**lockfileの推移閉包で解決できればYOK004、できなければYOK003**とする。lockfileが存在しない場合(requirements.txtのみの環境など)はtransitive判定が不可能なため、YOK004はYOK003に縮退し、その旨をmessageに含める。

environment markerとextrasの扱いも定める。

```text
marker付き依存 (例: pywin32; sys_platform == "win32")
  -> 解析環境では未使用に見えても誤検知になりやすい
  -> unused判定のconfidenceを1段下げ、defaultではwarning、--strict時のみerror

extra指定 (例: requests[security])
  -> extraが有効化するtransitive依存を推移閉包に含めてYOK004判定する

stub package (types-* / *-stubs)
  -> importされないため素朴にはYOK002になる
  -> 対応するruntime packageの使用があればtype contextでused扱い
  -> runtime package自体が未使用なら、stubも併せてunused報告する
```

`TYPE_CHECKING` 配下のimportはtype contextにする。runtime dependencyとしては扱わない。

```python
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pandas import DataFrame
```

この場合、`pandas` がruntime依存にある必要はない。ただし、type checkerを実行する環境で必要なら `type_groups = ["types", "mypy"]` 側にあるべき、という判断にできる。

`try import` はoptional扱いにする。

```python
try:
    import orjson
except ImportError:
    orjson = None
```

この場合、未宣言でも即 `missing_dependency` にはしない。`orjson` がoptional extraにあるならOK、main dependencyにあるならOK、どこにもなければ `optional_missing` としてdefaultはinfo、`--strict` 時はwarningにする。

## 11. unused files判定

unused filesはimport graphの到達可能性で判定する。

```text
entry roots
  -> import edges
  -> config/plugin edges
  -> reachable files

project files - reachable files = unused file candidates
```

ただし、以下はデフォルトで除外または低confidenceにする。

```text
__init__.py
*.pyi
migrations/**/*.py
tests/**/*.py
docs/conf.py
generated files
vendored code
namespace package fragments
plugin-marked files
```

`unused_file` のconfidenceはこう決める。

```text
certain:
  app modeで、明確なentryから到達せず、dynamic importもなく、plugin対象でもない

likely:
  dynamic importはあるが対象module名と一致しない

maybe:
  library mode、namespace package、wildcard importがある
```

デフォルトでは `confidence = "likely"` 以上だけ表示する。`--confidence maybe` で全部出す。

confidenceとseverityは独立した2軸として扱う。severityはissue種別の重み(error / warning / info)、confidenceは解析の確からしさ(certain / likely / maybe)。表示filterは `confidence >= 設定値`、exit code 1の対象はdefaultで `severity >= error` かつ `confidence >= likely` にする。`--strict` ではwarning以上かつmaybe以上まで広げる。

## 12. unused exports判定

Pythonのexportは曖昧なので、最初から強く削除提案しない。

対象にするsymbolは次の通り。

```text
module top-level function
module top-level class
module top-level constant
__all__ に含まれる名前
__init__.py でreexportされた名前
```

ただし `_` 始まりの名前は慣習的privateとして対象外にする(`__all__` に明示されている場合を除く)。

ただし、以下はused扱いにする。

```text
decorated route handler
pytest fixture
click/typer command
celery task
pydantic model used by annotation
Django model/admin/app config
dataclass referenced by annotation
entry point target
```

`__all__` があるmoduleでは、`__all__` をpublic API宣言として扱う。`__all__` にあるが内部から使われないものは、library modeではinfo、app modeではwarningにする。

`unused_export` の自動削除はv1までは避ける。安全なfixは `__all__` からの削除程度に限定し、関数・class本体の削除は `--fix --unsafe` がある場合だけにする。

## 13. fix仕様

`--fix` は最初から提供してよいが、削除対象を限定する。

```bash
uvx yokei --fix
```

初期の `--fix` でやること。

```text
unused dependencyをpyproject.toml / requirements*.txtから削除
duplicate dependencyを整理
明確なmisplaced dependencyをdependency groupへ移動
```

初期の `--fix` でやらないこと。

```text
Python関数/class本体の削除
未使用ファイルの削除
曖昧なmissing dependencyの自動追加
dynamic import周辺の変更
```

ファイル削除は明示フラグ必須。

```bash
uvx yokei --fix --allow-remove-files
```

missing dependency追加は別フラグにする。

```bash
uvx yokei --fix --add-missing
```

ただし、`yaml -> PyYAML` のように一意に解決できる場合だけ追加する。候補が複数ある場合はsuggestionに留める。

manifest編集はformat保持が重要。Rustなら `toml_edit` を使い、commentsと順序を極力維持する。requirements系はline-based編集で、hash付きrequirementsやconstraintsは原則自動編集しない。

lockfileとの整合にも注意する。`pyproject.toml` を編集するとuv.lock / poetry.lock等は古くなるが、`yokei` はlockfileを直接編集しない。fix適用後に `uv lock` / `poetry lock` の実行を促すメッセージを出力する。

## 14. 既存Pythonエコシステムの課題と解決

|課題                            |具体例                                                            |yokeiでの解決                                                                                  |
|------------------------------|---------------------------------------------------------------|-------------------------------------------------------------------------------------------|
|distribution名とimport名が一致しない   |`PyYAML -> yaml`, `Pillow -> PIL`, `python-dotenv -> dotenv`   |Core Metadataの `Import-Name` / `Import-Namespace`、`.venv` metadata、bundled map、user mapを重ねる|
|`uvx` 実行時にproject venvが見えない   |`uvx yokei` はyokei用一時環境で動く                                     |project venv必須にしない。manifest/lockfile/static mapを主情報源にする                                    |
|frameworkが文字列でmoduleを参照する     |Django `INSTALLED_APPS`, Celery autodiscover, Sphinx extensions|pluginでstring literalをmodule reference化する                                                  |
|decoratorsが外部entryになる         |FastAPI route, pytest fixture, Celery task                     |decorator patternをexternally usedとして扱う                                                     |
|libraryのpublic APIは外部利用される    |内部参照がなくても公開APIかもしれない                                           |app/library modeを分け、libraryではunused exports/filesを低confidenceにする                           |
|dev/test/docs/lint/type依存が混在する|`pytest` がmain dependenciesにある                                 |dependency contextを導入し、misplaced dependencyを出す                                             |
|namespace packageがある          |`google.*`, `zope.*`                                           |namespace package modeと `Import-Namespace` を使う                                             |
|dynamic importを完全には解けない       |`importlib.import_module(name)`                                |literalは解く。非literalはopaque dynamic importとしてconfidenceを下げる                                 |
|monorepo/workspaceで依存境界が曖昧    |root depsをmemberが使う                                            |workspace graphを作り、`--strict` でmemberごとの直接依存を要求する                                          |
|auto-fixが危険                   |dead code削除で実行時破壊                                              |default fixはmanifest中心。file/code削除は明示フラグ必須                                                 |

Python packagingでは、`Requires-Dist` が依存distributionを表し、optional featureは `Provides-Extra` とextra markerで表される。`yokei` はこのmetadataの考え方に合わせ、runtime dependency、optional dependency、dependency groupを区別して扱う。

## 15. Rust実装とpip配布

構成はこれでよい。

```text
yokei/
  Cargo.toml
  pyproject.toml
  src/
    main.rs
    discovery/   # 実装済み: pipeline step 1 (root discovery)
    config/      # 実装済み: pipeline step 2 (config load)
    cli.rs
    manifest/
    parser/
    resolver/
    graph/
    rules/
    reporters/
    fix/
    plugins/
```

`pyproject.toml` は概ねこうする。

```toml
[build-system]
requires = ["maturin>=1.8,<2"]
build-backend = "maturin"

[project]
name = "yokei"
version = "0.1.0"
description = "Find unused files, dependencies, and public symbols in Python projects"
readme = "README.md"
requires-python = ">=3.10"  # 3.9は2025年10月にEOL
license = "MIT"
keywords = ["python", "dead-code", "dependencies", "unused", "knip", "uv"]
classifiers = [
  "Development Status :: 3 - Alpha",
  "Environment :: Console",
  "Intended Audience :: Developers",
  "Programming Language :: Rust",
  "Programming Language :: Python :: 3",
  "Topic :: Software Development :: Quality Assurance",
]

[project.urls]
Homepage = "https://github.com/<org>/yokei"
Repository = "https://github.com/<org>/yokei"
Issues = "https://github.com/<org>/yokei/issues"

[tool.maturin]
bindings = "bin"
strip = true
```

Rust binaryをwheelに入れるだけならPython extension moduleは不要。`pip install yokei`、`uvx yokei`、`pipx run yokei` の全てで同じnative binaryを起動できる。なお、`requires-python` は配布上の制約に過ぎない。解析対象projectのPythonバージョンは `target_version` で独立に指定でき、yokei自身が動く環境と関係なく古いprojectも解析できる。

配布ではprebuilt wheelが必須。source buildだけにすると、ユーザー側にRust toolchainが必要になり、`uvx yokei` の体験が悪化する。最低限のwheel targetは以下。

```text
manylinux x86_64
manylinux aarch64
musllinux x86_64
musllinux aarch64
macOS x86_64
macOS arm64
Windows x86_64
Windows arm64  # 可能なら
```

releaseはGitHub Actions + PyPI Trusted Publishingに寄せる。

## 16. MVP範囲

v0.1で入れるべきもの。

```text
- uvx yokei で起動
- pyproject.toml / requirements.txt 読み取り
- [project.dependencies]
- [project.optional-dependencies]
- [dependency-groups]
- src layout / flat layout検出
- Python import graph
- unused dependencies
- missing dependencies
- transitive dependencies
- misplaced dependencies
- unused files, app modeのみ
- unused exports, preview扱い
- package_module_map / binary_map
- pytest / django / fastapi plugin
- default / compact / json / markdown reporter
- --production
- --strict
- --no-exit-code
- --explain
- --trace
- --fix, dependency削除のみ
```

v0.2で入れるもの。

```text
- uv workspace対応
- Poetry/PDM/Hatch設定の深掘り
- GitHub Actions reporter
- SARIF reporter
- baseline file
- flask / celery / tox / nox / pre-commit / github-actions plugin
- notebook parsing
- Sphinx / MkDocs / Alembic plugin
- cache
```

v1.0で安定させるもの。

```text
- plugin API
- rule severity設定
- stable JSON schema
- stable exit code
- stable ignore syntax
- safe autofix contract
- large monorepo performance
- editor/LSP連携
```

## 17. ロードマップ

各phaseに目標・成果物・exit criteriaを置く。期間は専任1〜2人を想定した目安。

### Phase 0: 基盤(〜4週)

```text
目標   : 解析エンジンの土台と配布パイプラインを先に固める
成果物 :
  - parser選定spike (Ruff parser vs RustPython parser、§6のリスク評価込み)
  - graph core (File/Module/Symbol/Distribution/edge構造、§6)
  - bundled package-module-map 初版 (PyPI download上位500package)
  - bundled binary map 初版 (主要dev tool 50個程度)
  - wheel build matrix + PyPI Trusted Publishing のCI (§15)
exit   : 空projectと小規模sample projectで uvx yokei が動き、wheelが全targetでbuildできる
```

### Phase 1: v0.1 MVP(+6〜8週)

```text
目標   : §16 v0.1 scopeの実装と、誤検知率の実測
成果物 :
  - manifest extraction / import resolution / reachability (§4, §6, §7)
  - YOK001-YOK010 (unused_exportはpreview)
  - pytest / django / fastapi plugin
  - default / compact / json / markdown reporter
  - --production / --strict / --explain / --trace / --fix (dependency削除のみ)
検証   : 既知のOSS project 20個 (Django app / FastAPI app / library / monorepo混在)
         でdogfoodingし、YOK002/YOK003の誤検知を分類・記録する
exit   : 検証セットでunused dependencyの誤検知率 5%未満、
         crash 0、cold実行がmedium projectで2s以内
```

### Phase 2: v0.2 導入支援(+6〜8週)

```text
目標   : 既存大規模projectへの導入障壁を下げる
成果物 : §16 v0.2 scope
  - baseline file (既存issueの凍結、§18)
  - uv workspace / Poetry / PDM / Hatch
  - SARIF / GitHub Actions reporter
  - cache (§19のwarm目標達成)
  - plugin拡充 (flask / celery / tox / nox / pre-commit / github-actions)
exit   : 10k files級monorepoでwarm 2s以内、baseline運用でCI導入事例を作る
```

### Phase 3: v0.3〜v0.x 安定化(継続)

```text
目標   : v1.0で凍結する契約の準備
成果物 :
  - plugin API設計のRFCと外部plugin試作
  - JSON schema / exit code / ignore構文のdraft凍結
  - rule severity設定
  - notebook parsing / Sphinx / MkDocs / Alembic plugin
  - 誤検知報告から package-module-map / binary map / plugin database を継続更新
exit   : 2 minor version連続でbreaking changeなし
```

### Phase 4: v1.0(条件達成次第)

```text
目標   : §16 v1.0 list の安定性保証
内容   :
  - stable JSON schema / exit code / ignore syntax / safe autofix contract
  - semver契約の明文化 (何がbreaking changeか)
  - editor/LSP連携
  - large monorepo performance の最終チューニング
```

### 横断work(全phase継続)

```text
- package-module-map / binary map のデータ収集と自動生成pipeline
- 誤検知報告template (--explain出力の添付を必須にする)
- 検証用OSS project setの拡充とregression test化
```

リリース判断は期日ではなくexit criteriaで行う。特にPhase 1の誤検知率は、§20の「信頼を失いにくい」方針の定量版であり、未達のままv0.1を出さない。

## 18. ignore仕様

inline ignore。

```python
from legacy import old_api  # yokei: ignore[YOK003]

def public_hook():  # yokei: ignore[YOK006]
    ...
```

file-level ignore。ファイル先頭のcomment block(最初のimport文・実行文より前)でのみ有効とする。

```python
# yokei: file-ignore[YOK006]
```

config ignore。keyはrule codeに統一する。値のpattern構文はruleの対象に依存し、dependency系ruleではdistribution名のglob、file系ruleではpath glob、symbol系ruleでは `path:symbol_glob` 形式とする。

```toml
[tool.yokei.ignore]
YOK001 = [
  "src/acme/migrations/**/*.py",
  "src/acme/generated/**/*.py",
]
YOK002 = ["boto3", "google-cloud-*"]
YOK003 = ["pkg_resources"]
YOK006 = ["src/acme/public_api.py:*"]
```

baselineも導入しやすさに効く。

```bash
uvx yokei --update-baseline
uvx yokei --baseline yokei-baseline.json
```

baselineは既存issueを黙らせ、新規issueだけCIで落とすためのもの。大規模既存projectでは必須。

## 19. パフォーマンス目標

Rustで作るなら、体感はRuff寄せにする。サイズの目安は、small ≈ 100 files以下、medium ≈ 1,000 files、large ≈ 10,000 files以上とする。

```text
small project      < 300ms
medium project     < 2s
large monorepo     < 10s cold
large monorepo     < 2s warm cache
```

cache keyは以下を使う。

```text
yokei version
config hash
manifest hash
lockfile hash
python target version
file path
file mtime
file size
file content hash
plugin version
```

parallelize対象は、file discovery、parse、import extraction、symbol extraction、plugin config parse。graph resolutionだけは集約後に行う。

## 20. 注意点

最も重要な設計判断は、project codeを実行しないこと。PythonではDjango settingsやsetup.pyをimportして解析する設計にすると、DB接続、環境変数依存、副作用、任意コード実行の問題が出る。`yokei` はstatic parseに徹し、runtime traceは将来の明示opt-inに分離する。

次に、`unused exports` を強く売りすぎないこと。Python libraryでは「内部から未使用」でも外部ユーザーがimportしている可能性がある。最初の訴求は `unused dependencies` と `unused files` に置き、`unused exports` はconfidence付きの補助機能として出す方が信頼を失いにくい。

最後に、`yokei` の価値は検出アルゴリズム単体ではなく、plugin databaseとpackage-module-mapの品質に依存する。MVP時点から、誤検知報告を集めやすい `--explain`、`--trace`、`--reporter json`、`package_module_map` を用意しておく。

Knip的な体験は「だいたい当たる静的解析」ではなく、「設定なしで始まり、誤検知を説明・抑制・改善できる運用性」で決まる。

## 21. 参考資料

- Knip: <https://knip.dev/>
- Knip unused dependencies: <https://knip.dev/typescript/unused-dependencies>
- Knip reporters: <https://knip.dev/features/reporters>
- uv CLI reference: <https://docs.astral.sh/uv/reference/cli/>
- uv workspaces: <https://docs.astral.sh/uv/concepts/projects/workspaces/>
- Python pyproject.toml guide: <https://packaging.python.org/en/latest/guides/writing-pyproject-toml/>
- Python dependency groups specification: <https://packaging.python.org/en/latest/specifications/dependency-groups/>
- Python entry points specification: <https://packaging.python.org/specifications/entry-points/>
- Python Core Metadata specification: <https://packaging.python.org/specifications/core-metadata/>
- Python importlib.metadata: <https://docs.python.org/3/library/importlib.metadata.html>
- maturin bindings: <https://www.maturin.rs/bindings.html>
- maturin documentation: <https://www.maturin.rs/>
- PyPI Trusted Publishers: <https://docs.pypi.org/trusted-publishers/using-a-publisher/>
- Ruff unused import rule: <https://docs.astral.sh/ruff/rules/unused-import/>
- Vulture: <https://pypi.org/project/vulture/>
- deptry: <https://github.com/fpgmaas/deptry>
