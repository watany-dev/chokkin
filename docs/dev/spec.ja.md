# chokkin 仕様案

`chokkin` は、Pythonプロジェクト全体の余計な依存・余計なファイル・余計な公開シンボルを検出する reachability analyzer として設計する。`uvx chokkin` で `npx knip` に近い体験を提供し、設定なしで実行でき、必要に応じて精密な設定とCI運用に移行できることを重視する。

Knipは、`package.json`、ソースコード、各種ツール設定を横断して未使用依存・不足依存を検出する。Python版でも同じ思想を持ち込み、`pyproject.toml`、requirements系、uv/Poetry/PDM、Django/FastAPI/pytest/tox/nox/pre-commit/GitHub Actionsなどを読む設計にする。

`uvx chokkin` の体験は成立する。`uvx` は `uv tool run` のエイリアスで、コマンド名と同名のPython packageを一時的な仮想環境に入れて実行できる。PyPI上のpackage名を `chokkin`、実行ファイル名も `chokkin` にすれば、`npx knip` に近い「インストールを意識しない実行」になる。Rust実装をpip/uvxで配る場合は、maturinの `bin` bindings を使い、Rust製バイナリをPython wheelに同梱して配布する。

## 1. コンセプト

一文説明は次の通り。

```text
Find unused files, dependencies, and public symbols in Python projects.
```

日本語では「Pythonプロジェクトの余計なファイル・余計な依存・余計な公開APIを検出する」。名前が `余計` 由来なら意味が通る。

`chokkin` はRuffのようなstyle/lint toolではない。Ruffは未使用importやfunction scope内の未使用変数を高速に検出できるが、プロジェクト全体の依存関係宣言、未到達ファイル、framework設定由来の暗黙参照までは主目的ではない。

VultureはPythonのdead code検出ツールだが、Pythonの動的性により、暗黙的に呼ばれるコードが未使用として報告され得る。deptryは未使用・不足・transitive dependency検出に強いが、主対象はdependencyであり、Knip的なunused files/exportsまで含む「プロジェクト到達性グラフ」ではない。

立ち位置は次の通り。

```text
Ruff     : ファイル内・構文単位のlint
Vulture  : Python ASTベースのdead code検出
deptry   : Python dependency manifestとimportの整合性検出
chokkin    : project graph全体から未使用ファイル・依存・公開シンボルを検出
```

## 2. 目指すUX

最重要の体験は次の通り。

```bash
uvx chokkin
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

CLIはKnip寄せでよい。

```bash
uvx chokkin
uvx chokkin --production
uvx chokkin --strict
uvx chokkin --fix
uvx chokkin --fix --allow-remove-files
uvx chokkin --include dependencies,files
uvx chokkin --exclude exports
uvx chokkin --reporter json
uvx chokkin --reporter github
uvx chokkin --reporter sarif
uvx chokkin --no-exit-code
uvx chokkin --explain CHK002:boto3
uvx chokkin --trace src/acme/legacy.py
uvx chokkin --init
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

`--no-exit-code` は導入初期やGitHub Actions summary用に必須。reporterはv0.1でdefault(human)/compact/JSON/Markdownを持ち、v0.2でSARIF/GitHub reporterを追加する(§16)。`--explain` と `--trace` は誤検知報告の導線としてv0.1から提供する(§20)。
`--baseline PATH` と `--update-baseline` は v0.2 導入支援のP0として実装する。`--update-baseline` は必ず `--baseline PATH` と併用し、通常実行では baseline にある fingerprint と一致する issue を抑制する。

## 3. issue種別

MVPでは以下のrule IDを固定する。

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

`unused_export` はPythonでは危険。JavaScript/TypeScriptの `export` と違い、Pythonではmodule top-levelの名前が原則import可能になる。最初は `unused_export` をpreview ruleまたはlibrary modeではinfo扱いにする。

## 4. project discovery仕様

`chokkin` はcurrent directoryから上に向かってproject rootを探索する。優先順は次の通り。

```text
1. pyproject.toml
2. uv.lock
3. setup.cfg
4. setup.py
5. requirements.txt
6. .git
```

