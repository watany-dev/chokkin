# 精度(precision)改善のための課題一覧 — v0.7.2 OSS 35 プロジェクト調査

対象: chokkin v0.7.2(`target/release/chokkin`)を OSS 35 プロジェクト
(airflow, mlflow, transformers, llama_index, langchain, fastmcp, mcp-python-sdk,
django, poetry, black, sqlalchemy ほか)に既定設定で実行し、warning 以上の報告を
手で分類した。clone は `target/oss-clones/<slug>/`、JSON 出力は
scratchpad の `out/<slug>.json`。

## 0. 全体像

| code | error/warning 件数 | 主な誤検知バケット | 本文書の課題 |
|---|---|---|---|
| CHK004 | 1,951(certain 1,786) | monorepo の「他 member が宣言 → 自 member 未宣言」。定義上は真陽性だが出力の 6 割を占め、他の issue を埋めてしまう | P1, P9 |
| CHK010 | 691 | optional な第三者 import(mlflow flavor / transformers integration)、`sys.path` 操作、nested 独立 pyproject、bundled map の穴 | P6, P7, P8 |
| CHK006 | 524 | workspace root が常に App 扱い、member の自 CLI が app signal、動的 import / 文字列参照の未考慮 | **P1, P2, P3** |
| CHK003 | 173(すべて certain) | requirements ファイル名の固定、`deferred` import を certain 扱い、`is_x_available()` guard | **P4, P5** |
| CHK001 | 130(error) | 文字列 dotted path による参照、build hook、生成ファイル、tests 配下 fixture | **P3, P7** |
| CHK008 | 113(certain) | 「使用」の根拠が `[tool.X]` セクションの存在だけ(自己参照) | **P5** |
| CHK007 | 52 | P1/P2 と同根 | P1, P2 |
| CHK005 | 48 | member の第三者 import に root の dev 宣言が当たる、flavor 慣習 | P9 |
| CHK009 | 12 | lint group / extras への意図的重複を certain で報告 | P10 |

真陽性と判断して除外したもの: airflow の PEP 723 CHK002(`scripts/ci/prek/*.py` の
wcmatch / hatchling は本当に import されていない)、airflow `cli_config.py` の
`ARG_WITHOUT_GOSSIP`(celery provider 側に重複定義)、`airflow/stats.py`
(deprecated shim)、llama_index integrations の `requests` 未宣言(CHK004)。

優先度は「件数 × 既定表示(warning 以上)に出るか × 修正の局所性」で付けた。

---

## P1. workspace root が無条件に App になる(CHK006/CHK007 の warning 化)

**症状** mcp-python-sdk(CHK006 10 + CHK007 10)、mlflow(CHK006 158)、
airflow(267)、fastmcp(60 + 40)、llama_index(CHK007 2)。いずれも root 自身は
library(`src/mcp`, `mlflow/`, `fastmcp_slim/fastmcp`)なのに `Mode: app` と
判定され、CHK006 の severity が Info → Warning に上がる。

**原因** `src/entry/mode.rs` の `resolve_project_mode` が
`workspace_member_count > 1` なら root の性質を一切見ずに `ProjectMode::App`
を返す。`--probe` の `Workspace: 5 members`(mlflow)で確認できる。

**修正案**
1. workspace の有無は mode の決定材料から外し、root にも member と同じ
   `is_library_member` 相当の判定(`names_distribution` + package dir +
   app signal 無し)を適用する。
2. `EntryPlan::mode_for(path)` は既に path 単位で Library/App を切り替えて
   いるので、root package のパスも `library_members` 相当の集合に入れれば
   `unused_export_severity` が自然に Info に落ちる。
3. 判定根拠を `--probe` に `Mode: app (reason: workspace)` のように出す。

**健全性の考え方** mode は severity にしか影響しないので、誤って Library に
寄せても検出が消えるのではなく Info に落ちるだけ(既定フィルタで非表示)。
false positive が warning に出るコストの方が高いので、迷ったら Library 側。

関連: #657(library→app 判定)。

## P2. library member の自 CLI(`[project.scripts]`)が app signal になる

**症状** fastmcp_slim(`fastmcp = "fastmcp.cli:app"`)が App member と判定され、
CHK007 40 件 + CHK006 59 件が warning。mcp-python-sdk root の
`mcp = "mcp.cli:app [cli]"` も同様に P1 と重なる。