`.git` markerは通常のGitリポジトリではディレクトリ、git worktreeやsubmoduleでは`gitdir:`を書いたファイル(gitfile)になる。どちらの場合もmarkerとして扱う。

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

requirements系filesのパース規則を定める。コメントはpip互換で「行頭または空白が先行する `#`」のみを除去し、URLフラグメント（`#sha256=` / `#egg=`）は保持する。`-r` / `--requirement`（`--requirement=other.txt` 含む）は再帰的に追跡する。`-c` / `--constraint` はversion制約の情報源としてのみ読み、`LoadedManifest.constraints` に積み、依存宣言とは合流させない（ファイル欠如はwarning）。`-e ./path` とlocal path指定はworkspace/first-party候補として扱い、distribution名は空のopaque依存として記録する。VCS URL・direct URL指定は `name @ url` 形式または `#egg=` からdistribution名を抽出し、抽出できない場合はopaque依存としてunused判定の対象外にする。environment markerは保持し、§10の判定で使う。

`setup.py` は `setup()` 呼び出し本体のみを静的パースする。リスト走査が途中で破綻した場合は `SetupPyPartiallyStatic` warningを出す。複数manifest sourceのmetadataは pyproject.toml > setup.cfg > setup.py の優先順位でマージし、衝突時は `MetadataConflict` warningを出して上位を保持する。`[tool.uv.workspace]` membersはStep 2で読み込んだ `UvWorkspaceHint` を `LoadedManifest.uv_workspace` にコピーし、キャッシュhash入力に使う。

`setup.py` が静的に解析できない場合(動的な `install_requires` 構築など)は、warningを出してそのsourceをskipし、他のsourceで解析を継続する。`[project]` の `dynamic = ["dependencies"]` が指定されている場合は、setuptoolsの慣習に従い `requirements*.txt` 側を依存宣言の実体として読む。

`[dependency-groups]` は、build metadataには含めない開発用途の依存を `pyproject.toml` に格納する標準仕様。lint/test/docs用の依存を扱うため、`chokkin` ではmain dependencyとは別contextとして扱う。

Python packagingのentry pointsは、distributionが提供するcomponentを他のコードやinstallerに知らせる仕組みで、`console_scripts` はインストール時にCLI wrapperを作るために使われる。`chokkin` は `[project.scripts]`、`[project.gui-scripts]`、`[project.entry-points]` をentry rootとして扱う。

## 5. 設定ファイル仕様

設定は `pyproject.toml` の `[tool.chokkin]` を第一候補にする。Knip体験に寄せるなら、別ファイルとして `chokkin.toml` と `.chokkin.toml` も許容するが、Pythonでは `pyproject.toml` 集約が自然。複数ソースは `.chokkin.toml` → `chokkin.toml` → `pyproject.toml` の順にマージし後勝ち。配列・mapは置換だが、`plugins` と `dependencies` のみキー単位マージ（`celery = true` だけ指定しても既定有効のpluginは維持される）。

最小設定例。

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
target_version = "py311"  # 解析対象projectのPython。stdlib判定と構文解析の基準
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

[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]
"python-dotenv" = ["dotenv"]
"scikit-learn" = ["sklearn"]

[tool.chokkin.binary_map]
# CLI名 -> distribution名。CHK008/CHK002のbinary usage判定に使う
"pytest" = "pytest"
"alembic" = "alembic"
"sphinx-build" = "Sphinx"

[tool.chokkin.plugins]
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
[tool.chokkin.workspaces.api]
path = "services/api"
entry = ["src/api/main.py"]
project = ["src/**/*.py", "tests/**/*.py"]

[tool.chokkin.workspaces.worker]
path = "services/worker"
entry = ["src/worker/__main__.py"]
project = ["src/**/*.py", "tests/**/*.py"]
```

uv workspaceも読む。uvのworkspaceは複数packageをまとめて管理する仕組みで、各memberが自分の `pyproject.toml` を持ち、workspace全体で単一lockfileを共有する。`tool.uv.workspace.members` がある場合は自動でworkspace modeに入り、確定rootから下方向に member glob と member `pyproject.toml` を解決する。v0.2では解決済みmemberを `LoadedConfig.workspace_members` として保持し、member別の `LoadedManifest` / source inventory を `ProbeReport.workspace_inputs` に載せる。resolver は `workspace_members` を受け取り、cross-member import を first-party として扱い、各 `ResolvedImport` に import元の `workspace_member` を付与する。Step 10 は `--strict` 時に `ResolvedImport.workspace_member` と member 別 manifest を照合し、root で宣言済みでも import元 member が直接宣言していない third-party import を CHK003 として報告し、member内で宣言済みでもruntime使用に対してcontextが合わない場合は CHK005 として報告する。Step 12以降のissue/reportersは `workspace_member` を保持し、human/GitHub系reporterでは subject に `member:` prefix を付け、JSONでは `workspace_member` fieldを出力する。以後の段階で各memberの依存・source・entryを別々に解析しつつ、root dependencyとの関係を判断する。

`chokkin --init` は、auto discoveryで検出したlayout・entry・dependency groupを反映した `[tool.chokkin]` の雛形を `pyproject.toml` に追記する。既存の `[tool.chokkin]` がある場合は上書きせずexit code 2で終了する。

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
3. manifest extraction     # 実装済み: src/manifest/ (`extract_manifest`)
4. source file discovery     # 実装済み: src/sources/ (`discover_sources`)
5. config/plugin extraction  # 実装済み: src/plugins/ (`extract_plugin_hints`)
6. Python parse              # 実装済み: src/parser/ (`parse_file`, `parse_project_sources`)
7. import resolution         # 実装済み: src/resolver/ (`resolve_imports`, bundled maps)
8. entry root construction    # 実装済み: src/entry/ (`build_entry_roots`, `apply_entry_plan`)
9. reachability analysis     # 実装済み: src/reachability/ (`analyze_reachability`, `trace_to_file`)
10. dependency reconciliation # 実装済み: src/rules/deps/ (`reconcile_dependencies`, CHK002–CHK009)
11. symbol usage analysis    # 実装済み: src/rules/symbols/ (`analyze_symbols`, CHK006–CHK007, CHK010)
12. issue emission
13. optional fix
```

Python parserはRust実装でよい。Ruff ecosystemのparserを使うか、RustPython parserを使うかはライセンス・保守性・Python新構文対応速度で選ぶ。ただし、Ruffのparser crate群をAstralが安定APIとして公開し続ける保証はないため、採用する場合はversion固定またはvendoring前提のリスクを織り込む。重要なのは、ASTだけではなくtoken位置・comments・string literalを保持すること。`# chokkin: ignore[...]`、`__all__`、`TYPE_CHECKING`、`importlib.import_module("...")`、framework設定のstring literalを拾う必要がある。

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

binary名からdistribution名への逆引き(CHK008、binary usage判定)も同じ多層戦略にする。`.venv` があれば `entry_points.txt` / `RECORD` のscriptsを読み、なければbundled binary map、最後にuser定義の `[tool.chokkin.binary_map]` を使う。import名問題と同型の課題であり、専用のfallback dataが必要になる。

重要なのは、`uvx chokkin` はprojectの仮想環境ではなく、`chokkin` 自身の一時環境で動く点。project venv必須にしてはいけない。`.venv` があれば読む、なければmanifest/lockfile/bundled map/user mapだけで解析する設計にする。

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

contextは依存だけでなく**file側にも割り当てる**。CHK005(misplaced)はこのfile contextに依存するため、割当規則を固定する。

```text
src/** / flat layoutのpackage/** / [project.scripts]到達file -> runtime
tests/** / conftest.py / *_test.py / test_*.py             -> test
docs/**                                                     -> docs
noxfile.py / 各tool設定が参照するscript                      -> dev
scripts/**                                                  -> dev (設定で変更可)
plugin / [tool.chokkin] のcontext指定が上記を上書きする
```

判定例。