**原因** `src/entry/mode.rs::is_library_member` が
`has_clear_app_signals(manifest, sources, &detect_auto_entries(sources), false)`
を `own_cli_is_library = false` で呼ぶ。docコメントは airflow-core /
llama-dev のような「CLI が本体の member」を想定しているが、ライブラリが自分の
console script を持つのは一般的(click, black, pdm, poetry もそう)。

**修正案** console script の entry が自パッケージ内(`names_distribution` の
package dir 配下)を指す場合は app signal にしない(`own_cli_is_library = true`
を既定にする)。「CLI が本体」の member は `manage.py` 等の `APP_ENTRY_FILE_NAMES`
か、`[project]` が `packages` を持たない等の別シグナルで拾う。
airflow-core / llama-dev が本当に App 扱いであるべきかも再検討(どちらも
配布される package なので Library で問題ないはず)。

## P3. CHK006 / CHK001 が動的 import と文字列参照を考慮しない

**症状**
- poetry: `poetry/console/commands/**` の `*Command` クラス 29 件が CHK006。
  実体は `application.py:43-47` の
  `import_module("poetry.console.commands." + ".".join(words))` と
  `getattr(module, "".join(c.title() for c in words) + "Command")`。
- airflow: `cli_config.py:43-53 lazy_load_command → import_string(path)` で
  `"airflow.cli.commands.cheat_sheet_command.cheat_sheet"` のように文字列で
  参照される関数が CHK006、モジュール `cheat_sheet_command.py` が CHK001。
  `config_templates/config.yml:1018 default: "airflow.utils.log.timezone_aware.TimezoneAware"`
  や root `pyproject.toml:1302 module="airflow.config_templates.default_webserver_config"`
  の参照先も CHK001。
- mlflow: `tests/tracing/test_fluent.py:2384
  python_model="tests/tracing/sample_code/model_with_add_trace.py"` のように
  ファイルパス文字列で参照される fixture 50 件が CHK001(error)。

**原因**
- `src/parser/dynamic.rs` / `visit.rs:595-660` は `importlib.import_module` /
  `__import__` の `module_prefix` を `parsed.dynamic_import_prefixes` に積むが、
  消費しているのは `src/reachability/bfs.rs:398,461` のみ。CHK006 側
  (`src/rules/symbols/external.rs::collect_external_symbols`)は entry root の
  symbol、plugin `symbol_refs`、登録デコレータ(`REGISTRATION_DECORATOR_SUFFIXES`、
  `pytest.mark.*`)しか見ない。
- `import_string` / `lazy_load_command` のようなカスタム loader は認識しない。
- 文字列リテラルの dotted path / ファイルパスは参照として扱わない。
- `src/rules/chk001.rs::chk001_severity` は Library+Maybe 以外すべて Error
  なので、tests 配下の fixture も error で出る。

**修正案(健全な過大近似)**
1. `dynamic_import_prefixes` に prefix `p` があるとき、`p` 配下の全モジュールの
   top-level 公開 symbol を external reference 扱いにする(poetry は
   `poetry.console.commands.` prefix で 29 件がすべて消える)。
   `has_opaque_dynamic_import`(prefix 不明)の場合は、その module から到達
   可能な package 全体を external 扱いにして CHK006 を Info に落とす。
2. 文字列リテラルのうち `^[A-Za-z_][\w]*(\.[A-Za-z_]\w*)+$` に一致し、先頭
   segment が first-party top-level package と一致するものは
   「module 参照 or module.attr 参照」として CHK001/CHK006 の使用根拠にする。
   同じ正規化を YAML / TOML / INI / JSON の値文字列(config_scan が既に読んで
   いるファイル)にも適用する。
3. 文字列リテラルが `.py` で終わり、存在するファイルに解決できるなら CHK001 の
   使用根拠にする。
4. 2・3 で拾った参照は confidence を `Maybe` に下げる根拠にも使える(「文字列で
   しか参照されていない」は人に見せる価値がある情報)。
5. `collect_external_symbols` に pytest hook(`pytest_configure`, `pytest_*`)、
   Sphinx `conf.py` の設定変数、`__getattr__` を持つモジュールの `__all__` 外
   symbol を追加する。

**形式手法の観点** CHK006/CHK001 は「参照が無い」ことを主張するルールなので、
参照の過大近似(多めに拾う)が健全。文字列参照の取り込みは recall を少し落とす
が、precision 側の損失(poetry 29 / airflow 61 / mlflow 50)の方が大きい。

関連: #488、#652。

## P4. CHK003 が certain を出し過ぎる(requirements ファイル・deferred・availability guard)