```text
runtime codeで import requests
  [project.dependencies] に requests がある
    -> OK

runtime codeで import yaml
  PyYAML がどこにもない
    -> CHK003 missing_dependency

tests/ だけで import pytest
  dependency-groups.dev/test に pytest がある
    -> OK

src/ で import pytest
  dependency-groups.dev にしか pytest がない
    -> CHK005 misplaced_dependency

src/ で import urllib3
  requests のtransitive dependencyとして入っているだけ
    -> CHK004 transitive_dependency

[project.dependencies] に boto3 がある
  import/config/binary usageがどこにもない
    -> CHK002 unused_dependency
```

判定の優先順位を固定する。宣言されていないimportは、**lockfileの推移閉包で解決できればCHK004、できなければCHK003**とする。lockfileが存在しない場合(requirements.txtのみの環境など)はtransitive判定が不可能なため、CHK004はCHK003に縮退し、その旨をmessageに含める。

environment markerとextrasの扱いも定める。

```text
marker付き依存 (例: pywin32; sys_platform == "win32")
  -> 解析環境では未使用に見えても誤検知になりやすい
  -> unused判定のconfidenceを1段下げ、defaultではwarning、--strict時のみerror

extra指定 (例: requests[security])
  -> extraが有効化するtransitive依存を推移閉包に含めてCHK004判定する

stub package (types-* / *-stubs)
  -> importされないため素朴にはCHK002になる
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
uvx chokkin --fix
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
uvx chokkin --fix --allow-remove-files
```

missing dependency追加は別フラグにする。

```bash
uvx chokkin --fix --add-missing
```

ただし、`yaml -> PyYAML` のように一意に解決できる場合だけ追加する。候補が複数ある場合はsuggestionに留める。

manifest編集はformat保持が重要。Rustなら `toml_edit` を使い、commentsと順序を極力維持する。requirements系はline-based編集で、hash付きrequirementsやconstraintsは原則自動編集しない。

書き込み安全性: manifest編集は同一ディレクトリへの一時ファイル作成→`rename` でアトミックに置換する(`fix/write.rs`)。既存ファイルのpermissionsは可能な範囲で引き継ぐ。fix対象パスはproject root内に収まることを検証し、ルート外への書き込みはskipする。requirementsの `-r`/`-c` インクルードとDjango `settings.py` 探索も同様にルート封じ込めする。

lockfileとの整合にも注意する。`pyproject.toml` を編集するとuv.lock / poetry.lock等は古くなるが、`chokkin` はlockfileを直接編集しない。fix適用後に `uv lock` / `poetry lock` の実行を促すメッセージを出力する。

## 14. 既存Pythonエコシステムの課題と解決

|課題                            |具体例                                                            |chokkinでの解決                                                                                  |
|------------------------------|---------------------------------------------------------------|-------------------------------------------------------------------------------------------|
|distribution名とimport名が一致しない   |`PyYAML -> yaml`, `Pillow -> PIL`, `python-dotenv -> dotenv`   |Core Metadataの `Import-Name` / `Import-Namespace`、`.venv` metadata、bundled map、user mapを重ねる|
|`uvx` 実行時にproject venvが見えない   |`uvx chokkin` はchokkin用一時環境で動く                                     |project venv必須にしない。manifest/lockfile/static mapを主情報源にする                                    |
|frameworkが文字列でmoduleを参照する     |Django `INSTALLED_APPS`, Celery autodiscover, Sphinx extensions|pluginでstring literalをmodule reference化する                                                  |
|decoratorsが外部entryになる         |FastAPI route, pytest fixture, Celery task                     |decorator patternをexternally usedとして扱う                                                     |
|libraryのpublic APIは外部利用される    |内部参照がなくても公開APIかもしれない                                           |app/library modeを分け、libraryではunused exports/filesを低confidenceにする                           |
|dev/test/docs/lint/type依存が混在する|`pytest` がmain dependenciesにある                                 |dependency contextを導入し、misplaced dependencyを出す                                             |
|namespace packageがある          |`google.*`, `zope.*`                                           |namespace package modeと `Import-Namespace` を使う                                             |
|dynamic importを完全には解けない       |`importlib.import_module(name)`                                |literalは解く。非literalはopaque dynamic importとしてconfidenceを下げる                                 |
|monorepo/workspaceで依存境界が曖昧    |root depsをmemberが使う                                            |workspace graphを作り、`--strict` でmemberごとの直接依存を要求する                                          |
|auto-fixが危険                   |dead code削除で実行時破壊                                              |default fixはmanifest中心。file/code削除は明示フラグ必須                                                 |

Python packagingでは、`Requires-Dist` が依存distributionを表し、optional featureは `Provides-Extra` とextra markerで表される。`chokkin` はこのmetadataの考え方に合わせ、runtime dependency、optional dependency、dependency groupを区別して扱う。

## 15. Rust実装とpip配布

構成はこれでよい。

```text
chokkin/
  Cargo.toml
  pyproject.toml
  src/
    main.rs
    discovery/   # 実装済み: pipeline step 1 (root discovery)
    config/      # 実装済み: pipeline step 2 (config load)
    cli.rs          # CLI argument parsing (`clap`, Phase 1)
    pipeline/       # `probe_project` (steps 1–4), `analyze_project` (steps 1–13)
    manifest/    # 実装済み: pipeline step 3 (manifest extraction)
    sources/     # 実装済み: pipeline step 4 (source file discovery)
    plugins/     # 実装済み: pipeline step 5 (config/plugin extraction)
    graph/       # 実装済み: graph skeleton + import 辺 (`build_graph_skeleton`, `add_parsed_imports`)
    parser/      # 実装済み: pipeline step 6 (`parse_file`, `parse_project_sources`)
    resolver/    # 実装済み: pipeline step 7 (`resolve_imports`, bundled maps, venv RECORD/entry_points)
    entry/       # 実装済み: pipeline step 8 (`build_entry_roots`, `apply_entry_plan`)
    reachability/ # 実装済み: pipeline step 9 (`analyze_reachability`, `trace_to_file`)
    rules/       # 実装済み: step 10 `rules/deps/` (`reconcile_dependencies`, CHK002–CHK009);
                 #           step 11 `rules/symbols/` (`analyze_symbols`, CHK006–CHK007, CHK010);
                 #           step 12 (`emit_issues`, `explain_issue`, ignore/filter)
    reporters/   # 実装済み: default / compact / json / markdown reporter
    fix/         # 実装済み: step 13 (`apply_fixes` — pyproject/requirements/setup.cfg; atomic write, root containment)
```

`pyproject.toml` は概ねこうする。

```toml
[build-system]
requires = ["maturin>=1.8,<2"]
build-backend = "maturin"

[project]
name = "chokkin"
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
Homepage = "https://github.com/<org>/chokkin"
Repository = "https://github.com/<org>/chokkin"
Issues = "https://github.com/<org>/chokkin/issues"

[tool.maturin]
bindings = "bin"
strip = true
```

Rust binaryをwheelに入れるだけならPython extension moduleは不要。`pip install chokkin`、`uvx chokkin`、`pipx run chokkin` の全てで同じnative binaryを起動できる。なお、`requires-python` は配布上の制約に過ぎない。解析対象projectのPythonバージョンは `target_version` で独立に指定でき、chokkin自身が動く環境と関係なく古いprojectも解析できる。

配布ではprebuilt wheelが必須。source buildだけにすると、ユーザー側にRust toolchainが必要になり、`uvx chokkin` の体験が悪化する。最低限のwheel targetは以下。

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
- uvx chokkin で起動
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

v0.2 時点の JSON reporter / baseline file は draft schema として扱い、互換性方針と migration note は `docs/dev/schema-migration-notes.md` に置く。stable JSON schema は Phase 3 で凍結し、v1.0 で semver 契約の一部にする。

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
exit   : 空projectと小規模sample projectで uvx chokkin が動き、wheelが全targetでbuildできる
```

### Phase 1: v0.1 MVP(+6〜8週)

```text
目標   : §16 v0.1 scopeの実装と、誤検知率の実測
成果物 :
  - manifest extraction / import resolution / reachability (§4, §6, §7)
  - CHK001-CHK010 (unused_exportはpreview)
  - pytest / django / fastapi plugin
  - default / compact / json / markdown reporter
  - --production / --strict / --explain / --trace / --fix (dependency削除のみ)