**症状** CHK003 の warning/error 173 件はすべて certain。
- django: `selenium` 23 件、`psycopg` 7 件、`pillow` 2 件。宣言は
  `tests/requirements/py3.txt:17 selenium >= 4.8.0`、`postgres.txt` にあるが
  manifest が読まない。import も `django/contrib/admin/tests.py:101-125` の
  関数内 import。
- transformers: `scipy` 23 件。`data/metrics/__init__.py:18-20
  if is_sklearn_available(): from scipy.stats import ...`、
  `loss/loss_deformable_detr.py:15-16 if is_scipy_available():`。
  `setup.py:140 "scipy"` は `_deps` にあるが extras に無い(宣言としては
  本当に欠けている可能性はあるが、guard 付きなので certain ではない)。
- airflow: `pydantic-ai` 36 件(devel-common 配下の例題・テスト)。
- mlflow: `torch` 11 件(すべて optional flavor 内)。

**原因**
- `src/manifest/extract.rs:120-140` の requirements ファイル名は root 直下の
  固定 6 件(`requirements.txt`, `requirements-dev.txt`, `dev-requirements.txt`,
  `requirements-docs.txt`, `requirements-tests.txt`, `requirements-test.txt`)。
  `requirements/*.txt`、`tests/requirements/*.txt`、`docs/requirements.txt`、
  `-r other.txt` include を扱わない。
- `src/rules/deps/missing.rs:428-450 collect_optional_imports` は
  `import.optional || import.platform_guarded` だけを見て `deferred`
  (関数内 import)を見ない。一方 `src/rules/deps/misplaced.rs:204-230
  import_strengths` は `deferred` を Likely に降格している(CHK003 と非対称)。
- `if is_x_available():` / `if TYPE_CHECKING:` 以外の条件 guard は
  `optional` にならない。

**修正案**
1. requirements の探索を「root と `requirements/`, `tests/`, `docs/` 配下の
   `*requirements*.txt` / `*.in`」に広げ、`-r` / `-c` include を再帰的に辿る
   (root 外へは出ない)。ディレクトリ名 / ファイル名から dev/test/docs context
   を推定する。
2. CHK003 の confidence を misplaced.rs と同じ強度表で決める:
   `deferred` → Likely、`optional`/`platform_guarded` → Likely(現状通り)、
   `if <call>():` 配下(呼び出し結果で分岐する import)→ Likely。
   「module top-level・無条件・非 TYPE_CHECKING」のみ certain に残す。
3. `_deps` のような dict/list に名前だけ現れる `setup.py` 定数は「宣言候補」
   として Maybe に落とす根拠にする(実行はしない、AST の文字列リテラルのみ)。

## P5. CHK008 の「使用」根拠が自己参照的(113 件 certain)

**症状** pre-commit 19、tox 13、coverage 12、mypy 12、ruff 11、sphinx-build 10、
pytest 8、black 8、flake8 7、isort 6 … ほぼ全プロジェクトで出る。
pre-commit は isolated env で動くので宣言不要。`[tool.ruff]` があるだけで
ruff が「使われている」とするのは循環論法(設定があるから使う、使うなら
宣言すべき)。

**原因** `src/plugins/config_scan.rs:94` が `[tool.X]` key をそのまま binary
使用に、`:266-293` が `.pre-commit-config.yaml` の存在を `pre-commit` 使用 +
各 hook entry に、`:299-322` が `tox.ini` の存在を `tox` 使用に、`:610` が
`docs/conf.py` を `sphinx-build` 使用にしている。一方 `:409-480` の tox deps /
extras は `used_distributions` にしか入らず宣言扱いにならない。
`src/rules/deps/binary.rs::detect_unlisted_binaries` は Warning/Certain 固定。

**修正案**
1. 使用根拠を「実行コマンド行」に限定する: Makefile / justfile / tox
   `commands` / nox session / `[tool.pdm.scripts]` / `[tool.poe.tasks]` /
   `[tool.hatch.envs.*.scripts]` / CI yaml の `run:` 行で binary 名が先頭
   token に現れたもの。`[tool.X]` セクションの存在は根拠にしない。
2. pre-commit hook は isolated env なので宣言不要。`language: system` の
   hook だけを対象にする。
3. tox `deps` / `extras`、requirements ファイル(P4)、
   `[tool.hatch.envs.*.dependencies]`、`[dependency-groups]` を宣言側に
   加える。