検証   : 既知のOSS project 20個 (Django app / FastAPI app / library / monorepo混在)
         でdogfoodingし、CHK002/CHK003の誤検知を分類・記録する
exit   : 検証セットでunused dependencyの誤検知率 5%未満、
         crash 0、cold実行がmedium projectで2s以内
```

検証ハーネスは `scripts/oss-clones.manifest` (20 project、tag pinned) と
`scripts/clone-oss-fixtures.sh` / `scripts/oss-metrics.sh` (`make oss-clones` /
`make oss-metrics`)。誤検知のground truthは `scripts/oss-fixtures.labels.tsv`
に `fp`/`tp` で記録し、未分類が残るとFP gateは通らない。最新の計測結果は
`docs/dev/oss-validation-report.md` に scorecard として残す。

FP gateは「報告された CHK002」を分母にするため、**何も報告しなければ自明に通る**。
これを塞ぐため、ハーネスは `scripts/oss-recall.manifest` の **recall sentinel**
(意図的に未使用依存を含む in-repo fixture) を OSS clone と一緒に計測し、`tp`
ラベルが findings に現れなければ **recall gate を失敗**させる (`pass_recall`)。
これが Phase 1.5 の誤検知是正が「全件抑制」へ退化していないことの保証になる。

**現状 (v0.1.0): §17 exit criteria達成。** Phase 1.5 完了後の OSS 20 件検証で
CHK002 誤検知率 **0.0% (0 FP / 2 reported)**、recall sentinel **2/2 検出**、
crash 0、cold 実行 medium 最遅でも 2s 以内。scorecard は
`docs/dev/oss-validation-report.md`。PyPI v0.1.0 はリリース済み。

### Phase 1.5: v0.1 誤検知是正(リリースブロッカー) — ✅ 完了

```text
目標   : §17のFP gate (CHK002誤検知率 5%未満) を通す
背景   : OSS 20件の検証でCHK002誤検知率100% (155/155)。crash・速度は合格済み。
         根因は「実際には使われている依存」をchokkinが利用と結べないことに集約され、
         検出algorithmではなくpackage-module-map / binary map / context判定の
         data・解像度不足が主因 (§21の指摘どおり)。
成果物 : 誤検知削減インパクト順 (括弧内は155件中の寄与):
  1. binary + config usage detection (110件 / 71%)
     - [tool.<name>] table・[project.scripts]・dist-info entry_points・
       .pre-commit-config.yaml・tox/nox env・CI step から dev toolのCLI利用を
       解決し、CHK008判定とCHK002の「binary usage found」に反映する。
     - §3のCHK008とbinary mapを前倒し実装するのが本丸。mypy / ruff / pytest /
       sphinx / mkdocs / twine / coverage 等がこれで「利用」と判定される。
  2. dependency-group / extras の context認識 (PDM/Hatch group分を含む)
     - PEP 735 [dependency-groups]・PDM/Hatch group・requirements-*.txt を
       dev contextとして解釈する (現状は "unsupported in v0.1" で素通り)。
     - dev群はCHK002をdefaultで抑制し、--strict時のみerrorにする緩いpolicy。
     - Phase 2予定だったPDM/Hatch manifest対応の「読取り」部分を前倒し。
  3. optional / conditional import tracing
     - try/except ImportError・sys.platform分岐・TYPE_CHECKING・extra guard下の
       importを、宣言extra/依存の利用としてカウントする。
     - tzdata / brotli / argon2-cffi / colorama / trio 等の取りこぼしを解消。
  4. package-module-map拡充 + 自己参照guard
     - bundled mapにimport名≠distribution名のpair追加 (python-multipart→
       multipart, pyopenssl→OpenSSL, pysocks→socks 等)。
     - projectが自身を `pkg[extra]` で宣言した場合は常に利用扱いとし、CHK002
       候補から除外する。
検証   : `make oss-metrics ARGS=--gate` を再実行し、scripts/oss-fixtures.labels.tsv
         を再分類する。CHK003 (現状1747件) の誤検知も大幅減を確認 (gateではないが
         §20の信頼性指標)。各根因にregression fixtureを追加してCI化する。