4. severity: 使用根拠が CI yaml / tox commands のみなら Likely、
   Makefile など開発者がローカルで叩く場所にあれば Certain。

関連: #564、#540。

## P6. CHK010(unresolved import)の 3 つの穴

**症状** 691 件(mlflow 224、transformers 206、llama_index 113、fastmcp 27)。

**6a. optional 第三者 import** mlflow の `dspy` 21 / `transformers` 14 /
`litellm` 13、transformers の `peft` 13 / `bitsandbytes` 9 / `mlx` 9。
flavor / integration モジュールが `try: import x` または
`if is_x_available()` で読む第三者パッケージで、宣言はどこにも無い(ユーザーが
入れる前提)。unresolved かつ `optional`/`deferred`/guard 付きなら Info に落とす
べき。現状 `optional` でも warning/likely。

**6b. `sys.path` 操作** transformers `tests/models/auto/test_configuration_auto.py:31-33`
`sys.path.append(str(Path(__file__).parent.parent.parent.parent / "utils"))`
→ `from test_module.custom_configuration import CustomConfig`
(実体 `utils/test_module/`)が 13 件。src 全体に `sys.path` の処理は無い
(grep 結果はコメント 2 件のみ)。`sys.path.(append|insert)` の引数が
`Path(__file__)` / `os.path.dirname(__file__)` から静的に辿れるときだけ
その directory を import root に加える。辿れないときは「追加 root 不明」
flag を立て、その module 内の unresolved を Maybe に落とす。

**6c. nested 独立 pyproject** fastmcp `examples/atproto_mcp/`(name
`atproto-mcp`、src layout、workspace 非 member)、`examples/smart_home/`
が root の一部として解析され `atproto_mcp` 9 / `smart_home` 7 件。
`[project]` を持つ nested `pyproject.toml` のディレクトリは、workspace
member でなければ既定で解析対象外(別プロジェクト)にする。
`--probe` に「skipped nested projects」を出す。

**6d. bundled map の穴** llama_index `azure` 11(namespace package:
`azure-*` 群)、`workflows` 10(`llama-index-workflows`)、`fitz`(PyMuPDF)。
namespace package は「宣言済み distribution の top-level 名の集合」に
lockfile の `[package]` 名 → top-level の対応を加えて解決する。

## P7. build hook / 生成ファイル / 補助スクリプトの CHK001・CHK010

**症状**
- airflow `airflow-core/hatch_build.py`(`airflow-core/pyproject.toml:262
  path = "./hatch_build.py"`)が CHK001。
- black `src/_black_version.py`(`pyproject.toml:112-113
  [tool.hatch.build.hooks.vcs] version-file`、`:206 known_first_party`)が
  CHK010 7 件。
- fastmcp `.agents/skills/release/scripts/changelog_entry.py`、`logo.py`、
  `tests/downstream/*.py`(別 harness から subprocess で起動)が CHK001。

**原因** `src/manifest/pyproject.rs:489` は
`tool.hatch.metadata.hooks.uv-dynamic-versioning` しか読まない。
`[tool.hatch.build.hooks.custom] path`、`[tool.hatch.build.hooks.vcs]
version-file`、setuptools_scm `write_to` / `version_file`、
`[tool.isort] known_first_party` の処理は存在しない。

**修正案**
1. build hook の `path` は entry root に加える(CHK001 対象外)。
2. `version-file` / `write_to` / `version_file` のパスは「生成される
   first-party module」として resolver に登録し、CHK010 の対象外にする。
3. `known_first_party` は first-party 名の追加ヒントとして読む。
4. `.github/`, `.agents/`, `scripts/`, `tools/`, `logo.py` のような
   repository 直下の補助スクリプトは、root に `[project]` package があるなら
   「script context」として CHK001 を Info に落とす(現状は Error 固定:
   `chk001_severity` は Library+Maybe のみ Warning)。

## P8. `.github/ui-preview/app.py` のような package 外 `app.py` が app signal

**症状** mlflow が App 判定される要因の一つ。`APP_ENTRY_FILE_NAMES =
["manage.py","asgi.py","wsgi.py","app.py"]` が Runtime context かつ package 外
にあれば app signal(`has_clear_app_signals`)。

**修正案** `app.py` は `asgi.py`/`wsgi.py`/`manage.py` より弱いシグナル。
root 直下か、`names_distribution` の package と並ぶ位置にある場合のみ採用し、
`.github/` / `docs/` / `examples/` / dot ディレクトリ配下は除外する。