exit   : CHK002誤検知率 5%未満 (未分類0)、recall sentinel全件検出 (過剰抑制ガード)、
         crash 0維持、cold実行 medium 2s維持。
```

### Phase 2: v0.2 導入支援(+6〜8週)

```text
目標   : 既存大規模projectへの導入障壁を下げる
成果物 : §16 v0.2 scope
  - baseline file (既存issueの凍結、§18)
  - uv workspace / Poetry / PDM / Hatch (group読取りはPhase 1.5で前倒し済み、
    ここではworkspace解決・推移閉包まで完成させる)
  - SARIF / GitHub Actions reporter
  - cache (§19のwarm目標達成)
  - plugin拡充 (flask / celery / tox / nox / pre-commit / github-actions。
    tox/nox/pre-commit/GitHub Actionsのbinary usage plugin化は初期実装済み。
    Flask/Celery は static app command/reference extraction を初期実装済み。
    Sphinx/MkDocs/Alembic は conventional config/entry extraction と Sphinx literal
    `extensions` module refs を初期実装済み。
    GitHub Actions は single-line `run:` と block scalar `run: |` / `run: >` に対応。
    notebook parsing は `.ipynb` discovery と Python code-cell extraction を初期実装済み。
    Flask/Celery decorator由来 module refs は literal scan として初期実装済み)
  - JSON reporter / baseline draft schema と migration 方針 (`docs/dev/schema-migration-notes.md`)
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
  (20件のharnessは `scripts/oss-*` / `make oss-metrics` として整備済み。
   各リリース前に再計測し `docs/dev/oss-validation-report.md` を更新する)
```

リリース判断は期日ではなくexit criteriaで行う。特にPhase 1の誤検知率は、§20の「信頼を失いにくい」方針の定量版であり、未達のままv0.1を出さない。

## 18. ignore仕様

inline ignore。

```python
from legacy import old_api  # chokkin: ignore[CHK003]

def public_hook():  # chokkin: ignore[CHK006]
    ...
```

file-level ignore。ファイル先頭のcomment block(最初のimport文・実行文より前)でのみ有効とする。

```python
# chokkin: file-ignore[CHK006]
```

config ignore。keyはrule codeに統一する。値のpattern構文はruleの対象に依存し、dependency系ruleではdistribution名のglob、file系ruleではpath glob、symbol系ruleでは `path:symbol_glob` 形式とする。

```toml
[tool.chokkin.ignore]
CHK001 = [
  "src/acme/migrations/**/*.py",
  "src/acme/generated/**/*.py",
]
CHK002 = ["boto3", "google-cloud-*"]
CHK003 = ["pkg_resources"]
CHK006 = ["src/acme/public_api.py:*"]
```

baselineも導入しやすさに効く。

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --baseline chokkin-baseline.json
```

baselineは既存issueを黙らせ、新規issueだけCIで落とすためのもの。大規模既存projectでは必須。fingerprintは `rule_id + stable target` を基本とし、file pathは `/` 区切りに正規化し、dependency/file issueではline numberをkeyに含めない。

## 19. パフォーマンス目標

Rustで作るなら、体感はRuff寄せにする。サイズの目安は、small ≈ 100 files以下、medium ≈ 1,000 files、large ≈ 10,000 files以上とする。

```text
small project      < 300ms
medium project     < 2s
large monorepo     < 10s cold
large monorepo     < 2s warm cache
```

cache は project root 配下の `.chokkin/cache` を既定directoryにする。`--no-cache` は cache read/write を両方無効化し、未実装またはstale疑いのcache unitが解析結果を変えないようにする。v0.2初期は `CacheOptions` でpolicyだけを先に通し、parse/manifest/plugin/module index の各unitを後続PRで保守的に追加する。parse cache のkeyは `CacheKeyContext` と `SourceFingerprint` を組み合わせ、mtime/sizeに加えてfile bytesのstable content hashを含める。`ParseCacheStore` によるin-memory reuseに加え、disk永続化は `.chokkin/cache/parse/<key>.json` に `ParsedModule` JSON を保存する。corrupt JSON はmiss扱いにしてsourceを再parseする。

cache keyは以下を使う。

```text
chokkin version
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

config/manifest scan cache は `ScanInputFingerprints` で、実際に読んだ config file と manifest file の `SourceFingerprint` を保持する。uv workspace だけを持つ `pyproject.toml` も config input として扱う。requirements の再帰読取り結果は `ManifestSources.requirements_files` を通じて fingerprint 対象にする。`ScanCacheKey` と `ScanCacheRecord` は `.chokkin/cache/scan/<key>.json` のmetadata envelopeとして使う。`read_scan_record` / `write_scan_record` による disk backend と、`read_scan_payload` / `write_scan_payload` による typed JSON payload slot は初期実装済みで、corrupt JSON、key mismatch、payload shape mismatch はmiss扱いにする。generic config scanner (`ConfigScanResult`) と full manifest extraction (`LoadedManifest`) のpayload wiring は初期実装済み。manifest payload は extraction 後に判明する recursive requirements 入力 fingerprint も payload 内に保持し、cache hit 時に再検証して stale hit を避ける。module index は path-based payload として保存し、current graph の `FileId` に再解決する。

parallelize対象は、file discovery、parse、import extraction、symbol extraction、plugin config parse。graph resolutionだけは集約後に行う。

warm cache の性能確認は `benches/cache.rs` の `parse_cache_warm` を使う。`make bench` は manifest/source/cache の全benchを走らせ、baseline比較は `make bench-save BASELINE=main` → `make bench-cmp BASELINE=main` で確認する。

tox/nox/pre-commit/GitHub Actions は v0.2 plugin 拡充の初期実装として `src/plugins/devtools.rs` に集約し、`tox.ini` / `noxfile.py` / `.pre-commit-config.yaml` / `.github/workflows/*.yml` または対応する `[tool.*]` から binary usage を出す。GitHub Actions は single-line `run:` と block scalar `run: |` / `run: >` の command parse に対応する。

Flask/Celery は `src/plugins/flask.rs` と `src/plugins/celery.rs` で初期実装し、`.flaskenv` の `FLASK_APP`、script内の `flask --app`、`project.scripts` / scripts / bin にある `celery -A` / `celery --app` から symbol reference と binary usage を出す。Flask は literal route decorators (`@app.route`, `@bp.get` など)、Celery は literal task decorators (`@shared_task`, `@app.task` など) を持つ module を module reference として扱う。

Sphinx/MkDocs/Alembic は `src/plugins/doctools.rs` で初期実装し、`docs/conf.py` と `alembic/env.py` を plugin entry にし、`mkdocs.yml` / `mkdocs.yaml`、`docs/conf.py`、`alembic.ini` から binary usage を出す。Sphinx `extensions = [...]` の literal string は module reference として扱う。MkDocs は static config scan で `material` theme と既知 plugin (`mkdocstrings`, `autorefs` など) を used distribution として扱う。

notebook parsing は v0.2 plugin 拡充の初期実装として、source discovery が `.ipynb` を `FileKind::Notebook` として拾い、parser が `cells[].cell_type == "code"` の `source` だけを連結して既存の Python static parser に渡す。markdown/raw cell と outputs は無視し、notebook JSON が壊れている場合は per-file warning diagnostic に留める。

## 20. 注意点

最も重要な設計判断は、project codeを実行しないこと。PythonではDjango settingsやsetup.pyをimportして解析する設計にすると、DB接続、環境変数依存、副作用、任意コード実行の問題が出る。`chokkin` はstatic parseに徹し、runtime traceは将来の明示opt-inに分離する。

次に、`unused exports` を強く売りすぎないこと。Python libraryでは「内部から未使用」でも外部ユーザーがimportしている可能性がある。最初の訴求は `unused dependencies` と `unused files` に置き、`unused exports` はconfidence付きの補助機能として出す方が信頼を失いにくい。

最後に、`chokkin` の価値は検出アルゴリズム単体ではなく、plugin databaseとpackage-module-mapの品質に依存する。MVP時点から、誤検知報告を集めやすい `--explain`、`--trace`、`--reporter json`、`package_module_map` を用意しておく。

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