## P9. CHK004 / CHK005 の monorepo 校正(1,951 件、出力の 6 割)

**症状**
- airflow providers: `packaging` 211(各 provider `__init__.py` の生成
  boilerplate)、`wtforms` 69、`sqlalchemy` 61。llama_index integrations:
  `requests` 98、`sqlalchemy` 60。langchain: `typing-extensions` 151。
  fastmcp root `examples/` の `rich` 24 / `pydantic` 8(root は
  `fastmcp-slim[client,server]` しか宣言しない)。
- `pydantic-core`(fastmcp 16)、`typing-extensions`、`mcp-types`
  (fastmcp_slim は `mcp-types` を宣言済みなのに root `examples/` が未宣言)
  のような「companion distribution」。
- llama_index CHK005 19: member の第三者 import に root の dev 宣言が当たり、
  member transitive(CHK004)が CHK005 warning に変わる。
- mlflow CHK005 13: flavor module が extras 宣言の distribution を runtime
  context で import する慣習(`mlflow/transformers/__init__.py` など)。

**評価** 定義上は真陽性(直接 import しているが直接宣言していない)で、
rule 自体は正しい。問題は (a) error/certain 固定で、同じ distribution が
provider 数だけ繰り返される、(b) 他 member が同じ distribution を宣言している
monorepo では「member に宣言を足す」以外の解決が無く、ユーザーは一括で
抑制したくなる、の 2 点。

**修正案**
1. workspace では distribution × member で集約し、1 member あたり 1 件に
   する(JSON には個別 import を `locations` で残す)。
2. 「import 元 member が依存する別 member がその distribution を直接宣言
   している」場合は Likely に落とす(airflow provider → apache-airflow-core
   → packaging)。transitive の中でも「自分のプロジェクトが管理している
   宣言」だからである。
3. companion の既知ペア(`pydantic` → `pydantic-core`、`pydantic` →
   `typing-extensions`、`mcp` → `mcp-types`、`httpx` → `httpcore`)を
   bundled map に持ち、宣言側が存在すれば Info に落とす。
4. `examples/` / `docs/` 配下の import は runtime context ではなく
   「example context」として、宣言不足を Info にする(fastmcp root 46 件)。

## P10. CHK009(重複宣言)の意図的重複

**症状** transformers 7(`[dependency-groups] lint` 等)、mlflow 4、poetry 1。
`ruff` を runtime と lint group の両方に書くのは「lint group だけ install
しても動く」ための意図的な重複。

**原因** `src/rules/deps/duplicate.rs:1-23` は runtime ⊇ group/extra のとき
duplicate(Certain)。extras 同士 / groups 同士は除外済み(#494)、
specifier 差は refinement(#507, #629)。

**修正案** group 名が `lint` / `dev` / `test(s)` / `docs` / `typing` の
いずれかで、runtime 側と specifier が同一なら Likely に落とす
(「install 単位を分けるための重複」は珍しくない)。

## P11. その他、1 プロジェクトで確認した小さな穴

- sqlalchemy: `[build-system] requires = ["cython>=3.3; platform_python_implementation == 'CPython'"]`
  が宣言 context にならず、`setup.py` / `lib/**/*.pyx` 隣接 `.py` からの
  `import Cython` 等が CHK010 8 件。build-system requires は「build
  context」の宣言として読む(`setup.py`, `hatch_build.py`, `conftest.py`
  の import に対して)。
- airflow `ARG_*` のように「同名 symbol が別 member に重複定義」されている
  ケースは真陽性だが、message に「別 member で同名定義あり」を添えると
  判断しやすい。

---

## 付録 A. 修正の優先順位(推定削減件数)

| 順 | 課題 | 削減見込み(warning 以上) | 変更箇所 |
|---|---|---|---|
| 1 | P1 workspace root の mode | CHK006/007 ≈ 500 → Info | `entry/mode.rs` |
| 2 | P2 自 CLI の app signal | fastmcp 99、(P1 と重複) | `entry/mode.rs` |
| 3 | P9-1,2 CHK004 の集約と Likely 降格 | ≈ 1,500 件を数十件に | `rules/deps/transitive` 系, reporter |
| 4 | P5 CHK008 使用根拠の見直し | ≈ 100 | `plugins/config_scan.rs`, `rules/deps/binary.rs` |
| 5 | P4 requirements 探索 + deferred/guard の confidence | CHK003 certain 173 → 大半 Likely | `manifest/extract.rs`, `rules/deps/missing.rs` |
| 6 | P3 動的 import / 文字列参照 | CHK006 ≈ 60、CHK001 ≈ 110 | `rules/symbols/external.rs`, `rules/chk001.rs`, parser |
| 7 | P6 CHK010 の optional/sys.path/nested | ≈ 400 → Info | `resolver/`, `rules/chk010` |
| 8 | P7 build hook / 生成ファイル | 20 前後 | `manifest/pyproject.rs`, `entry/` |
| 9 | P8, P10, P11 | 数件ずつ | — |

## 付録 B. 静的解析・形式手法の観点からの提案

1. **ルールごとの健全性の向きを spec に明記する。** CHK001/006/007
   (「参照が無い」主張)は参照集合を過大近似すべきで、文字列・動的 import・
   `getattr` を「不明な参照」として扱うと健全。CHK003/010(「宣言が無い」
   主張)は宣言集合を過大近似すべきで、requirements/tox/build-system/
   companion を宣言側に寄せると健全。CHK004/008 は両者の交差なので、
   不確かな側に応じて confidence を落とす。現状は confidence が
   `in_all` / `optional` のような少数の syntactic 特徴だけで決まり、
   「解析器がどの近似を使ったか」を反映していない。
2. **confidence を証拠の lattice にする。** `Certain` = 構文上無条件
   (top-level import、`__all__`)、`Likely` = 条件付き(deferred / guard /
   workspace 他 member 宣言)、`Maybe` = 解析器が諦めた(opaque dynamic
   import、`sys.path` 不明、dynamic metadata 部分抽出)。各 rule が
   「どの不確かさに触れたか」を bitset で持ち、最弱の証拠で confidence を
   決める。`--explain` にはその bitset を出す(`resolved via lockfile
   transitive closure` の行はこの形に近い)。
3. **差分テストを継続的に回す。** 既に `docs/dev/oss-differential-and-recall.md`
   がある。本調査の 35 プロジェクトについて「人手ラベル付き誤検知セット」
   (fingerprint の allowlist)を `tests/fixtures/oss-labels/` に置き、CI で
   precision の回帰を検出する。各課題の修正は対応する fingerprint が消える
   ことで検証できる。
4. **メタモルフィック性質で rule を縛る。** 例: (a) 任意の module に
   `importlib.import_module(<定数文字列>)` を足しても、その対象以外の CHK006
   が増えてはならない、(b) `[tool.uv.workspace]` を 1 member にして解析した
   結果の CHK006 severity は、全 workspace で解析したときの同 member の
   severity 以下でなければならない(P1 の性質)、(c) requirements ファイルを
   `-r` で分割しても結果は変わらない(P4)。proptest で生成した pyproject /
   モジュール木に対して検査できる。
5. **mutation testing の対象を rule の confidence 計算に広げる。**
   `docs/dev/mutants-phase-a.ja.md` の枝を `unused_export_severity` /
   `collect_optional_imports` / `import_strengths` に伸ばすと、
   「deferred を無視しても test が落ちない」(P4 の非対称)のような穴を機械的に
   見つけられる。

## 付録 C. 証拠ファイル一覧(clone 内の path:行)

- `fastmcp/pyproject.toml:3,49-52`(dynamic deps)、`fastmcp_slim/pyproject.toml`
  (`[project.scripts] fastmcp = "fastmcp.cli:app"`)、
  `examples/atproto_mcp/pyproject.toml`
- `mcp-python-sdk/pyproject.toml`(`mcp = "mcp.cli:app [cli]"`, members)
- `mlflow/pyproject.toml:132-133`、`.github/ui-preview/app.py`、
  `tests/tracing/test_fluent.py:2384`
- `poetry/src/poetry/console/application.py:43-47,142`
- `airflow/airflow-core/src/airflow/cli/cli_config.py:43-53,2280`、
  `airflow-core/pyproject.toml:262`、`pyproject.toml:1302`、
  `airflow-core/src/airflow/config_templates/config.yml:1018`
- `django/django/contrib/admin/tests.py:101-125`、`tests/requirements/py3.txt:17`
- `transformers/src/transformers/data/metrics/__init__.py:18-20`、
  `loss/loss_deformable_detr.py:15-16`、`setup.py:140`、
  `tests/models/auto/test_configuration_auto.py:31-33`
- `black/pyproject.toml:112-113,206`
- `sqlalchemy/pyproject.toml:5`
- `llama_index/llama-index-integrations/embeddings/llama-index-embeddings-anyscale/`
