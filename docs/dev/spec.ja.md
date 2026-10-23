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
1. pyproject.toml / setup.cfg / setup.py / requirements*.txt / lockfile を探索
2. src layout / flat layout / tests / scripts / docs / config files を推定
3. entry points と framework entry を推定
4. Python import graph を構築
5. dependency graph と照合
6. unused dependencies / missing dependencies / unused files / unused exports を表示
```

出力例。

```text
chokkin 0.2.0

Project: acme-api
Config : pyproject.toml
Mode   : app, production=false

Unused files  2
  src/acme/legacy.py       src/acme/legacy.py     file `src/acme/legacy.py` is not reachable from any entry root
  src/acme/old_handlers.py src/acme/old_handlers.py file `src/acme/old_handlers.py` is not reachable from any entry root

Unused dependencies  3
  boto3                    pyproject.toml:8       boto3 is declared in project.dependencies[1], but no reachable import, config, or binary usage was found
  python-dotenv            pyproject.toml:10      python-dotenv is declared in project.dependencies[3], but no reachable import, config, or binary usage was found
  rich                     pyproject.toml:9       rich is declared in project.dependencies[2], but no reachable import, config, or binary usage was found

Missing dependencies  1
  src/acme/config.py:3 yaml src/acme/config.py:3   imported pyyaml in src/acme/config.py:3 (no lockfile — transitive check skipped) but not declared in matching dependency context

Unused exports  2
  acme.auth:OldTokenBackend src/acme/auth.py:5     public class `OldTokenBackend` in `acme.auth` is not referenced from outside the module
  acme.utils:legacy_slugify src/acme/utils.py:5    public function `legacy_slugify` in `acme.utils` is not referenced from outside the module

Summary: 8 issues
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
--explain      : `CHK002:<distribution>` など selector に対し、宣言箇所と到達性
                 evidence（top-level modules、reachable/unreachable import）を出す。
--trace        : 到達可能な file には entry からの positive trace、未到達 file には
                 negative trace（未到達理由、entry roots、incoming import 連鎖）を出す。
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
|`CHK009`|`duplicate_dependency` |同じcontext内、またはruntimeとgroup/extra/buildに重複宣言されている|同じcontext内: warning / runtimeとgroup/extra/build: info|
|`CHK010`|`unresolved_import`    |first-party/third-party/stdlibのいずれにも解決できないimport  |`TYPE_CHECKING`・optional import・docs/dev context の file: info / それ以外: warning|

PEP 723 script (`# /// script` block を持つ `.py`) の CHK002 / CHK003 は project manifest ではなく script block の `dependencies` に対して判定し、subject を `script:<root 相対 path>:<distribution>` とする (例: `CHK003:script:scripts/tool.py:pyyaml`)。JSON では `path` に script、`distribution` に distribution 名を入れ、`target` / baseline fingerprint / `[tool.chokkin.ignore]` / `--explain` は同じ `script:` 形式を受け付ける。詳細は §10。

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
uv.lock / pylock.toml / pylock.<name>.toml / poetry.lock / pdm.lock
```

lockfileは1つだけ読み、優先順は uv.lock > pylock.toml > pylock.<name>.toml(名前順) > poetry.lock > pdm.lock とする。複数形式を合成しないのは、同一projectで形式が混在するのは移行途中であり、新しい側(uv/PEP 751)が実体に近いため。読んだlockfileの種類と相対pathは `ManifestSources.lockfile` に記録し、`--probe` に表示する。どの形式も package名と依存edgeだけを `LockfileGraph` に読み、同名packageのedgeは合流させる。pylock.tomlは `[[packages]].dependencies[].name`、poetry.lockは `[package.dependencies]` のkey(1.x / 2.x共通)、pdm.lockは `dependencies` のPEP 508文字列の先頭名を使う。uv.lockだけは `[package.optional-dependencies]` も extra名→配布物名 として `LockfileGraph.extras` に読む(上記のextra指定の判定に使う。依存側の `extra = [...]` はmanifestの宣言から分かるので読まない)。pdm.lockの `groups` / `extras` は読まない。CHK004はimportが宣言依存から到達可能かだけを問い、group別の到達性はmanifest側のcontextで判定済みのため。候補lockfileはすべてcacheの入力fingerprintに入れる(優先順位が変わると読む対象が変わるため)。lockfileは読むだけで編集しない。32 MiB を超える lockfile は読まず、読んだ結果が 20,000 package を超える lockfile も使わない(#972。corpus 最大は airflow の 3.2 MB / 960 package)。package を 1 件も載せない lockfile(textual の 52 byte の stub `uv.lock`、#976)も使わない。どれも `LockfileSkipped` warning を出して優先順で次の候補を読み、使える候補が残らなければ lockfile が無い場合と同じに扱う。そのとき CHK004 の判定は行わず、宣言の無い import は CHK003 になる。CHK003 と CHK004 はどちらも直接の宣言を求める finding なので、確信度は下げない。優先順を manifest の種別(`[tool.poetry]` なら poetry.lock)で変える案は採らない。uv へ移行途中の project では poetry の table が残っていても新しい側が実体だから。「宣言依存を 1 つも載せない lockfile」も skip しない。lock の選択が manifest の読み取り結果に依存し、読めなかった宣言があると実在の lock を捨てることになるため。複数の候補が並ぶときは `--probe` の lockfile 行に使わなかった候補を `not used:` として並べる。宣言依存から lockfile edge で到達する閉包は、宣言の組と lockfile の組ごとに 1 回だけ計算して使い回す。

manifest の PEP 508 requirement は pip と同じく `packaging` が受理するものを受理し、それ以外は invalid requirement の warning にする (`tests/pep508_packaging_golden.rs` で固定する)。先頭の名前を読める行は、その distribution の CHK003 / CHK004 を下げる gap になる (§11、#928)。`===` の値は空白・`;`・`)` までカンマごと読み、specifier の並びはその後カンマで分けて空の要素を捨てる (`foo===1.0,,>=1` は2つの specifier、`foo===1.0, >=1` は invalid)。保持する specifier は `, ` でつなぐが、`===` の直後だけは空白を入れない (`===1.0,>=1`)。fix で書き戻した行を再び読めるようにするため。例外として、末尾が `_` の名前・extra (`foo_`、`foo[te_]`) は `packaging` が受理しても invalid とする。PEP 508 は名前を英数字で終えることを求めており、uv も拒否し、PyPI にも存在しえないため (#572)。`foo_ @ <url>` も URL だけの行ではなく invalid として扱う。トークンを区切る空白は PEP 508 の `WS` と同じく space と tab だけで、`foo\u00a0>=1` や `foo>=1\u00a0,<2` は invalid とする。ただし演算子とバージョンの間は `packaging` の `\s*` に合わせて Unicode の空白も許す (`foo>=\u00a01`)。requirement 全体の前後の空白は pip・setuptools の `str.strip` と同じく Unicode の空白も除く (#879)。ここで言う Unicode の空白 (演算子とバージョンの間・`===` の値の終わり・全体の前後・requirements 行のコメント直前) は Python の `str.isspace` / `\s` と同じ集合で、Rust の `char::is_whitespace` に加えて U+001C–U+001F を含む (`foo>=\x1f1` は `>=1`) (#890)。`requires-python` は requirement の文法ではなく、pip と同じく `packaging` の `SpecifierSet` として読む。先にカンマで分けて、`str.strip` で空になる要素を捨てるので、`>=3.9,`・`>=3.9,,<4`・` , >=3.9` も `>=3.9` を下限として読む。分けてから各要素を読むので、`===1.0, >=1` も 2 つの specifier になる。要素が 1 つも残らない場合 (`,`) と、specifier でない要素がある場合は読めず、target version は既定の py311 に戻る (#880)。読めなかったときは、値を取った manifest（`pyproject.toml`・`setup.cfg`・`setup.py` のうち値を残したもの）を名指しして `manifest: could not read requires-python ...; assuming target version py311` を出す。`[tool.chokkin] target_version` を明示していれば推定を使わないので出さない。PEP 723 script block の `requires-python` が読めないときも同じく warning を出し、その script は project の target version で読む (#917)。root の `requires-python` が読めないときは、仮定した py311 で stdlib でないと決まった import だけを gap にする (#924)。同梱した minor (3.8〜3.15) のうち py311 から上限までの範囲には無く、範囲外の minor だけが持つ module (`binhex` など) が対象で、その import を CHK003 / CHK004 / CHK010 が言うかどうかは仮定した範囲で変わる。それ以外の import の判定は範囲に依らないので下げない。CHK003 / CHK010 を一律に下げるほどの不確かさではないため。範囲は解決と同じく py311 を下限、読めない `requires-python` の上限を上限にする。`target_version` を持つ script と、明示した `target_version` は何も仮定しないので gap にしない。member の `requires-python` は解決に使わないので、読めなくても gap にしない (warning の文言からも降格の説明を省く)。PEP 723 script の `requires-python` が読めないときは project の target version で読み、その範囲で stdlib でないと決まった import を、その script の file だけに効く gap にする (`AssumedScriptTargetVersion`、#936)。範囲は解決と同じく project の範囲で、対象と下げ方は root の場合と同じ。root と script の両方が読めないときは、どちらの gap も同じ import を下げる。どちらの gap も、その root を import した script の CHK003 (`script:` の欠落) を info / maybe にする。script の欠落も、どの module を stdlib とみなすかで変わるため。

requirements系filesのパース規則を定める。コメントはpip互換で「行頭または空白が先行する `#`」のみを除去し、URLフラグメント（`#sha256=` / `#egg=`）は保持する。`-r` / `--requirement`（`--requirement=other.txt` 含む）は再帰的に追跡する。`-c` / `--constraint` はversion制約の情報源としてのみ読み、`LoadedManifest.constraints` に積み、依存宣言とは合流させない（ファイル欠如はwarning）。`-r` / `-c` の path が root の外を指す場合（`..` で root を出る path・root 外への symlink）は、見つからない場合と区別して報告する。`-r` は `requirements file `…` resolves outside the project root` のエラー、`-c` は「root の外を指すので読まなかった」warningにする (#981)。root の外は走査しないので読まないが、not found と書くと path の誤りに見えるため。`-e ./path` とlocal path指定はworkspace/first-party候補として扱い、distribution名は空のopaque依存として記録する。`-e` の付かない行も、pip が directory や archive として install する形 (pip の `_looks_like_path` と `is_archive_file`: `.` で始まる、先頭の名前の直後が path 区切りか Windows の drive (`pkg/sub`、`/abs/pkg`、`C:\pkg`)、archive 名そのもの (`wheels/foo.whl`、`foo-1.0-py3-none-any.whl`、`foo.whl[tests]`。extras の `[...]` は pip の `_strip_extras` と同じく行末にあるときだけで、`foo.whl[tests]>=1` は `foo.whl` という名前の requirement)) なら同じ opaque 依存とし、warning は出さない (#963)。形だけで判定し、path の実在は見ない (pip は存在しない directory を error にするが、chokkin は解析対象を触らない)。pyproject / setup.cfg / setup.py の PEP 508 field と `-c` の constraints には path の形が無いので、そこでは invalid requirement のままにする。VCS URL・direct URL指定は `name @ url` 形式または `#egg=` からdistribution名を抽出し、抽出できない場合はopaque依存としてunused判定の対象外にする。`#egg=` の値 (fragment の最初の `egg=` から `&` まで) は pip 25 の `Link._project_name_re` と同じく PEP 508 の名前でなければ (extras の `[...]`・marker・空白を含む、空、名前の端が `-` `_` `.`)、pip がエラーにするので invalid requirement の warning にする。URL の行の marker は `"; "` でしか切らないので、`#egg=acme;python_version<"3"` の `;` 以降は egg の値に残る。path でない `-e` の行も同じ (#929)。opaque 依存は宣言として数えないが、archive の file 名から distribution 名を読める行 (path でも URL でも、wheel の PEP 427 の file 名 `bar-1.0-py3-none-any.whl`、sdist の `bar-1.0.tar.gz`) は、その名前の import の判定を下げる gap にする (§11、#988)。environment markerは保持し、§10の判定で使う。markerはpipの `parse_req_from_line` と同じく、行を最初の `;` (先頭がpipの知るURL・VCSのschemeの行では `"; "`) で分けてから読む。両側を `str.strip` するので `;` の前後のUnicodeの空白は許し (`foo;\u00a0os_name == "nt"`)、空のmarker (`foo ;`) はmarkerなしとする。markerがPEP 508の文法に合わない行はpipもエラーにするので、invalid requirement の warning にする。pyproject.toml・setup.cfg・setup.py の依存の宣言は `packaging` の文法のまま読む (#889)。行はpipの `join_lines` と同じく、`\` で終わる行を次の行とつないでから読み、行番号は最初の物理行とする。`\` は `str.strip("\\")` と同じく両端から除く。空白の後が `#` の行(コメント行)は `\` で終わっても継続せず、継続中に来れば空白を挟んでつなぐ。コメントはつないだ後に除くので、継続した行の途中の `#` は後ろの物理行も消す。requirement の行は `break_args_options` と同じく space だけで区切り、`-` で始まる最初のトークンから後をオプションとして切り離す (`--hash`・`--config-settings` など。tab の後のオプションは切り離さないので invalid になる)。オプションは `shlex.split` と `optparse` の規則で、pipが requirements ファイルで受け付けるもの (`SUPPORTED_OPTIONS` と `SUPPORTED_OPTIONS_REQ`、long 名の一意な略記を含む) だけを受理し、後ろの位置引数はpipと同じく無視する。未知のオプション・曖昧な略記・値の欠落や余計な値・`sha256` / `sha384` / `sha512` 以外の `--hash`・`KEY=VAL` でない `--config-settings`・閉じない引用符はpipもエラーにするので、行全体を invalid requirement の warning にする (先頭の名前を読めれば §11 の gap になる)。`pip-compile --generate-hashes` の出力を宣言として読むため (#927)。`-r` / `-c` / `-e` の行も同じオプションの規則で読む。pip の `ParsedLine` と同じく、フラグが取る値は 1 つだけで (`-r base.txt --pre` は `base.txt`、`-r "my reqs.txt"` は引用符を外した `my reqs.txt`)、同じフラグが繰り返されれば最初の値を使い、`-e` があれば同じ行の `-r` / `-c` は無視し、`-e` が無ければ `-r` を `-c` より優先する。`-e` の短縮形は pip と同じく `-e` の直後をそのまま値にし (`-example-pkg` は `xample-pkg` の opaque 依存)、値が path か URL かでは判定しない。これらの行がオプションの規則に合わなければ (`-r` だけ、`-r base.txt --unknown`、`-c pins.txt --hash=md5:x`) requirement の行と同じく行全体を invalid requirement の warning にして、残りの行は読む。規則に合う `-r` の先が無いときに読むのを止めるのは変えない (#964)。

`-r` で読んだ依存の context は include パスから推定する。ディレクトリ名と、ファイル stem を `-` / `_` / `.` で区切った語の**完全一致**だけを見る（`dev` → dev、`docs` / `doc` / `documentation` → docs、`tests` / `test` / `testing` → tests、`optional` / `optionals` → optional extra `optional`。`requirements-packaging.txt` / `requirements-github-actions.txt` は dev）。optional requirements は extra と同じく必要なときに入れる runtime の機能なので、dev ではなく extra とする (#786)。どれにも当たらなければ include 元の context を引き継ぐ。部分文字列では判定しないので、`protests.txt` や `documentsdb.txt` は runtime のまま (#505)。include パスの `..` は字句的に畳み、`docs/../dev-requirements.txt` は直接読んだときと同じ `dev-requirements.txt` を origin にする。

root の固定名(上の一覧と `requirements-docs.txt` / `requirements-tests.txt` / `requirements-test.txt`)以外の requirements ファイル、つまり root 直下の `*requirements*.txt` / `requirements*.in` と `requirements/`・`tests/requirements/` 配下の `*.txt` / `*.in`(werkzeug `requirements/tests.in`、urllib3 `emscripten-requirements.txt`、django `tests/requirements/py3.txt` など、#749)も同じ parser で読む(`-r` も辿る)。読んだ名前はすべて正規化した distribution 名として `ManifestSources.extra_requirements` に積む。さらに、書かれているファイル自身のパスが上の規則で context を示す要素(`requirements/tests.in`、`docs/requirements.txt`、`requirements-optional.txt`)だけは、その context の依存宣言として合流させる。test / docs / dev / optional のファイルは PEP 735 の group と同じく単独で install されるので、ファイルの相対パスを group 名(optional は extra 名)にし(`group:requirements/tests.in`)、別ファイルや `[dependency-groups]` の同名 group と CHK009 の重複にしない。読み済みのファイルを別の候補が `-r` しても二度は宣言しない。context を示さないファイルの要素は、dev のファイルから `-r` されていても名前だけにとどめる(mlflow `extra-ml-requirements.txt` は利用者が入れる framework の一覧で、dev 宣言ではない)。同じディレクトリに同名の `.in` がある `.txt` は pip-compile の出力で、推移依存まで pin した環境なので宣言にせず、`.in` だけを宣言とする (#786)。`extra_requirements` の名前は §7 の import 解決で、ほかのどの段でも解決できなかった import root を third-party とみなす根拠にだけ使い(完全一致または §7 step 8 と同じ接頭辞・接尾辞の一致)、CHK010 を出さない。この名前に distribution は付けないので、宣言にならなかった要素は CHK001–CHK005 の判定に使われない。これらのファイルは best effort で読み、壊れていても error にはしないが、root の requirements ファイルと同じく読めなかった行は warning に出す。`-r` 先が無いなどで途中で止まったファイルは `` manifest: stopped reading `path`: … `` を出し、止まる前に読めた要素は残す(#888)。候補ファイルは manifest cache の key に入れる(#679)。

`setup.py` は `ruff_python_parser` でASTにし、`src/manifest/setup_py_eval/` の小さな評価器で `setup()` 呼び出しの `name` / `version` / `python_requires` / `install_requires` / `extras_require` を静的に読む(構文エラー時は静的解析不可として扱う)。評価器はコードを実行せず、文を上から順に辿って文字列・list/tuple/set/dict リテラル、変数代入(`+=`・subscript 代入・unpack を含む)、`+` 連結、`*`/`**` 展開、条件なし内包表記、`return` 1 文だけのユーザー関数のインライン展開(位置引数だけの呼び出しに限る)、名前で引く依存表(`deps["torch"]` 形式)を扱う。依存表は whitelist 外の既知のイディオムとして `setup_py_eval/idioms.rs` に分け、subscript が不明値になる直前の 1 か所からだけ試す(#554)。依存表は version pin の一覧で宣言ではないので、`deps[...]` で引かれたリストの文字列要素は `setup()` で使われていなくても distribution 名だけを `extra_requirements` に積む(transformers `_deps` の `peft`、#749)。`if` / `try` は各分岐を分岐前の環境から評価して合流させ(同じ値はそのまま、list は和集合、dict はキーごと、それ以外は不明値)、`with` は本体を辿る。list と dict は Python と同じく参照を共有し、`b = a` の後の `a += [...]`・`a['k'] = ...`・`del a['k']` は `b` にも、`a` を値に持つ dict にも届く(`list(a)` などのコピーと tuple は共有しない。#565)。ループ本体は辿らない。本体で `+=` や subscript 代入を受ける変数と、`x.append(...)` のように method 呼び出しを受けた変数は、その別名も含めて不明値にする。本体で代入し直される変数はそれ自身だけを不明値にして別名との共有を切る(本体が一度も走らず、ループ後にその変数を通した変更が別名に届く場合は取りこぼす近似)。分岐ごとに別の object を指す変数は、合流後に新しい object として扱い、どちらの分岐の別名を通した変更も届かない(どちらかの分岐の値を上書きしないため。両分岐で同じ object を指す変数どうしは共有を保つ)。ただし list は不明な要素を足し、dict は「読めない entry もある」(partial)と印を付けて、どちらも読めた要素を残す(#869)。subscript 代入と subscript の `del` は、代入先が list で添字が int リテラル (`reqs[0]`、`reqs[-1]`) のとき、CPython と同じ数え方で指す要素を置き換える・取り除く。ただし添字が範囲内で、数える範囲 (正の添字なら先頭からその要素まで、負の添字ならその要素から末尾まで) が読めた要素だけの場合に限る。長さの分からない展開 (`*more`) をまたぐと位置がずれうるため (#868)。また list リテラルで作られ、その後も辿れる変更 (int リテラル添字の代入・`del`、読めた list リテラルの `+=`) だけを受けた list に限る。method 呼び出し (`reverse()`・`sort()`・`insert()` など) やループ本体で変わりうる list、中身の違う分岐を合流させた list、コピー (`list(x)`・`sorted(x)`・`x + y`・内包表記) は要素を残したまま順番を失うので、添字はどの要素も指しうる。それ以外の添字 (名前・slice・`True`・範囲外) や、dict でも list でもない代入先 (`reqs[0] = ...` の `reqs` が不明値) は、代入先を同じく不明値にする (list は不明な要素を足して partial にし、読めた要素は残す。どの要素を置き換えても代入した値は list にあるので、slice 代入でなければ値も要素として足す)。dict への key が文字列に評価できない代入(`extras[key] = ...`)は、dict リテラルの文字列でない key・読めない `**` 展開・内包表記の文字列でない key と同じく、既知の entry を残して dict を partial にする。足した entry が既知の宣言を消すことはなく、dict を不明値にすると既知の extras をすべて失うため(#827)。partial の dict で見つからない key は不明値として依存表を試す。`del` は entry を消しうるので、key が文字列に評価できないか dict でなければ代入先を不明値にする。評価は step 数と値の大きさに上限を設け、超えた式は不明値にする。`setup(**kwargs)` / `setup(*args)` のように引数が展開されていて `install_requires` が見えない場合は runtime 依存を不明として扱う。評価できない呼び出しの引数が文字列だけで `.txt` / `.in` を指す場合は requirements ファイル参照とみなし、root 直下と `requirements/` 配下を root 内に限って探して `extract_requirements_path` で読む(`..` や絶対パスは拒否。読めたファイルと見つからないファイルはキャッシュ fingerprint に加える)。setup.py(と setup.cfg の `file:`)が読んだ requirements ファイルは、root の requirements ファイルとして重ねて読まない(重複宣言の誤検出を防ぐ)。文字列の `install_requires` / `extras_require` の値と、list の文字列要素は setuptools の `_reqs.parse_strings` と同じ順で読む (#965): 行に分けて空行と `#` で始まる行を捨て、` #` 以降を落とし (空白の前置きが無い `#` は pip と違って落とさないので `rich#x` は invalid のまま)、`\` で終わる行は `join_continuation` と同じく `\` とその直前の 1 文字を落とし、残りを `str.strip` して次の行と空白を挟まずにつなぐ (`requests \` + `>=2` は `requests>=2`、`requests\` + `>=2` は `request>=2`。list の要素をまたいでもつなぐが、評価できない要素と requirements ファイル参照の前後はつながない)。最後の行が `\` で終わって続きが無い要素を setuptools は黙って捨てるが、chokkin は書かれたまま残して invalid requirement の warning (と #928 の gap) にする (ADR 0005)。ただし URL が `\` まで取り込む形 (`pkg @ https://h/x.zip\`) は宣言として残る。リストに評価できない要素が混ざる場合や参照ファイルが見つからない場合は `SetupPyPartiallyStatic` warningを出す。extras は `ManifestSources.extras` に読めた度合いを残す(ADR 0005 移行 step 2)。要素を読み切れなかった extra は名前ごとに Partial とし、`extras_require` が dict に評価できない場合、partial の dict の場合(読めた extra は宣言として残す。#869)と、`extras_require` を明示せずに `setup(**kwargs)` で引数を展開している場合は、どの extra があるかも分からないので Unknown にして `SetupPyPartiallyStatic`(argument `extras_require`)を出す。`extras_require` を明示した呼び出しでは、同じ keyword を 2 度渡すと `TypeError` になるので `**kwargs` は extras を変えない。`setup()` 呼び出しが見つからない場合と setup.py を decode できない場合も Unknown にする。setup.cfg を decode できない場合も Unknown にする。pyproject.toml の `dynamic` が `optional-dependencies` を挙げる場合(#905)、uv-dynamic-versioning hook の `optional-dependencies` 表は文字列の配列でない extra を、`[tool.setuptools.dynamic.optional-dependencies]` 表は(`file =` を読まないので)すべての extra を名前ごとに Partial にする。どちらの表もなければ、setuptools は setup.cfg `[options.extras_require]` か setup.py の `extras_require`(または `setup(**kwargs)`)で埋めるので、それらがあれば Known、なければ Unknown にする。Partial と Unknown は `DynamicExtrasUnread` warning(Unknown は extra 名を空にする)で報告する。source ごとの状態は Unknown を優先し、Partial は名前の和集合にして合わせる。Partial と Unknown は gap (UnknownExtras、§11) になり、その manifest の範囲の CHK003 / CHK004 / CHK005 を likely 以下に下げる (#975)。読めない extra は任意の import を optional として宣言しうるので、欠落と置き場所の誤りを言い切れない。runtime 依存は読めているので、RuntimeDependenciesUnknown のように info にはしない。Partial も Unknown と同じに下げるのは、読めなかった要素が何を名指しするか分からないため (ADR 0005 §6 は読めた要素からの絞り込みを採らない)。pyproject.toml の `[project]` に `name` / `version` が無いときは `[tool.poetry]` の `name` / `version` を使う(#974。Poetry 1.x は `[project]` を書かず、textual は name が無いため app mode になり公開 package の 167 file を CHK001 にしていた)。`[tool.poetry]` の他のキーは metadata に読まない(`python` は依存の制約で `requires-python` ではない)。複数manifest sourceのmetadataは pyproject.toml > setup.cfg > setup.py の優先順位でマージし、衝突時は `MetadataConflict` warningを出して上位を保持する。`requires-python` も同じで、`[project] requires-python` が無ければ setup.cfg の `[options] python_requires`、setup.py の `python_requires`(文字列に評価できるときだけ)を使う。ただし `[project]` テーブルがあって `dynamic` に `requires-python` が無いときは、setuptools と同じく setup.* の値を使わない(#764)。`[tool.uv.workspace]` membersはStep 2で読み込んだ `UvWorkspaceHint` を `LoadedManifest.uv_workspace` にコピーし、キャッシュhash入力に使う。

`setup.cfg` は `configparser` と同じ規則で section に分け、`[metadata]` の `name` / `version`、`[options]` の `python_requires` / `install_requires`、`[options.extras_require]` を読む。option 行は最初の `=` か `:` で key と値に分け(`install_requires:` も setuptools は受理する)、字下げした行は直前の option の値に続ける。`configparser` が拒否する行(区切りの無い行、option の前にある字下げ行)は読まずに捨てる。setuptools もそのファイルを読めず build できないので、そこに宣言は隠れていない。`file:` でない `install_requires` と各 extra の値は、setup.py の文字列と同じ `parse_strings` の規則で読む (` #` のコメント、`\` の行継続。#965)。宣言の label の index は、つないだ要素の最初の行が値の何番目の行かにする。`--fix` は物理行を数えてその行の名前を確かめ、`\` で終わる行は要素の途中 (URL の直後の `\` は名前まで読めてしまう) なので消さない。継続した要素の削除は no-op になる。1 行の値を `;` で区切る setuptools の `_parse_list` は読まず、1 行全体を 1 要素とする。`install_requires` と各 extra の値が `file:` で始まる場合は、setuptools と同じくカンマで区切った各 path を root 相対の requirements ファイルとして `extract_requirements_path` で読み、その option の宣言にする(setuptools が拒否する `-r` も辿るので、読む宣言は setuptools 以上になる)。読めたファイルと見つからないファイルはキャッシュ fingerprint に加え、読んだファイルは setup.py の参照と同じく root の requirements ファイルとして重ねて読まない。ファイルが見つからない場合、root の外を指す場合(setuptools と同じく `..` を字面で畳んだうえで root を出る path・絶対パス・root 外への symlink)、parse が途中で止まった場合は `RequirementsUnreadable` warning を出し、その option を読めなかったものとして扱う。`install_requires` なら setup.cfg の runtime 依存を不明にし(下記の `RuntimeDependenciesUnknown`。他の source に runtime 依存の宣言があれば立てない)、extra ならその extra を Partial にする。setuptools は見つからないファイルを warning だけで飛ばすが、build の前に生成されるファイルもあるので、空とはみなさない。

Poetry の依存表は、値の table のうち install されるかどうかを決めるキーも読む(#888)。`[tool.poetry.dependencies]` の `optional = true`(配列ならすべての要素が optional)は extra 経由でしか入らないので、PEP 621 の optional-dependencies と同じく `OptionalExtra` にし、extra 名は `[tool.poetry.extras]` でその依存を挙げる最初の extra(名前は正規化して比べる)、どの extra にも無ければ正規化した依存名とする。group の `optional` は group ごと入れないかを決めるキーで依存の条件ではないので、group の依存は従来どおり `Group` にする。`markers` は記述どおり、`platform` は `sys_platform == "<値>"`(`*` は条件なし、`linux || darwin` のような制約は `platform <制約>` という PEP 508 ではない marker)、`python` は PEP 508 の marker に直し、2 つ以上あれば各部分を括弧で囲んで ` and ` でつなぐ(`markers` の中の `or` が `and` より弱く結び付くため)。`python` の制約は `||` と `|` を or、`,` と空白を and とし、`*` は制約なし(どこかの選択肢が `*` なら marker を付けない)、演算子なしの version は `==`、PEP 440 の演算子はそのまま、`^` と `~` は Poetry と同じ上限を付けた範囲(`^3.8` は `python_version >= "3.8" and python_version < "4"`、`~3.4.1` は `< "3.5"`)にする。変数は version が 3 要素以上なら `python_full_version`、それ以外は `python_version`。これ以外の形(`3.9-dev`、`^3.x`、上限が u64 を超える version)は `python <制約>` という PEP 508 ではない marker として残す。条件付きの宣言として CHK002 は対象外にし(§10 の marker 付き宣言と同じ。`[tool.poetry.dependencies]` の `python` も同じ規則で marker に直し、§10 の「対応する interpreter」の範囲に使う)、`--fix` は PEP 508 に書けないので CHK005 の移動を Ambiguous で skip する。複数制約の配列は、すべての要素が marker を持つときだけ各 marker を ` or ` でつなぎ、ひとつでも無条件の要素があれば marker を付けない。そのほかのキー(`source`、`develop`、`allow-prereleases`、`git` / `branch` / `rev`、`path`、`url`)は取得元を決めるだけで、install されるかどうかは変えないので読まない。ただし PEP 508 の requirement には書けないので、`version` / `extras` / `markers` / `platform` / `python` / `optional` 以外のキー(知らないキーも含む。`extras` は名前に読む単一の table のときだけで、配列の形では読まないので含む)をどれかの形に持つ宣言は、名前だけにすると default index から解決されることになる。`--fix` はその CHK005 の移動を Ambiguous で skip し、理由には `git` / `path` / `url` / `source` を優先してキーをひとつ挙げる(#1041)。

`[tool.uv]` のその他のキーは次のように読む（R-04）。legacy `dev-dependencies` は `[dependency-groups] dev` と同じ `Group("dev")` 宣言として合流させ、`--fix` は `tool.uv.dev-dependencies[i]` ラベルで配列要素を削除できる。`constraint-dependencies` / `override-dependencies` は `-c` と同様に `LoadedManifest.constraints` へ積み、origin ラベル（`tool.uv.constraint-dependencies[i]` 等）を保持するが、CHK002/CHK003/CHK009 の入力にはしない。`build-constraint-dependencies` は正規化した名前だけを `LoadedManifest.uv.build_constraints` に保持し、宣言にも constraint にも入れない。dependency group のうち `[build-system].requires` の distribution を含むもの（openai-python の `build` group は `hatchling` を含む）は build 環境を hash 付きで export するための pin とみなし、その要素のうち `build-constraint-dependencies` にもある宣言は CHK002 の対象外にする。build backend の依存（`pathspec` / `pluggy` / `trove-classifiers`）はどこからも import されないため（#819）。2 つの条件の片方だけでは判定しない。constraint だけで判定すると、build 環境と無関係な group に同じ名前の未使用宣言があっても見逃す。group だけで判定すると、`hatchling` を置いた test group（virtualenv）の要素をすべて見逃す。除外は宣言単位で行い、distribution を used にはしないので、ほかの group の同名の宣言は従来どおり CHK002 になる。`default-groups` は読まない（`--production` は runtime 以外の context を常に除外するため、インストール既定値に依存させない）。`[tool.uv.sources]` は distribution 名ごとに path / workspace / その他（git / url / index 等）の種別と、path source の記述どおりの相対pathを `LoadedManifest.uv.sources` に保持する（絶対pathはキャッシュに入れない）。path source（`editable = true` を含む）はStep 7でroot基準に解決したローカルtreeからlayout推論でimport名を導出し、そのdistributionへのCertainな対応としてvenvなしで解決する（キャッシュ外で毎回読むためstaleにならない。package未検出時はdistribution名の `-`→`_` をimport名とする。root外を指す path（絶対pathや `..`）は §5 の warning を出すだけで、そこではlayout推論をせず同じくdistribution名を使う。root外のtreeは読まないため、#1042）。`workspace = true` source は既存のworkspace member判定でfirst-partyとして解決し、到達可能なimport（third-party と同じく、pytest の `-p` など config file が名指す参照を含む）か、manifest の entry point、config や pytest plugin の命名規則 (#809) による使用があればそのdependencyをusedとして扱う。used になったmemberのtree内ファイルのimport（stdlib以外。`airflow` のようにdistributionへ解決されるものも含む）も同様に数え、他memberのtreeに届けばそのmemberもusedとする（`airflow-core` が `airflow.sdk` をimportすれば `apache-airflow-task-sdk` もused）。root は到達可能な file の import だけを数えるのに対し、used な member は tree 内の到達できない file（member の tests など）の import も数える。member の manifest はその tree 全体の依存を宣言し、root はその member を通して tree 全体を配布するため。この判定は最小不動点で、member の宣言順に依らず、証拠を足しても CHK002 は増えない（`tests/prop_workspace_used.rs`、#1109）。src layout の root のように root の inventory が member のファイルを含まない場合は、root の parse と同じ呼び出し（同じ parse cache bundle）で member inventory のうち root にないファイルも parse し、その絶対importをこの判定にだけ使う。他のruleの入力には入れない。memberのtreeが持つmodule名は、member直下とその下の各 `src/` を起点に数える（airflow の `dev` member の `dev/breeze/src/airflow_breeze` は `airflow_breeze`）。§7 の namespace 上書きより起点が広いのは、ここで module を見逃すと使っている member を CHK002 にし、広く取りすぎても member を used にするだけだから。import名（`from a import b` は `a.b`）がmemberのtreeにない場合は、親 `a` をそのmemberのファイル（`a.py` / `a/__init__.py`）が持つときに限りtreeに届いたとみなす（`from acme_util import y`）。namespace package の親（providerの `airflow`）は複数memberが持つので数えない（#712）。

`setup.py` が静的に解析できない場合(動的な `install_requires` 構築など)は、warningを出してそのsourceをskipし、他のsourceで解析を継続する。`install_requires` が読めず、pyproject.toml / setup.cfg / root の requirements ファイルにも runtime 依存がない場合、または `dynamic = ["dependencies"]` で runtime 依存が 1 つも得られない場合は、`ManifestSources.runtime_dependencies_unknown` を立てて `RuntimeDependenciesUnknown` warningを出す。これは ADR 0005 の gap になり、emit の降格段 (§11) が import を対象にした CHK003 / CHK004 を `info` / `maybe` に下げる(既定の confidence filter では表示されない)。`[project]` の `dynamic = ["dependencies"]` が指定されている場合は、setuptoolsの慣習に従い `requirements*.txt` 側を依存宣言の実体として読む。`dynamic` が `dependencies` / `optional-dependencies` を含む場合は、hatch の metadata hook `[tool.hatch.metadata.hooks.uv-dynamic-versioning]` の `dependencies` / `optional-dependencies` を runtime / extra の宣言として読む。hook は配列中の `{{ version }}` などの template を描画するだけなので、version 位置の template は仮の version に置き換えて parse し specifier を捨て、distribution 名に template が掛かる要素は invalid requirement の warning にする (#590)。他の metadata hook は読まない。

`setup.py` / `setup.cfg` / requirements ファイルは §6 の parser と同じ `decode_python_source` で読み、UTF-8 でなければ先頭 2 行の PEP 263 宣言(latin-1 / cp1252)で decode する。decode できないファイルは `FileUndecodable` warning を出して skip し、解析を止めない(#552)。skip したのが `setup.py`、`setup.cfg`、または runtime context の requirements ファイルで、他の source に runtime 依存の宣言がない場合は、上記の `RuntimeDependenciesUnknown` と同じ扱いにする(`setup.py` の `install_requires` が完全に読めていれば skip した `setup.cfg` は補われる)。pyproject.toml と lockfile は TOML 仕様どおり UTF-8 のみ読む。

`[dependency-groups]` は、build metadataには含めない開発用途の依存を `pyproject.toml` に格納する標準仕様。lint/test/docs用の依存を扱うため、`chokkin` ではmain dependencyとは別contextとして扱う。

PEP 735 の `{include-group = "..."}` は、group名を PEP 503 と同じ規則で正規化して解決し、推移閉包として展開する。要件は include 先へ複製せず宣言元 group に 1 件だけ置き、`DeclaredDependency.included_via` には include 元の group ごとに、宣言元までの最短経路で次に通る group を 1 つだけ持たせる (例: `all` → `dev`、`dev` → `test`)。経路上の各 group も同じ要件の include 元なので、経路 (`["all", "dev", "test"]`) はこの next hop を辿って復元できる。経路を丸ごと持つと、直列に include し合う n group で O(n³) になるため (#1046)。最短経路が複数あるときに復元される経路は、include 元の BFS が最初に見つけた next hop で決まる。§10 の context 判定・CHK005 は宣言元 group と各経路の先頭 group の bucket をすべて有効な宣言先として扱い、CHK002 は有効な bucket がすべて dev のときだけ非 strict で抑制する。CHK009 は宣言元の context だけで判定するので、include だけで重複扱いにはならない。`--fix` は宣言元 group の label (`dependency-groups.<group>[i]`、index は include table も数える) だけを編集し、include 要素は書き換えない。`--explain` には `included via dependency-groups: server -> shared` の行で経路を出す。未定義 group への include (`DependencyGroupIncludeUndefined`) と循環 (`DependencyGroupIncludeCycle`) は manifest warning にして、解析は止めずに循環を切って続ける。

Python packagingのentry pointsは、distributionが提供するcomponentを他のコードやinstallerに知らせる仕組みで、`console_scripts` はインストール時にCLI wrapperを作るために使われる。`chokkin` は `[project.scripts]`、`[project.gui-scripts]`、`[project.entry-points]` をentry rootとして扱う。同じ宣言を Poetry の `[tool.poetry.scripts]` (`name = "pkg.cli:main"` と `{ reference = "...", type = "console" }` の table。旧形式の `callable` も読む) と `[tool.poetry.plugins."group"]`、`setup.cfg` の `[options.entry_points]` (setuptools と同じく、値に改行があれば行ごと、無ければ `,` で区切る)、`setup.py` の `setup(entry_points=...)` (静的に読める dict で、group ごとに行を並べた文字列か文字列の list。または `[group]` 見出しを持つ 1 つの文字列) からも読む (#916)。どの形でも setuptools の group 名 `console_scripts` / `gui_scripts` は `[project.scripts]` / `[project.gui-scripts]` と同じ console / gui として扱い、§8 の mode 判定にも効く。どの形にも当たらない宣言 (文字列でも table でもない値、`type = "file"` の Poetry script、`[options] entry_points = file: ...`、`name = target` の形でない行、静的に読めない `entry_points`、一部の group を読めない dict) は root にせず、`` manifest: skipped entry point `tool.poetry.scripts.x` in `pyproject.toml`: <理由> `` を warning に出す。読めない宣言が CHK001 / CHK006 を下げるかは #888 で扱うので、今は warning だけで gap にはしない。`setup(**kwargs)` が隠しうる `entry_points` は名指しされないので warning も出さない。複数の manifest が同じ entry を宣言しても root が重なるだけなので、すべて読む。

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
  ".chokkin/**",
  "build/**",
  "dist/**",
  "**/__pycache__/**",
]
# 解析・到達性の追跡はするが issue は出さない vendored code (#490)
vendored = [
  "**/_vendor/**",
  "**/vendored/**",
  "**/externals/**",
  "**/third_party/**",
]
# 組み込みの loader API に加えて、dotted name の文字列の module を import する関数 (#493)
module_loaders = ["LazyFeature"]

[tool.chokkin.dependencies]
dev_groups = ["dev", "test", "tests", "lint", "docs"]
runtime_groups = ["server", "worker"]
type_groups = ["types", "typing", "mypy"]

[tool.chokkin.package_module_map]
"PyYAML" = ["yaml"]
"Pillow" = ["PIL"]
"python-dotenv" = ["dotenv"]
"scikit-learn" = ["sklearn"]
# namespace package の配下は dotted name で指定できる(最長一致)
"protobuf" = ["google.protobuf"]

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

# ルール別 severity override (off / info / warning / error)
[tool.chokkin.severity]
CHK001 = "off"
CHK006 = "info"
CHK002 = "error"
```

`entry` / `project` / `exclude` / `vendored` と `workspaces.<id>` の `path` / `entry` / `project` は root 相対で書く。絶対パスに加え、`..` を字面で畳んで root の外に出る path（`../shared/**`）も設定の検証エラー（exit 2）にする (#981)。root の外は走査しないので、黙って何にも一致しない設定になるのを防ぐ。

`vendored` は `exclude` と違い走査から外さない。vendored code 経由の import や symbol 参照は到達性と参照元に数え、そこで見つかった issue だけを報告しない（`SuppressReason::Vendored`。baseline 件数には含めない）。layer をまたいでは置き換えで、`vendored = []` で無効にできる。

`module_loaders` は §11 の loader API に足す関数の名前で、呼び出しの関数名 (`x.LazyFeature(...)` なら属性名) と一致させる。identifier でない名前 (`pkg.LazyFeature`) はどの呼び出しにも一致しないので、設定の検証エラー（exit 2）にする。既定は空で、layer をまたいでは置き換える。

`respect_gitignore = true` のとき、走査が出会った `.gitignore` を読めなかった場合（root でも配下の directory でも、UTF-8 でない・読み取り権限がない・リンク先のない symlink）は、そのファイルごとに `sources: could not read <path>; its ignore rules may be missing` を 1 回出す。workspace member の走査が同じファイルに出会っても重ねて出さない (#915)。読めなかった ignore 規則は除外を減らす方向にしか働かず、finding を誤って消さないので gap にはしない。

ソース走査は symlink を辿らない（root 自体が symlink の場合だけは、その先を root として走査する）。root 内を指す symlink は、実体を実体の path で 1 回だけ数えるので無言で飛ばす（同じファイルを 2 つの module 名で数えない）。root の外を指すもの・ループするもの・リンク先がないものは、ユーザーが解析させるつもりで置いた可能性があるので、リンクごとに `sources: did not follow symlink <path>: <reason>` を出す（reason は `it points outside the project root` / `it forms a loop` / `it points to nothing`。それ以外の読めなかった理由は OS の文言をそのまま出す）。対象は、ソースの拡張子（`.py` / `.pyi` / `.ipynb`）を持つ名前、ディレクトリを指すもの、リンク先が分からず拡張子も先頭の `.` もない名前に限る。ほかの名前（`LICENSE.txt`・`.gitignore` など）はもとから走査しないので、辿っても読まない。exclude と `.gitignore` で除かれる path は出さない。workspace member の走査が出したものは root の走査が同じリンクを見るので落とす（member 間のリンクは root 内なので出さない）。多いときは他の warning と同じく `… and N more like the above` にまとめる (#986)。root の外を辿らないのは解析対象の外を読まないための既定で、変えない。root の外のコードは project の外なので、その import を依存の使用に数えないのが既定どおりで、gap にはしない（ループとリンク先のないものには読む中身がない）。

ルートの `.chokkin/` は解析データ専用とし、分析対象のソースは置かない。default / mandatory exclude の `.chokkin/**` を既存の walker filter_entry で枝刈りし、cache JSON 数に比例する探索を避ける。

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

uv workspaceも読む。uvのworkspaceは複数packageをまとめて管理する仕組みで、各memberが自分の `pyproject.toml` を持ち、workspace全体で単一lockfileを共有する。`tool.uv.workspace.members` がある場合は自動でworkspace modeに入り、確定rootから下方向に member glob と member `pyproject.toml` を解決する。`tool.uv.workspace.exclude` の glob に一致するディレクトリは、uv と同じく `members` に一致してもmemberにしない（#568）。root の `[tool.uv.sources]` の path source のうち、root配下にあり `[project]` を持つ `pyproject.toml` があるもの（streamlit の `lib/` など）も、distribution名をidとするmemberとして同様に扱う（#499）。marker付きの配列形式（`foo = [{ path = "lib", marker = "..." }, { index = "..." }]`）では `path` を持つ要素を候補とし、条件を満たす最初のpathをmemberにする（memberのidをdistribution名で一意に保つため。#508）。root外のpath source（`../shared` など）は、root外のファイルをスキャンしないためmemberにしても照合するimportが無く、対象外とする。root内でも、directory が無い path source は `does not exist` warningで、`[project]` を持つ `pyproject.toml` が無い path source は member にしない理由を warningで知らせ、後者の下の finding を §11 の gap で下げる (#1000)。root の `[tool.uv.sources]` の root外のpath source は、読まなかったことを `resolves outside the project root and was not read` warningで知らせる (#981)。member の sources は対象外とする（monorepo の member が隣の member を `../core` で指すのは普通で、root 内なら member として読まれる）。git / url sourceはローカルにtreeが無いため対象外とする。v0.2では解決済みmemberを `LoadedConfig.workspace_members` として保持し、member別の `LoadedManifest` / source inventory を `ProbeReport.workspace_inputs` に載せる。resolver は `workspace_members` を受け取り、cross-member import を first-party として扱い、各 `ResolvedImport` に import元の `workspace_member` を付与する。Step 10 は `--strict` 時に `ResolvedImport.workspace_member` と member 別 manifest を照合し、root で宣言済みでも import元 member が直接宣言していない third-party import を CHK003 として報告し、member内で宣言済みでもruntime使用に対してcontextが合わない場合は CHK005 として報告する。非 strict でも import元 member が使用contextに合う宣言を持てば CHK003 / CHK004 / CHK005 はいずれも出さない。CHK003 / CHK004 / CHK005 の振り分けは「import元 member に宣言があればそれを、なければ root の宣言を」見る共通 lookup で行い、1 importにつきどれか1つだけを出す（member が宣言していなくても root で dev-only なら CHK005）。Step 12以降のissue/reportersは `workspace_member` を保持し、human/GitHub系reporterでは subject に `member:` prefix を付け、JSONでは `workspace_member` fieldを出力する。以後の段階で各memberの依存・source・entryを別々に解析しつつ、root dependencyとの関係を判断する。

uv / chokkin のworkspace宣言がない場合（llama_indexのように数百の `pyproject.toml` を持つmonorepo、#488）は、Step 4のsource discovery後に `detect_nested_members` が確定rootから深さ4までの入れ子 `pyproject.toml` を探し、`[project].name` を持つものだけを `workspace_members` にする（`--no-auto-workspace` で無効）。root自身・tool設定だけの `pyproject.toml`・`exclude` globに一致するもの、隠しdirectoryと `venv` / `build` / `dist` / `node_modules` / `tests` / `fixtures` などの配下は対象外。member idはdirectory名で、重複したら相対pathにする。検出時は `AutoWorkspace` warningで件数を出す。以後は宣言されたworkspaceと同じく member別manifestで依存を判定する。

member manifest の warning も root のものと同じく出す。file は root からの path に直し(`` manifest: invalid requirement at `packages/x/pyproject.toml` … ``)、file を持たない warning(Poetry / PDM / Hatch section の検出、見つからない `-c` constraints file)は `` manifest: `packages/x`: … `` と member を前に付ける。`--production` で外した dev 専用 member の warning は、その member を解析しないので出さない。member の `RuntimeDependenciesUnknown` はその member の gap になり、その member の finding だけを下げる (§11)。文言も root と同じく confidence を下げる旨を含め、下がる範囲として member を名指しする(`` …; missing-dependency findings in `packages/x` are reported as low-confidence info ``、#910)。member の source discovery の warning (`sources: …`) も root のものと同じ文面で、path を root からの path に直して出す(`` sources: no package directory matches project `x`; guessed `packages/x/other` ``、#892)。`LargeProject` だけは出さない。root の探索が member の tree も歩くので、root の件数がすでに member の file を含むため。読めないディレクトリは、walk の root ではなく読めなかったパスを `` sources: could not read `<path>`: <理由> `` と出す (#900)。root と member の walk が同じディレクトリで止まっても同じ文面になり、1 件にまとまる。パスを持たないエラーと、壊れた ignore file (文面に file 名を含む) は、今の通り walk の root を出す。

workspace member でも member の祖先でもない入れ子 `pyproject.toml` が `[project].name` を持つ場合（fastmcp の `examples/smart_home/` など）は、自前の依存を持つ独立 project として、そのdirectory配下のfileを source inventory から外す（#694）。root の manifest はそれらの依存を記述していないため。`setup.py` を持つdirectoryも同じく独立 project とする（click 8.1 の `examples/*/`、#1078）。`setup.py` は静的に読むだけで実行せず、module のどこか（`main()` の中や `if __name__ == "__main__":` の下を含む）で `setuptools` / `distutils` を import するか、任意の module から `setup` を import していれば（`from skbuild import setup`、別名の `as _setup` を含む）、`name` が動的でも、`setup.cfg` だけに書かれていても distribution とみなす。文字コードや構文が読めない `setup.py` も、pip がその file 名で directory を install するので独立 project とする（root に残すと、その下の import を root の宣言で判定して CHK003 を誤検出するため）。読めてそのような import の無い `setup.py`（`django.setup()` を呼ぶ script など）は同名の script とみなして root に残す。`setup.py` の無い `setup.cfg` だけの directory は pip が install しないので対象外。root の `[tool.uv.sources]` の path source で `[project]` の無いdirectoryそのものは、§11 の UnreadPathSource として root の code として解析するので外さない（#1000）。`__init__.py` を持つdirectory（package内に置かれた `pyproject.toml`）と、`tests` / `fixtures` などtest系directoryの配下（testがdataとして読むfixture project）は対象外。ただし PEP 723 の inline script block を持つ `.py` は自前の依存を宣言しているので外さず、`script:` の CHK002 / CHK003 で判定する（#714）。件数は `NestedProjectsSkipped` warning（`workspace: skipped N nested projects ... (--no-auto-workspace keeps them)`）で出し、`--no-auto-workspace` では外さない。`load_config` ではなくprobeで行うのは、member manifest読込みが `load_config` を再帰的に呼ぶため。

`chokkin --init` は、auto discoveryで検出したlayout・entry・dependency groupを反映した `[tool.chokkin]` の雛形を `pyproject.toml` に追記する。既存の `[tool.chokkin]` がある場合は上書きせずexit code 2で終了する。`pyproject.toml` が root 外への symlink の場合も、書かずに exit code 2 で終了する(§13 の containment、#980)。

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

実装（`src/graph/`）が edge として保持するのは `File imports Module` と `Distribution provides Module` だけで、entry/plugin/file からの到達は step 9 の BFS がその場で計算する（`ReachPredecessor` に最短経路を残す）。

step 9 は関数本体の import（`deferred`。文字列 prefix の動的 import も含む）と `TYPE_CHECKING` 配下の import を辿らない BFS ももう 1 回走らせ、その到達集合を `ReachabilityReport::eager` に置く。eager に入らない reachable file は、関数が呼ばれるまで読み込まれないか、実行時には読み込まれない (#610)。さらに optional な import（`try` / `with suppress(ImportError)` 配下、platform guard 配下）も辿らない BFS を走らせ、その到達集合を `ReachabilityReport::certain` に置く。この 2 つの BFS は test / docs / dev context の file の import を、library mode の file 以外へは辿らない。test から top-level で import されても、runtime で読み込まれる根拠にはならないため。library の module は利用者が直接 import しうるので辿る (#614)。

その間に `ReachabilityReport::exposed` を置く。certain と同じく optional な import は辿らないが、利用者が import したときにだけ読み込まれる起点からの到達もここに数える。library mode の公開サブパッケージの `__init__.py`（親ディレクトリにも package 内の runtime の `__init__.py` がある root で、auto rule `library subpackage`）と、見つかった module 自身を指す plugin の module reference（Flask の route / Celery の task decorator が出すもの）は、パッケージ自身が読み込む根拠ではないので exposed から始める。test / docs / dev context の file から library の file への import も、行き先がサブモジュール（package 内で親ディレクトリに runtime の `__init__.py` がある module）なら exposed までしか渡さない。package の最上位の `__init__.py` や module は import すれば必ず読み込まれるので、そのまま渡す。certain は、package の最上位の `__init__.py`（namespace package では最も浅い `__init__.py`）、manifest や設定の entry point、その他の plugin reference から optional でない import で辿れる file だけになる (#802)。CHK001 / CHK006 が使う reachable は変わらない。

処理順は固定する。

```text
1. root discovery          # 実装済み: src/discovery/ (`discover_project_root`)
2. config load             # 実装済み: src/config/ (`load_config`)
3. manifest extraction     # 実装済み: src/manifest/ (`extract_manifest`)
4. source file discovery     # 実装済み: src/sources/ (`discover_sources`)
5. config/plugin extraction  # 実装済み: src/plugins/ (`extract_plugin_hints_with_parse`)
6. Python parse              # 実装済み: src/parser/ (`parse_file`, `parse_project_sources_with_cache`, attribute access)
7. import resolution         # 実装済み: src/resolver/ (`resolve_imports`, bundled maps)
8. entry root construction    # 実装済み: src/entry/ (`build_entry_roots`)
9. reachability analysis     # 実装済み: src/reachability/ (`analyze_reachability`, `trace_to_file`)
10. dependency reconciliation # 実装済み: src/rules/deps/ (`reconcile_with_context`, CHK002–CHK009)
11. symbol usage analysis    # 実装済み: src/rules/symbols/ (`analyze_with_context`, CHK006–CHK007, CHK010)
12. issue emission
13. optional fix
```

Python parserはRust実装でよい。Ruff ecosystemのparserを使うか、RustPython parserを使うかはライセンス・保守性・Python新構文対応速度で選ぶ。ただし、Ruffのparser crate群をAstralが安定APIとして公開し続ける保証はないため、採用する場合はversion固定またはvendoring前提のリスクを織り込む。重要なのは、ASTだけではなくtoken位置・comments・string literalを保持すること。`# chokkin: ignore[...]`、`__all__`、`TYPE_CHECKING`、`importlib.import_module("...")`（代入・`return`・呼び出し引数など式中のネストも含む）、framework設定のstring literalを拾う必要がある。

現行は `ruff_python_parser` `=0.0.15` (MSRV 1.96)。Python 3.12 以降の構文 (PEP 695 / 750 / 758 / 810) に追従するため、ADR 0001 の Amendment 2026-09-25 と #320 / #321 の実測 (PoC は draft PR #349) を受けて、#351 で `rustpython-parser` 0.4 から移行した。`ruff_*` crate は同じ version に完全固定 (`=0.0.N`) して 1 PR で lockstep に更新する (dependabot の `ruff-parser` group)。parser の AST 型を扱うのは `src/parser/` と `setup.py` の literal 評価 (`src/manifest/literals.rs` / `setup_py.rs` / `setup_py_eval/`) に限り、`ParsedModule` 以降は backend に依存しない (結果: `docs/dev/issue-294-parser-migration-plan.md`)。行番号は byte offset から `src/parser/lines.rs` の `LineIndex` で求める。decorator 付きの def/class の symbol 行は CPython と同じく `def` / `class` の行にする。`type X = …` (PEP 695) は module level の変数と同じ symbol にし、alias の値と type parameter の bound / default も走査する。t-string (PEP 750) と `except A, B:` (PEP 758) は f-string / tuple の except と同じに走査する。`lazy import` (PEP 810) は通常の import と同じ edge にする。

source は byte で読み、UTF-8 として decode する（`src/parser/encoding.rs`）。UTF-8 でない file は PEP 263 の coding 宣言（先頭2行、2行目は1行目が comment か空行のときだけ）を見て、latin-1 と cp1252 は decode する。それ以外の encoding や宣言のない非UTF-8 file は解析を止めずに `ParsedModule::skipped` を立てて skip し、`parse: skipped …` の warning を stderr と JSON reporter の `diagnostics` に出す。中身を読めていないので、skip した file は CHK001 の候補にしない。到達可能な file を skip した場合は、その import が分からないため opaque dynamic import と同じく CHK001 の confidence を下げ、CHK002 も Likely にして `--fix` の対象から外す (#486)。この降格は ADR 0005 の gap として emit の降格段 (§11) で行う。どこからも到達しない file でも、library の公開 surface にあれば利用者が import しうるので、distribution の CHK002 だけを同じく Likely にする (§10、#550)。また PEP 723 の inline script も同じ decode を通して読む (#486)。構文エラーで parse できない file も同じく `skipped` にして、`parse: skipped …: syntax error at line N: …` の warning を出す。parser は error の手前までの AST を返さないので、中身の無い module として流すと「import が無い module」と区別できず、その file だけが使う依存が CHK002 certain になって `--fix` で消えてしまう。文・式・pattern の入れ子が 1,000 段を超える file も、構文エラーと同じく `skipped` にして `parse: skipped …: syntax error at line N: nested deeper than 1000 levels, which chokkin does not walk` を出す (#1045)。parser 自体は任意の深さを受け付けるが、AST を辿る visitor と AST の解放は段ごとに再帰するので、固定長の thread stack (Windows の main thread は 1 MiB で約 2,000 段) を超えると process ごと abort するため。CPython は 200 段の括弧で SyntaxError にし、演算子の入れ子も数千段で compile できなくなるので、実在の code はこの上限に届かない。深い AST は解放せずに leak させる。`setup.py` などを plugin や manifest が読むときも同じ上限で読めない file とし、深すぎる文字列 annotation は parse できない annotation と同じく名前を読まない。Flask / Celery の decorator 走査は、error diagnostic を見て行単位のテキスト走査に切り替える（後述）。JSON として読めない notebook や `cells` 配列の無い notebook も、同じ理由で `skipped` にして `` parse: skipped `path`: invalid notebook JSON: … `` を出す (#893)。code cell の `source` が文字列でも文字列の配列でもない cell (文字列でない要素を含む配列も、残りの行をつなぐと notebook に無いコードになるので cell ごと)、`cell_type` が文字列でない cell (code かもしれない) は読み飛ばし、他の cell は解析を続ける。読み飛ばしがあれば notebook ごとに `` parse: `path`: some code cells were not read: their `cell_type` or `source` is not text `` を 1 件出す (#898)。読み飛ばした cell は何を import するか分からないので、`ParsedModule::has_unread_cells` を立て、その notebook が到達可能なら ADR 0005 の gap として emit の降格段 (§11) が skip と同じ範囲を下げる。CHK001 の confidence を likely 以下にし、CHK002 も Likely にして `--fix` の対象から外す (#902)。読めた cell の import はそのまま使うので、CHK003 / CHK010 は下げない。code cell が 1 つも読めなかった notebook は `cells` が無いのと同じく分からないので、`skipped` にする。code cell の無い notebook は空として読む。notebook は全て entry なので、どれか 1 つが読めなければ CHK001 の confidence が下がり、CHK002 は Likely になる。

notebook の cell には IPython の構文 (magic、shell escape、help) が書ける (#962)。IPython は cell を compile する前にこれを Python に変換するので、chokkin も同じく 1 行を 1 行に置き換えて読む (`src/parser/ipython.rs`。行番号は notebook のまま)。cell をつないだ source がそのまま parse できればこの変換はしない。parse できないときは、IPython と同じく cell を 1 つずつ読み、そのままでは parse できない cell だけを変換する。落とすのは、どの rule も読まないものに限る。shell escape (`!cmd`)、help (`obj?` / `?obj`)、Python を実行せず module も import しない line magic (`%pip`・`%matplotlib`・`%env`・`%cd` など、`INERT_LINE_MAGICS`) と、本体が別の言語の cell magic (`%%bash`・`%%html`・`%%script bash` など、cell ごと) がこれにあたる。shell が何を実行するかは追わない (shell script を読まないのと同じ)。`%pip install` の引数を依存の宣言として読むかは別に決める (#991)。Python を実行する magic は、その Python を残す。`%time` / `%timeit` / `%prun` は option を除いた文に、`%%time` / `%%capture` / `%%python` / `%%script python3` (`--out` などの option は飛ばす) は magic の行だけを落として本体を読む。`%%timeit` / `%%prun` は magic の行の option を除いた文 (setup) も読む。代入の右辺にある escape (`x = !cmd`、`x = %time f()`) は代入先を `...` で残す (`x = ...; f()`)。変換するのは文の始まりの行だけで、括弧の中や複数行の文字列の中の行 (`(a\n % b)` の `% b` など) は IPython と同じく Python のまま読む (#994。ruff の lexer の IPython mode で括弧と文字列の範囲を取るので、shell command の中の括弧や引用符は数えない)。`\` で続く行は、IPython と同じく 1 行につないでから変換する。module を読み込む magic は import に置き換える。`%load_ext X` / `%reload_ext X` は `import X` (IPython 同梱の `autoreload` と `storemagic` は落とす)、`%aimport a, b` は `import a, b` にする (`-skip` などの option 付きは落とす)。これら以外の magic (`%run`、`%%writefile`、未知の magic、cell の先頭以外の `%%`。#992) は何をするか分からないので変換せず、変換しても parse できない cell と同じく読み飛ばす。読み飛ばした cell ごとに `` parse: `path:line`: code cell not read: … `` (magic なら名前、構文エラーならその内容) を出し、`has_unread_cells` を立てて上と同じ gap にする。読めた cell の import はそのまま使う。どの cell も読めなければ従来どおり notebook ごと `skipped` にする。

skip せずに残した file でも、parser が読めなかったものは `ParsedModule::diagnostics` に warning として積む。`__all__` の未対応の書き方、解決できない相対 import (file がどの package にも無い (layout の package にも `__init__.py` の chain にも)・親 package の無い top-level module にある・level が top-level package を超える、の 3 つを文面で分ける。どれも同じ文面だと layout 推定の失敗に見えるが、直し方が違うため。#982)、`target_version` に無い構文がこれにあたる。どれも `` parse: `path:line`: … `` (行を持たないものは `` parse: `path`: … ``) として stderr と JSON reporter の `diagnostics` に出す (#877)。1 件 1 行で出すのは、finding と同じく file と行から原因を探せるようにするため。ただし同じ文面の warning は 1 度だけ出す。共有された `-r` include や root も読む member の file は複数の段階から同じ warning になるが、読者が知りたいのは 1 件だけだから。同じ種類 (warning の variant が同じもの。parse の diagnostic は文面も同じもの) が 5 件を超えたら、stderr・markdown・github は最初の 5 件と `` <area>: … and M more like the above (`--reporter json` lists every warning) `` の 1 行にまとめる (#888)。django の `tests/` のように 1 つの原因が数千行になると、他の warning が埋もれて読まれなくなるため。JSON と SARIF は tool が読むので全件を残す。root の inventory に無い workspace member の file (#712) も root の file と同じく、skip と diagnostic を warning に出す。`--production` で CHK006 の根拠として別に読む library の tests (#588) も同じく出す (#894)。その tests は、root の walk が `--production` で inventory から外すときに取り置いたものを使い、tree をもう一度 walk しない (#844)。同じ walk なので、tests の下で読めなかったパスも root の sources warning としてすでに出ている (#899)。warning は finding の confidence を変えない。読めなかった範囲で finding を下げるのは ADR 0005 の gap の役目にする (§11 の降格段)。

## 7. import resolution仕様

Pythonの依存解析で最大の罠は、distribution名とimport名が一致しないこと。Python標準の `importlib.metadata` でも、distribution package名とtop-level import package名は必ずしも1:1対応しない。1つのdistributionが複数import packageを持つことも、namespace packageで1つのimport名に複数distributionが対応することもある。

解決戦略は多層にする。

```text
0. 削除された stdlib module の backport 判定 (bundled 表のいずれかの minor に入っている root を、venv か map が直接宣言した distribution に結びつけていれば、その distribution とみなす。`legacy-cgi` → `cgi` / `cgitb`、`standard-imghdr` → `imghdr`、`audioop-lts` → `audioop` など PEP 594 の backport を bundled map に持つ。backport は module を外した minor でそれを提供するので、target の stdlib がまだ持っていてもその import を backport の利用として数え、同じ module の他の backport との曖昧さの warning も出さない (#939)。宣言していなければ下の手順のままで、stdlib に無い minor では backport を候補に CHK003 になる)
1. stdlib判定 (bundled 表 `resolver/stdlib/modules.txt`。3.10 〜 3.14 各版の `sys.stdlib_module_names` に `__main__` を加え、root ごとに入っている minor の範囲を付けて 1 本にまとめたもの。`sys.stdlib_module_names` の無い 3.8 / 3.9 は、CPython 3.8.20 / 3.9.25 の stdlib・lib-dynload と `sys.builtin_module_names` から求めた 3.10 との差分 (3.10 で消えた `formatter`・`parser`・`symbol` など、3.9 で入った `zoneinfo`・`graphlib` など) を generator に持たせて加える (#940)。`scripts/generate-stdlib-modules.py` で再生成。`target_version` から `requires-python` の上限 minor (上限なしなら最新 bundled) までのいずれかで stdlib なら stdlib。`sys.version_info` ガード下の `tomllib` 等を誤検出しないため)
2. first-party module判定
3. workspace member判定
4. local .venv の dist-info / METADATA / top_level.txt / RECORD を読む
5. Core Metadata の Import-Name / Import-Namespace を読む
6. bundled package-module-map を使う
7. user-defined package_module_map を使う
8. 最後に import root を PEP 503 `normalize_distribution_name` で正規化し、manifest の宣言依存か lockfile の package(import したファイルの PEP 723 script block・所属 workspace member の manifest / lockfile も含む)に同じ正規化名があればその distribution とみなす(confidence `Likely`。`import foo_bar` と宣言 `Foo_Bar` など)。完全一致しなければ、正規化した宣言名・lock 名から接頭辞 `python-` / `py-` / `py` と接尾辞 `-python` / `-py` / `py` を外した名前と比べ、一致すればその distribution とみなす(confidence `Maybe`。`import docket` と宣言 `pydocket`、`import discord` と lock `discord-py` など)。それでも一致しなければ推測はせず `Unknown`(CHK010)にする。綴りだけでは third-party と決めない(#361)
```

step 4 の dist-info は、METADATA の `Name:` (無ければ directory 名の version より前) が PEP 508 の project 名 (`[A-Za-z0-9._-]` で両端が英数字) でなければ、その import・console script・`pytest11` を何にも対応付けず、`` resolve: ignored `<dist-info>`: `<name>` is not a PEP 508 distribution name `` の warning を出す (#973)。pip がそういう名前を install することはなく、信用すると制御文字入りの名前に `yaml` が対応付き、宣言した `pyyaml` が CHK002、`yaml` が CHK003 になる。無視した dist-info の import は bundled map 以降の手順で解決するので、finding は下げない。

root の `pyproject.toml` が宣言する生成 version file (`[tool.hatch.build.hooks.vcs] version-file`、`[tool.setuptools_scm] write_to` / `version_file`、`[tool.pdm.version] write_to` の `.py`) は build 時にだけ作られる first-party module とみなし、step 2 で解決する。module 名は layout の package root か `src/` の下に相対化したパスの先頭 (`src/_black_version.py` → `_black_version`) (#720)。

Pyodide ランタイムが組み込みで提供する `js` / `pyodide` / `pyodide_js` / `_pyodide` は、install された distribution ではなくランタイム自体が持つので、上の手順にも、`[build-system].requires` や context 不明の requirements ファイルの名前との照合にもかからなかったときに限り、distribution なしの third-party とみなして CHK010 を出さない。最後に置くので、venv・map・宣言で解決できる同名 package が先に勝つ。distribution は付けないので CHK003–CHK005 には使わない (#722)。

`TYPE_CHECKING` 配下の `_typeshed` は typeshed にしか無い型専用モジュールなので、上の手順より先に stdlib とみなす。runtime の `_typeshed` は今まで通り CHK010 になる。`TYPE_CHECKING` 配下で解決できない import は実行されないため、CHK010 を info に下げて typo 検出のためだけに残す(#584)。`try:` 本体・`else:` や `with suppress(ImportError):` 配下の optional import(CHK003 と同じ判定)も、見つからなくても例外が捕まえられ実行は失敗しないので同じく info にする。`except ImportError:` 節の fallback import は optional ではないため warning のまま(#654)。ただし `try:` 本体が import 文だけで、その root がすべて `requires-python` の下限 minor から上限(上限なしなら最新 bundled)までのどの版でも stdlib なら、`ImportError` / `ModuleNotFoundError` を捕まえる handler(下の「失敗した import を捕まえる `except` 節」)は実行されないので、その中の import も info にする(`import threading` に対する `dummy_threading`、`http.server` に対する Python 2 の `SimpleHTTPServer` など)。platform 専用か optional build の stdlib(`fcntl`・`msvcrt`・`sqlite3`・`tkinter` など)は入っていない interpreter があるので除く。下限が 3.8 / 3.9 のときもその版の集合で判定し (3.8 では `zoneinfo` / `graphlib` などは除かれる)、下限が 3.8 未満か Python 2 を許すとき、`requires-python` が無いときは下げない(#721)。CHK010 は (file, module) ごとに 1 件にまとめ、失敗しうる最初の import 行(すべて `TYPE_CHECKING` 配下か optional ならその最初の行)を位置にして残りの行は explain に "also imported at lines …" として載せる。まとめた import のどれかが失敗しうる(`TYPE_CHECKING` 配下でも optional でもない)なら warning、すべてそうでなければ info。docs / dev context の file(`docs/`・`examples/`・`scripts/`)は package を install した環境の外で動かすデモや補助 script なので、まとめた import によらず info にする(#694)。inline の `# chokkin: ignore[CHK010]` は、まとめた全行に付いているときだけ抑止する(#585)。位置を持たない参照 (#835) が 1 つでも混ざっていれば、他の行に付いていても抑止しない。その参照には directive を書けないため (#860)。

PEP 723 script の CHK002/CHK003 は script block に対して判定する。script と同じディレクトリにある module / package(`<dir>/<root>.py` や `<dir>/<root>/`)は `sys.path` 先頭で distribution を shadow するため CHK003 の対象外とし、それらの helper module の import も script block の依存の使用として数える。未解決(`Unknown`)の import root と first-party の import root(project 同梱の script が `uv run` で index から自 project を入れるケース)も、block が同名を宣言していれば使用とみなす。`[sys.executable, <file>, ...]` で別 file を実行する script は、子 process が block の環境を共有し import を追えないため script の CHK002 を出さない。`subprocess` を import する script では、複数語の文字列 literal(`"ruff format ..."`)の先頭語をコマンド名とみなし、同名の宣言依存を使用として数える。

`google` / `opentelemetry` / `databricks` のように複数 distribution が共有する namespace package は root だけでは決まらないため、root が third-party / unknown に分類された import は、user map と bundled map の dotted key(`google.protobuf`、`opentelemetry.sdk` など、2 segment 以上)を import 先の module 名(`from a.b import c` は `a.b.c`)に対して最長一致で引き、見つかればその distribution で上書きする。first-party root(`poetry` に対する `poetry.core` など)も、一致した dotted module が import 可能な位置(project root・`src/`・各 workspace member の直下または `src/`)に無い場合に限り同様に上書きする。`tests/fixtures/vendor/poetry/core/` のような深い位置のファイルは import できないため数えない。`pkg/__init__.py` は `pkg` として数える。この import 可能な module の集合は、probe 時に組み立てる `LocalTrees` から求める (#547)。

`azure` のように root 自体を提供する distribution が無い namespace は dotted key(`azure.identity` など)だけを bundled map に載せ、未登録の sub-package を任意の 1 distribution に誤って寄せない。同じ import 名を別の distribution も提供する場合(`paddle` の `paddlepaddle` / `paddlepaddle-gpu`、`haystack` の `haystack-ai` / `farm-haystack`)は両方を載せる。片方だけだと、もう片方を宣言した project では step 8 の宣言名照合より先に map の候補で決まり、宣言は未使用・import は未宣言に見えてしまう。候補が複数あれば import site ごとに宣言されている方を選ぶ(#732, #748)。map に無い namespace portion は、root が step 8 の宣言名照合で `Unknown` のままなら、先頭 2 segment を `-` で繋いだ名前(`sphinxcontrib.log_cabinet` → `sphinxcontrib-log-cabinet`、`sphinxext.opengraph` → `sphinxext-opengraph`)を同じく宣言依存・lock 名と照合する(confidence `Likely`)。共有 root だけでは distribution を決めない(#809)。root が map・venv・lock 名で third-party に決まっても、map の dotted key に一致しない import では、同じく先頭 2 segment を `-` で繋いだ名前を file の script block・member manifest・root が宣言していればそちらを選ぶ(confidence `Likely`)。`optimum-intel` は `optimum.intel` を配り、`optimum` 自体はその推移依存として lock にあるだけなので、root の lock 名で決めると宣言は未使用・import は推移依存に見える(#1022)。宣言されている portion を推移依存の root より優先するのは #732 / #748 と同じ考え方。lock にあるだけの portion では上書きしない(root の distribution 自身の subpackage かもしれないため)。bundled map は `data/package-map.seed.json` から `scripts/generate-package-map.py` で生成し、`--verify-wheels`(ネットワーク必須、CI 対象外)で各 entry の import 名を PyPI 最新 wheel の `top_level.txt` / RECORD と照合する。

resolver の warning は pipeline warning として報告する(#888)。venv・map で 1 つの import root に distribution 候補が複数あるときは `resolve: import `<root>` maps to several distributions (<候補>)`、`.venv` / `venv` はあるが site-packages や metadata を読めないときは `resolve: could not read virtualenv `<root からの相対 path>`: <理由>` を出す。venv を読めなくても解析は venv なしの経路で続け、finding の confidence は変えない。未解決の import は warning ではなく CHK010 として報告する。

同一 import root は resolver 内で1回だけ分類し、グラフへの `module_origin` 書き込みは `Unknown` から解決済みへの単調マージとする(複数 import site で矛盾してもダウングレードしない)。

Core Metadata 2.5(PEP 794、2025年9月承認)には `Import-Name` と `Import-Namespace` が定義されており、distributionが提供するimport名を表す。ただし承認直後でエコシステムへの普及はこれからであり、当面はこのfieldを持つpackageがほぼ存在しない。したがって現時点ではbundled package-module-mapと `.venv` metadataを主情報源とし、`Import-Name` は普及に応じて優先度を上げていく将来の主情報源と位置付ける。

binary名からdistribution名への逆引き(CHK008、binary usage判定)も同じ多層戦略にする。`.venv` があれば `entry_points.txt` / `RECORD` のscriptsを読み、なければbundled binary map、最後にuser定義の `[tool.chokkin.binary_map]` を使う。import名問題と同型の課題であり、専用のfallback dataが必要になる。

CHK008 は binary usage の distribution が次のいずれかで提供されていれば出さない (#680)。root の宣言済み依存、root 直下以外の requirements file (`extra_requirements`。`requirements/` と `docs/` / `doc/` 直下の `*requirements*.txt` / `requirements*.in` を含む、#723)、tox `deps` と nox `session.install` のリテラル、CI workflow (`.github/workflows/*.yml`) の `run:` にある `pip install` / `uv pip install` / `pipx install` / `uv tool install` の distribution 名と既知 action (`pre-commit/action` → `pre-commit`、`j178/prek-action` → `prek`) (#725)、各 workspace member の依存と requirements file、これらの lockfile 上の直接依存、宣言済み wrapper が提供する tool (`dagster-dg` → `dg`、主な dbt adapter (`dbt-snowflake` など) → `dbt`、`mkdocs-material` → `mkdocs`、`pre-commit-uv` / `prek` → `pre-commit`、`pytest-cov` → `coverage`、`tox-uv` → `tox`)。workflow は provider としてだけ読み usage には加えないので、github-actions plugin が無効でも抑制方向にしか効かない。さらに remote pre-commit hook の id は pre-commit が環境を用意するので、tool の config section (`[tool.mypy]` など) や hook 自体の usage は提供済みとするが、Makefile や CI の command 実行由来の usage は対象外のままにする。usage 自体は CHK002 の used 判定に残す。同じ distribution の binary は 1 件にまとめる。

重要なのは、`uvx chokkin` はprojectの仮想環境ではなく、`chokkin` 自身の一時環境で動く点。project venv必須にしてはいけない。`.venv` があれば読む、なければmanifest/lockfile/bundled map/user mapだけで解析する設計にする。

## 8. entry point自動推定

zero-configにはentry推定が必要。デフォルトでentry扱いにするものは次の通り。

```text
[project.scripts]
[project.gui-scripts]
[project.entry-points.*]
[tool.poetry.scripts] / [tool.poetry.plugins.*]
setup.cfg の [options.entry_points] / setup.py の setup(entry_points=...)
[tool.hatch.*.hooks.custom] / [tool.hatch.build.targets.custom] の path
__main__.py
main.py
app.py
manage.py
asgi.py
wsgi.py
noxfile.py
setup.py（root と各 workspace member 直下）
conftest.py
docs/conf.py
alembic/env.py
scripts/**/*.py
.github/**/*.py
examples/**/*.py
docs/**/*.py
**/*.ipynb
module top-level に `if __name__ == "__main__":` を持つ file
```

単独ファイル名でのentry推定(`main.py` / `app.py` / `manage.py` / `asgi.py` / `wsgi.py` / `noxfile.py`)は**project root直下、およびsrc layoutのpackage直下のみ**を対象にする。任意の深さで同名ファイルをentry扱いすると、unused file検出が事実上無効化されるため。`__main__.py` と `conftest.py` は全階層で有効。`docs/conf.py`（root と各 workspace member 直下）と `alembic/env.py` は記載のpathに限定する。Sphinx plugin は、これに加えて `index.rst` か `index.md` と同じディレクトリにある探索済みの `conf.py` も entry にする (§19、#814)。

root と各 workspace member 直下の `setup.py` は setuptools がビルド時に実行するため、origin `auto: setup.py` の dev entry にする (#1031)。既定の discovery glob にも root の `setup.py` を含め、src / flat layout でも読む。package 内や任意の子 directory の同名 file は対象外。hatchling の build script と同じく、import はその manifest の `[build-system].requires` で解決し、`--production` では file ごと外す。`setup.py` だけの project でも entry にするが、backend と build requires が未指定なら frontend の既定の build 環境として `setuptools` を使う。これは `setup.py` の import にだけ効き、runtime 依存の宣言には加えない。`project` / `exclude` / gitignore により走査から外れた file は追加せず、構文や文字コードが読めない場合は既存の parse warning / gap に従う。解析のために `setup.py` を実行することはない。

`examples/**` と `docs/**` の `.py` は root 直下と各 workspace member 直下を entry にする（rule `examples/**` / `docs/**`、#666）。どちらも `python examples/foo.py` や `fastmcp run docs/demo.py` で直接実行する単体スクリプトの置き場で、どこからも import されないため。これらは docs / dev context なので、依存の判定には使わず (#694)、解決できない import (CHK010) も info にする。その layout の package 内（flat layout の `examples` package など）は対象外。layout によらず既定の discovery glob は root の `examples/**` と `docs/**` を含むので、src / flat layout でも読む (#1023)。ただしこれらと `**/*.ipynb` の glob でしか拾わない workspace member 内の file は root の inventory に入れない。member は独自の layout を持ち、root の examples ではないため（mcp-python-sdk の `examples/snippets`、flask の `examples/celery`）。root の `examples/` に `__init__.py` があっても module 名は付けない（sqlalchemy の `examples/` は file として実行する）。

root 直下の `scripts/**` と `.github/**` の `.py` も entry にする（rule `scripts/**` / `.github/**`）。`.github/scripts/check_diff.py` のように workflow が `python .github/scripts/x.py` で直接実行し、どこからも import されないため (#734)。workflow の `run:` は読まずに tree 単位で扱い、context は変えない（langchain の `.github/scripts/get_min_versions.py` のように宣言の無い import は CHK003 のまま報告する）。

コマンドが path で直接実行する `.py` も entry にする (#797)。CI workflow の `run:`（github-actions plugin が無効でも読む）、`repo: local` の pre-commit hook の `entry:`、root の `.agents/skills/<name>/SKILL.md` と `.claude/skills/<name>/SKILL.md` の fenced code block と inline code span から、`python[3] [flags] <path>.py` と `uv run [options] [python] <path>.py`（`-s` / `--script` を含む）の path を読む。`sudo -E` や `bash -c`、`poetry run` などの wrapper の後ろも読み、`-m` / `-c` やコマンド名だけの `uv run pytest` は対象外。path は root 相対で、同じコマンド内の先行する `cd <dir>` と `uv run --directory <dir>` があればその directory 相対にする。directory 指定が無いとき、workflow は checkout の `path:` 配下で実行することがあるため（fastmcp の `python3 fastmcp/tests/downstream/_harness.py`）、そのままの path が無ければ先頭の component を外した path も試す。skill doc は同梱 script を skill directory 相対で書くことがあるため、skill directory 配下の path も試す。`${{ }}` や `$VAR` を含む path、絶対 path、`..` を含む path は読まない。discovery 済みの file を指すときに限り origin `command: <label> (<file>:<line>)` の entry とし、context は dev にする（`--production` では落とす）。手動や CI で実行する script なので、hatchling の build script (#735) と同じく file の context も dev にして、import する dependency group を CHK005 にせず、未宣言 import の CHK003 は `--strict` 時だけ出す（langchain の `.github/scripts/check_diff.py` の `packaging` は workflow が `pip install` してから実行する）。ただし同じ directory に `__init__.py` がある package の module は package 側から import されうるので file の context を変えない。他の root が本番で動かす file（`[tool.chokkin].entry`、manifest の entry point、runtime の plugin entry と symbol 参照、server の app、§8 の `main.py` / `app.py` / `manage.py` / `asgi.py` / `wsgi.py` の名前の auto entry、Dockerfile と Procfile の command の target）も、command が同じ file を実行するだけでは dev にしない。workflow が `python main.py` を実行しても `main.py` は app のままで、dev にすると `--production` で root が無くなり、そこから届く file と依存がすべて未使用になるため。scan 結果は config scan の cache に入り、SKILL.md も cache の入力にする。

command が server に渡す app module も entry にする (#1004)。task file と tool config の command（Dockerfile・Procfile・Makefile・justfile・GitLab CI・pre-commit の local hook・tox の commands・`scripts/` と `bin/` の shell script・PDM scripts・`[tool.poe.tasks]`・`[tool.hatch.envs.<env>.scripts]`）にある `gunicorn` / `uvicorn` / `hypercorn` / `daphne` / `waitress-serve` / `granian` の `module:attr` と、Python code が string literal で渡す `uvicorn.run("module:attr")` / `uvicorn.Config("module:attr")`（第 1 引数か `app=`、`import uvicorn`（`import uvicorn.config` などの submodule を含む）と `from uvicorn import run, Config`（`uvicorn.main` の `run`、`uvicorn.config` の `Config` を含む）の名前だけ）を読む。option の直後の word は値として扱い（`-c python:conf`、`-k egg:dist#worker`）、app を他に見つけられないときだけ使う。値を取らない flag（`--reload`・`--preload`・`--factory`・`-D` や `--no-*` など）と、値を抱えた `-w4`・`--workers=4` の直後の word は app として読む。saleor は `saleor.asgi:application` を Dockerfile の `CMD`・poe の `start`・`saleor/__main__.py` の `uvicorn.Config` からしか参照しない。`:attr` の無い module は runner の既定の attribute（`gunicorn` は `application`、`hypercorn` と `granian` は `app`、`uvicorn`・`daphne`・`waitress-serve` は無し。waitress 3 は `pkgutil.resolve_name` で解決し、既定の attribute を持たない）を symbol にする。module が discovery 済みの file に解決できるときだけ origin `command: <label> (<file>:<line>)` の entry にし、context はその file 自身の context にする（server は本番で動くので dev に倒さない。file が dev context なら `--production` で落ちる）。Python code の `uvicorn.run` は root から解決できないとき、その file と同じ directory の module も探す（`python script.py` は script の directory を `sys.path` に入れる）。Dockerfile の app も同じく、root から解決できないときは Dockerfile と同じ directory の module を探す（Dockerfile の directory が通常の build context）。root より下の Dockerfile / Containerfile は discovery の walk で拾い（project の glob は掛けず、exclude と gitignore と nested project は root の file と同じに外す）、server の app だけを読む (#1061)。中の `RUN` の binary・script・module・opaque な command は image 自身の tree と依存で動き、root の import root と manifest では読めないので、binary usage にも entry にも gap にもしない。`COPY` / `WORKDIR` は追わない。Dockerfile の directory からだけ解決した app は image 自身の tree で動くので、その directory の下の import の判定を gap で下げる（UnreadImageTree、§11）。解決できない module（third-party や typo）は project の file を実行しないので何もしない。走査から外れた file を指すときは既存の entry path と同じく warning にする。同じ command 内の先行する `cd`、`uv run --directory`、runner の `--app-dir` / `--chdir` / `--pythonpath` / `--working-dir`、`PYTHONPATH=` の代入（`env` の後を含む）、poe task の `cwd`（`cd <cwd> &&` として読む）、`uvicorn.run(..., app_dir=...)` は import root を動かすので読まず、下記の opaque な command として gap にする。`celery -A` は worker を起動するが app を属性として import するとは限らないため entry にせず、gap のままにする。f-string や変数で渡す uvicorn の app、app object を直接渡す `uvicorn.run(app)` は読まない（recall の取りこぼし。後者は同じ file にあるので不要）。parse から読む app は config scan の cache に入れず、parse cache 側に持つ。

task file と tool config の command（下記 #913 の一覧）が実行する script（`python X.py`）と `-m` の module も entry にする (#912)。script は #913 と同じ組み立てで最初に見つかった discovery 済みの file、module は `pkg.mod.__main__`、`pkg.mod` の順で解決した file を、origin `command: <label> (<file>:<line>)` の entry にする。Makefile・tox・pre-commit などの command は手元や CI で動くので entry の context は dev にし、#797 と同じく package の外の file は file の context も dev にする（dependency group を import しても CHK005 にしない）。ただし `__main__` block を持つ file (#1026) と wheel target が配布する file は、Makefile の `run:` が app そのものを動かすことが多いので file の context を変えない（#797 の CI script はこの例外を持たず dev のまま）。Dockerfile と Procfile の command は deploy した image や process で動くので、entry の context はその file 自身の context にし、本番で動かす root として file の context を dev にしない。`cd`・`uv run --directory`・`PYTHONPATH=` で import root を動かした `-m`、task runner が読み込む module（`celery -A`）、`-c` の code が import する module は entry にせず、#913 の gap のままにする。解決できない target は何もしない。`--production` では dev の entry が落ちるので、dev の command の target はどこからも届かなければ gap のまま残る。datamodel-codegen の `custom-formatters` が名指す formatter module (#1063) も同じく dev の command target にする。airflow の `task-sdk/dev/datamodel_code_formatter.py` は package の外にあるので file の context も dev になり、`--production` では解析対象から外れる。

command は shell を実装せず、次の範囲だけを読む (#861)。行末の `\` による継続行を 1 行につなげ、改行・`;`・`|`・`&` で segment に分ける（`&&` と `||` も同じ区切りになる）。segment は空白とタブで word に分け、word の両端の引用符（`"` `'` と backtick）を外す。`$` で始まらない word は両端の `(` `)` も外す。各 segment では、`-` で始まる word、`NAME=value` の環境変数前置、`exec` / `env` / `time` / `nohup` / `command` / `sudo` / `then` / `do` / `else` / `!`、`sh -c` / `bash -c`、`uv` / `poetry` / `pdm` / `hatch` / `rye` / `pipenv` の `run` を読み飛ばした次の word をコマンド名とする。`cd <dir>`（2 word ちょうど）は同じ command の後続 segment の directory に積む。引用符の対応は取らないので、引用符の中の区切りでも segment を分ける（`sh -c "make; python x.py"` の `python x.py` を読む）。`#` コメント、heredoc、subshell、`if` / `for` の構文、alias、`pipx run` は解釈しない。`#` で始まる segment はコマンド名が `#…` になって何も実行しないが、コメント中の `;` は区切りとして扱う。この範囲の外の書き方は認識しないだけで（recall の不足）、finding は下げない。SKILL.md の fenced code block は、行頭の空白を除いて `` ` `` か `~` が 3 個以上並ぶ行で始まり、同じ文字が開始以上の個数並ぶ行で閉じる（info string は見ない）。閉じない block は文書の終わりまでを本文にする。fence の外では、各行の backtick の対の中を inline code span として読む。

hatchling が build 時に path で読み込む custom builder / hook も entry にする (#735)。root と各 workspace member の `pyproject.toml` の `[tool.hatch.build.targets.custom]`・`[tool.hatch.build.hooks.custom]`・`[tool.hatch.build.targets.<target>.hooks.custom]`・`[tool.hatch.metadata.hooks.custom]` の `path`（省略時 `hatch_build.py`、manifest 相対）が discovery 済みの file を指すときに限り、origin `manifest: [<table>].path` の entry とする。build 時にだけ動き wheel には入らないので dev context とし（§10）、`--production` では落とす。その file の import は build 環境が入れる `[build-system].requires` で解決し、宣言済みとして CHK003 / CHK004 / CHK005 の対象外にする（airflow の `hatch_build.py` の `hatchling`）。ただし宣言済みの distribution の使用としては group を問わず数え、CHK002 を出さない（openai-python の `scripts/hatch_metadata.py` が import し `build` group に宣言した `hatchling`、#783）。

PEP 517 の build backend として frontend が import する module も entry にする (#782)。PEP 517 で必須の hook `build_wheel` と `build_sdist` を、両方とも定義するか `__all__` で再 export する runtime context の module が対象で、origin は `build backend: PEP 517`。利用者の `build-backend` に書かれて読まれる互換 module（poetry の `poetry.masonry.api`）は、リポジトリの中から import されないため。manifest の `build-backend` とは照合しないので、同名の 2 関数を持つだけの module も entry になるが、PEP 517 の必須 hook を両方そろえる module はまれで、誤りは未使用を見逃す側に倒れる。`__all__` を書かずに `from x import *` で hook を受け渡すだけの shim は対象外。片方の hook だけを持つ module や、test などの runtime 以外の file も対象外。`[build-system].backend-path` の各 directory 直下にある in-tree backend（`build-backend = "backend"` と `backend-path = ["_custom_build"]` の `_custom_build/backend.py`）は、hatchling の build script (#735) と同じく、origin `manifest: [build-system.backend-path].path` の dev の entry とし、import を `[build-system].requires` で解決する。frontend が build 環境に入れてから import し、wheel には入らないため。`build-backend` が dotted name のとき（poetry-core の `poetry.core.masonry.api` を `backend-path = ["src"]` で読む self-hosting）は配布する package の module なので、build script にはせず上の hook 判定だけで扱う。

discovery が拾った notebook（`.ipynb`）は深さを問わず全て entry にする（rule `**/*.ipynb`、#514）。既定の discovery glob は layout によらず `**/*.ipynb` を含むので、src / flat layout でも `notebooks/` などの package 外の notebook を読む (#1023)。notebook は cell 単位で直接実行され他の file から import されないため、PEP 723 script と同じく root としてしか使われない。そのため notebook 自身は CHK001 にならず、notebook からだけ import される module も到達可能になる。app 判定（`has_clear_app_signals`）は file 名で見るため、notebook があっても mode は変わらない。

module top-level に `if __name__ == "__main__":` の block を持つ file も、深さと file 名を問わず entry にする（rule `main-block`、#1026）。この block は `python path/to/file.py` で直接実行するために書くもので、fastmcp の `contrib/mcp_mixin/example.py` や airflow の `gunicorn_config.py` のように、どこからも import されない file がこれを持つ。file 名でなく内容で決めるので `scripts/**` などの path 規則に漏れた置き場も拾う。guard は §6 の `in_main_block` と同じ `__name__ == "__main__"` の比較で、`elif` や `and` の中（`__name__ == "__main__" and not TYPE_CHECKING`）も含むが、その比較だけで branch が除外されるときに限る。`or` の相手が不明なら branch は残るので数えず、`if TYPE_CHECKING:` のように比較を含まない除外も数えない。`__main__.py` と notebook は常に `__main__` として動くので数えない。関数や class の body の中の block はその file の実行方法を示さないので対象外にし、`__name__ in ("__main__",)` など認識しない形の guard も entry にしない。認識できないときは file が orphan のまま CHK001 に残るので、見逃しは silence でなく報告になる。file の context は discovery のまま（test tree の block は test、package 内の block は runtime）で、`--production` では runtime 以外の block を落とす。mode の判定は file 名だけで見るので、深い `app.py` が block を持っても library は app にならない。`--trace` は origin `auto: main-block` で示す。

entry の参照先 (`module:attr`) を source file に対応付けられないとき、その entry は root にせず、`` entry: could not map `mod` to a source file (`project.scripts.x` in `pyproject.toml`) `` を warning に出す。workspace member の manifest の entry も同じで、file は root からの path で書く。ただし、inventory には無いが disk に対応する file がある (`exclude` した file を指す) ときは、その file を path entry と同じく skip した entry として扱い、下の `` entry: skipped entry `path` `` を出して gap にする (#933)。disk のどこにも file が無い参照先 (typo、build 時に生成する module、install 済みの distribution の module) は、読めた code を何も実行しないので、disk に無い config entry と揃えて gap にしない。root にしようとした file が discovery の inventory に無いとき (exclude した config entry や plugin entry など) も、`` entry: skipped entry `path`: not a discovered source file; unused files and dependencies are reported below certain `` を出す。その file が import するものは読まないので、skip した entry は gap にする (§11、#923)。member の manifest の entry が、member の inventory にあって root の inventory に無いとき (root の `exclude` が member の file を外したとき) も、同じ warning を root からの path で出して gap にする (#934)。disk に無い config entry や directory を指す config entry は source discovery が `sources: …` で報告するので、重ねて出さず、何も実行しないので gap にもしない。

ただし、library projectではaggressiveにunused filesを出すと誤検知が増える。`mode = "auto"` の判定は次にする。

```text
gui_scripts がある、console_scripts の参照先が自 package の外にある、
または package の外に runtime の manage.py / asgi.py / wsgi.py / app.py の entry がある
(workspace member では自 package を指す console_scripts も含む)
  -> app mode

[project] name (無ければ [tool.poetry] name、または setup.py の setup() に静的に読めない name) があり、
src/<package>/__init__.py または root直下の <package>/__init__.py
(flat layout) があり、明確なentryがない
  -> library mode

いずれにも該当しない
  -> app mode。ただしunused_fileのconfidence上限をlikelyに落とす
```

workspace root も member の数によらず同じ基準で判定する (#776)。従来は member が 2 つ以上あると app mode に固定していたため、mlflow のように root が自 package の wheel を配布する library でも公開 API が CHK006 warning になっていた。root が app mode のときは各 member を下記の `is_library_member` で判定する。root が library mode のときは member も library mode で判定する。

app signal は project 自身の package（`layout.packages`）の外にあるものに限る（#652）。`django-admin = "django.core.management:..."` や `httpx = "httpx:main"` のように、console entry の参照先 module の先頭要素が自 package なら、CLI を同梱した library とみなして app signal にしない（GUI を同梱する library は想定しないため、gui entry は常に app signal）。`manage.py` 等のファイル名も、package 内の module（werkzeug の `src/werkzeug/wsgi.py`、mitmproxy の `mitmproxy/tools/web/app.py`）や test / docs / dev context の file（flask の `tests/test_apps/helloworld/wsgi.py`）は数えない。どちらも従来は library の公開 API を app mode の CHK006 certain にしていた。ファイル名の signal は entry になった file だけを数える。単独ファイル名の自動検出は root 直下と package 直下に限るので、`deploy/wsgi.py` は `entry` 設定や plugin（Procfile の command など）が entry にしたときだけ app signal になる。workspace member の判定（`is_library_member`）は file 名の基準だけを共有し、自 package を指す console entry も app signal のままにする。monorepo は CLI を専用 member（airflow の `airflow-core`、llama_index の `llama-dev`）に分けるため、CLI を持つ member は app とみなす。app signal を持つ workspace root（fastmcp）や CLI を持つ member が app になっても、wheel が配布する file の `__all__` は公開 API なので CHK006 / CHK007 にしない（§12、#678、#727）。mode 自体は変えない。metapackage の root が配布する member も同じく、mode は app のまま CHK001 の confidence だけを下げる（#1060）。root の wheel target が file を1つも含まず（hatch の `bypass-selection = true` 等。#529 の CHK002 と同じ判定）、root 自身も app signal を持たない（root の判定と同じく、自 package を指す console entry は数えない）とき、root の `[project].dependencies` / optional-dependencies / `extras_require` が名指す distribution の member（名前は PEP 503 で正規化して member の `[project].name` と照合）は、利用者が install する library 本体であり、自 package を指す console entry は CLI 同梱とみなす（`is_distributed_library`。gui entry や package 外の manage.py 等があれば対象外）。その member は `EntryPlan.distributed_members` に入り、member 自身の wheel target（`apply_member_surfaces` と同じく member manifest から求め、パスは member 相対。wheel target を宣言しない member は CHK006 / CHK007 と同じく layout の package を配布するとみなす、#733）が含む未到達 file は、CHK001 を library mode 相当の confidence（`maybe`）にする（`apply_distributed_surfaces`）。wheel の外の file は app mode 相当の confidence のまま、CHK006 / CHK007 も app mode の severity のままにする（member ごと library mode にすると、`fastmcp_slim` の未使用 symbol の CHK006 がすべて info に落ちて検出を失う）。fastmcp の root は `fastmcp-slim[...]` だけを配るので、`fastmcp_slim` の wheel が含む未到達 file は `maybe` になる。airflow の root も metapackage だが、root 自身の `apache-airflow` console entry が app signal なので何も配布せず、`airflow-core` はほかの app member と同じに判定する。code を配る root が member に依存するのは library を使う app であって配布ではなく、dependency group と build requires は配布しないので、どれも数えない。member を経由した推移的な依存も辿らない。この理由で confidence を下げた file は、`--explain` の CHK001 details と `--trace` の negative trace に `confidence: library (workspace member ... which the metapackage root distributes as ... in <file>:<line>, and its wheel ships this file)` の1行を出す。

setup.py の `setup()` が `name=` を渡していれば、値を読めなくても name ありとみなす (workspace member の判定も同じ)。これがないと、requests のように `name=about["__title__"]` を `exec` 経由で埋める library が name を静的に解決できず app mode に落ち、`--production` で全 file が CHK001 になる (#586)。

workspace member（§5。宣言済み・自動検出とも、#488 / #587）は、`mode = "auto"` でrootがapp modeになったとき（entry 構築時に `build_entry_roots` が判定する）、member manifestに `[project].name` がありapp entry（console_scripts / package 外の manage.py 等）がなければ `EntryPlan.library_members` に入る。namespace package（`llama_index/`）は `__init__.py` を持たないため、root判定と違いpackageの存在は要求しない。そのmember配下のファイルはCHK001のconfidence・severity・test除外と、そこで定義・再exportされるsymbolのCHK006 / CHK007のseverityをlibrary modeで判定し（`EntryPlan::mode_for`、#515）、rootのwheel public surfaceによる引き上げの対象外にする。代わりにmember自身のwheel target（member manifest、パスはmember相対）から求めたpublic surfaceを使い、その外にある未到達file（`scripts/release.py` 等）はCHK001をapp mode相当のconfidenceに戻す（`apply_member_surfaces`、#587）。入れ子のmemberでは最も深いmemberのsurfaceで判定する。

`app mode` ではunused filesを積極的に出す。`library mode` では、public moduleは外部利用され得るため、unused filesは `maybe` confidenceに落とし、デフォルトでは表示しないかinfo扱いにする。libraryで本気のunused file検出をしたい場合は、ユーザーに `entry` を明示させる。

library mode の public surface は wheel target 設定から静的に求める (v0.5 R-05): `[tool.hatch.build.targets.wheel]` (なければ `[tool.hatch.build]`) の `packages` / `include` / `only-include`、`[tool.setuptools]` の `packages` / `packages.find` / `package-dir` / `py-modules`、`[tool.pdm.build]` の `includes`、`[tool.flit.module]`、`[tool.maturin]` の `python-source` / `module-name`。`build-backend` が既知ならその backend の table だけを読む。hatch の `sources` による path 書き換えは読まない。surface 外の未到達 file は CHK001 を app mode 相当の confidence に戻し、surface 外 module の symbol は CHK006 を app mode 相当 (warning) にする。どちらも同じ判定（`EntryPlan::surface_mode`）を使い、戻した file は以降 app mode の file として扱う（ライブラリ利用者が import し得る orphan とはみなさず、CHK002 の使用元にも unread gap にもしない、#548）。public surface は一度だけ求めて rules に渡す。設定がない、または一致する file がない場合は従来どおり全 file を public 扱いにする。wheel target が宣言されているのに一致する file がない場合 (`bypass-selection = true` を含む) は code を持たない metapackage とみなし、配布する依存を CHK002 の対象外にする (§10、#529)。

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
airflow
dagster
prefect
scrapy
```

plugin の有効/無効は `[tool.chokkin.plugins]` の明示値 > enabler > 既定値 (pytest / django / fastapi のみ on) の順で決める (v0.5, R-07)。enabler は Knip の enablers 相当で、`src/plugins/enablers.rs` の表 1 か所にまとめ、root と各 workspace member の宣言済み依存 (例: `flask` / `celery` / `sphinx` / `mkdocs` / `alembic` / `apache-airflow` / `dagster` / `prefect` / `scrapy`) と設定ファイルの存在 (`tox.ini` / `noxfile.py` / `.pre-commit-config.yaml` / `docs/conf.py` / `mkdocs.yml` / `alembic.ini` / `airflow.cfg` / `workspace.yaml` / `dg.toml` / `prefect.yaml` / `scrapy.cfg`) だけを見る。github-actions は workflow がほぼ全 repo にあり未宣言 tool の CHK008 を大量に出すため enabler を持たない (workflow の install は plugin と無関係に CHK008 の provider として読む、#725)。解決は probe (Step 1〜4) で workspace member の確定後に行い、結果を `effective_config.plugins` に書き戻すので config-scan cache key にも自然に入る。`--probe` の `Plugins` 節は有効な plugin と既定以外の理由を持つ plugin を `default` / `config` / `disabled-by: config` / `enabled-by: dependency <name> [(member <id>)]` / `enabled-by: file <path>` で表示する。

pluginの責務は3つだけにする。

```text
1. entry filesを追加する
2. string/module referencesを追加する
3. binary usageを追加する
```

plugin が path で entry にする file (Django の `manage.py` と settings、Sphinx の `docs/conf.py` (root document の隣の `conf.py` は探索済みの file だけなので加えない。探索で runtime context になったものは docs context に付け替える)、Alembic の `alembic/env.py`、Airflow の DAG file、Prefect の entrypoint、Dagster と Scrapy が設定で名指す module) は、layout の推定 glob や明示した `project` の外にあっても source inventory に加え、通常の parser で読む (#932)。その import は到達性と依存の使用に効く。flat layout の root `manage.py` や、`project = ["src/**/*.py"]` の下の `docs/conf.py` がこれにあたる。plugin の有効化と member の確定後、parse より前に entry を一度集め、名指しした file だけを加える。親 directory 全体には広げない (Airflow の DAG folder、Dagster の `defs_module`、Scrapy の `SPIDER_MODULES` は、tool 自身が directory を walk して読むので、その下の file を名指したものとして扱う)。context は path が §10 で test / docs / dev になるときはそれを、runtime になるときは plugin が付けたものを使い、`--production` では test / docs / dev の entry を加えない。discovery が見つけてから落とした file (`--production` の test、dev 専用 member、nested project の中) は entry として戻さない。`exclude`・gitignore・symlink の扱いは通常の discovery と同じで、外れた file は §8 の skipped entry の warning と gap のまま残る。読めない file は §6 の parse warning と gap にする。disk に無い entry は source にせず、gap にもしない。warning とその他の hint は、加えた file を含む inventory で parse 後に抽出する。inventory だけから entry を選ぶ plugin (pytest の test file、Alembic の revision など) は inventory の外の file を加えない。

Django pluginの例。

```text
manage.py をentryにする
DJANGO_SETTINGS_MODULE が指す settings module (なければ settings.py) をentryにする
INSTALLED_APPS の文字列をmodule referenceにする
MIDDLEWARE の文字列をsymbol/module referenceにする
ROOT_URLCONF をmodule referenceにする
WSGI_APPLICATION / ASGI_APPLICATION の module.symbol を module:symbol reference にする (既存のコロン表記も読む)
migrations/**/*.py はframework-used扱いにする
INSTALLED_APPS の project app の apps.py / models / admin / templatetags / management commands をentryにする
```

`DJANGO_SETTINGS_MODULE` は root の `manage.py`、続けて発見した `manage.py` / `wsgi.py` / `asgi.py` / `runtests.py` / `conftest.py` (runtime context を先に)、発見されていない root の同名 file (wagtail の `runtests.py`)、最後に pytest-django の ini option (`pytest.ini` / `pyproject.toml` / `tox.ini` / `setup.cfg` のうち pytest が選ぶ config の `DJANGO_SETTINGS_MODULE`) から、`os.environ.setdefault("DJANGO_SETTINGS_MODULE", "...")` と `os.environ["DJANGO_SETTINGS_MODULE"] = "..."` の文字列 literal を読む (#914、#979)。`examples/` / `example/` の下の setter は sample project のものなので読まない。dotted な module 名でない値 (project template の `{{ project_name }}.settings.dev`) は literal でない形と同じに扱う。読めた値はどれも module reference にし、project の file に解決できる最初の値を settings module とする (先に読んだ値が別 app や template のものでも、後の値を隠さない)。`.env` は読まない (shell や runner が読むかどうかを静的に決められない)。settings module は発見した source、なければ disk 上の `pkg/mod.py` / `pkg/mod/__init__.py` (`src/` の下も)、最後に値を設定した file の import 元 (root にない `manage.py` ならその directory、`wsgi.py` / `asgi.py` なら一つ上の directory。Django が `sys.path` に置く場所) の下に解決し、`config/settings/dev.py` のような package の中の module もそのまま entry にする。解決できないときだけ、深さ 4 までの `settings.py` を候補から選ぶ (複数あれば ambiguous 警告)。候補は `examples/` / `example/` の外にあり、top-level で `INSTALLED_APPS` / `ROOT_URLCONF` を代入するか star import する file に限る (#979)。`views/settings.py` のような同名 module を settings にしないためで、どの候補も当てはまらなければ settings を選ばない (Django project なら下の gap になる)。読めない候補は残し、読む段階で gap にする。候補を field の数で順位付けする案は、本物の settings どうしの差が意味を持たないので採らない。settings は top-level の `from m import *` を、相対 import も含めて project の file に解決できるかぎり辿り (最大 16 file。発見が package として扱わない directory の相対 import は、import する file の directory から disk 上の file に解決する。wger の `settings/`、#1065)、各 field を辿った file すべてから読む。`ROOT_URLCONF` と `WSGI_APPLICATION` / `ASGI_APPLICATION` は最初に読めた file のものを使い、`ROOT_URLCONF` の URLconf は同じ順で発見した source に解決できたときだけ entry にする。`django-split-settings` の `include()`、`exec()`、`runpy.run_path()` が実行する file と、引数なしの `globals()` / `vars()` / `locals()` への `.update(...)` が足す名前は読まず、下の条件で gap にする (#925、#937)。settings を import したときに実行される呼び出しだけを数え、module top-level と `if` / `try` / `with` などの block の中は対象、関数・class・lambda の本体は対象外にする。`include` / `exec` / `run_path` は名前 (`include`) と属性の末尾 (`split_settings.tools.include`) で判定し、import 元は解決しない。`.update()` は受け手が名前空間を返す builtin の呼び出しのときだけ数え、dict や `vars(obj)` の `.update()` は settings の名前空間ではないので数えない。`include()` の glob を展開して読むこと (recall) はしない。`globals()["X"] = ...` や `setattr(sys.modules[__name__], ...)` など他の書き込み方は読まず、gap にもしない。

settings を読めないことは gap にする (§11、#914)。Django project であること、つまり root に `manage.py` があるか、`DJANGO_SETTINGS_MODULE` を設定する file があることを条件にする。django を依存に持つだけの reusable app は自分の settings を持たないので、settings が見つからなくても gap にしない。`migrations/**/*.py` の framework-used は settings の有無によらず置く。Django は INSTALLED_APPS にある app すべての migrations を読むので、reusable app (DRF の `authtoken/migrations/`) の migrations も使われる。settings が見つかった project では origin を settings file に、見つからなければ gap の file、reusable app なら Django を宣言した行にする。gap になるのは、settings module も `settings.py` も見つからないとき (`DJANGO_SETTINGS_MODULE` の literal が project の file でない、literal でない形で設定している、`manage.py` が何も設定していない、の順に理由を選ぶ)、settings module と辿った star import 先の file を読めないか、発見していない file で parse できないとき (発見した file の構文エラーは parse の skip が報告する)、そして `ROOT_URLCONF` / `INSTALLED_APPS` / `MIDDLEWARE` のどれかが辿った file のどこにもなく、かつ project の file に解決できないか 16 file の上限で辿らなかった star import があるか (`django-configurations` の基底 class のように、そこで設定しているかもしれない)、辿った file が `include()` / `exec()` / `run_path()` か名前空間の `.update()` を呼ぶとき (両方あれば star import を名指しする)。3 つとも設定していれば、star import や呼び出しが値を足していても gap にしない。literal でない `DJANGO_SETTINGS_MODULE` でも `settings.py` の候補が見つかればそれを読むので gap にしない。literal が project の file でなくても候補があれば同じにする。理由は `` plugin: django: … ; unused files and dependencies are reported below certain `` の warning で出す。

`INSTALLED_APPS` / `MIDDLEWARE` は文字列 literal list として読み、literal list でなければ partial parse 警告を出す。追加の import 設定は `src/plugins/django/settings_refs.rs` の `IMPORT_PATHS` で定義する (#1003)。`AUTHENTICATION_BACKENDS` / `PASSWORD_HASHERS` / `DATABASE_ROUTERS` の list・tuple、`DEFAULT_FILE_STORAGE` / `STATICFILES_STORAGE` / `EMAIL_BACKEND` / `SESSION_ENGINE` / `TEST_RUNNER` / `FORM_RENDERER` / `CSRF_FAILURE_VIEW` の文字列、`AUTH_PASSWORD_VALIDATORS[*].NAME`、`STORAGES.*.BACKEND`、`TEMPLATES[*].BACKEND` と `OPTIONS` の `context_processors` / `libraries` / `builtins`、`LOGGING` の handler の `class` と handler・filter・formatter の `()` を module reference にする。nested container は literal の値だけを読み、storage の bucket 名など他の option は third-party import とみなさない。値が計算式や名前、container の展開などで読み切れなければ、読めた literal は残し、その field と行を示す settings gap・warning を出す (ADR 0005)。省略された optional key は gap にしない。

表に無い設定も、代入の右辺にある dotted path の文字列 literal が発見した first-party module または `module.attr` (module が存在するもの) を指せば module reference にする。`BUILTIN_PLUGINS` / `PRIVATE_FILE_STORAGE` / `CELERY_BROKER_TRANSPORT` のような独自設定も `--production` で辿れる。module-level の `if` / `try` 等の block、型注釈付き代入、`+=`、`STORAGES['default']['BACKEND'] = ...` のような literal key・index による更新も読む。known field の key が計算式なら、右辺の first-party 文字列は拾うが、import slot を決められないので field の gap にする。関数・class・lambda 本体と dict の key は読まない。各 reference の origin は settings file・文字列の行・field 名にし、`--explain` の根拠にも使う。first-party に解決できない文字列は参照にしない。任意の設定は import を表すとは限らず、存在しない first-party module も隠さないため gap にもしない。表に無い field の計算式そのものは評価しない。これらの設定は、辿った star import 先からも同じように読む。

`WSGI_APPLICATION` / `ASGI_APPLICATION` は Django の通常の dotted path (`pkg.server.application`) と従来から読む `pkg.server:application` を symbol reference にする。`None` / 空文字列は application を設定していないので参照も gap も足さない。計算式や不正な target で読み切れなければ、field と行を示す settings gap・warning にする。

`INSTALLED_APPS` (list か tuple の literal) の値が project の package (`mysite.polls`) か、project の module にある `AppConfig` の class (`mysite.polls.apps.PollsConfig`) を指せば、Django が規約で読み込む module を entry にする (#399)。config の class は used にし、app の directory は class 本体の literal な `name` が指す package とする。`name` が literal でなければ class を定義した module の package とし (Django の app は config を自分の package に置く)、`name` が無ければ third-party の `name` を継ぐ config (`AdminConfig` の subclass) なので app の directory は足さない。entry にするのは app の package の下の `apps.py` (その中で基底の名前が `Config` で終わる top-level の class も used。Django が default の config を module から選ぶため)、`models.py` / `models/__init__.py`、admin が入っているときの `admin.py` / `admin/__init__.py`、`templatetags/` の下 (`templatetags/__init__.py` 自身を除く) で `register` を束縛する module (`register` と `@register.*` で装飾した名前も used)、`management/commands/` 直下で `_` で始まらず `Command` を束縛する module (`Command` も used)。admin が入っているとみなすのは `django.contrib.admin` か、`SimpleAdminConfig` 以外のその config、`AdminConfig` を基底に持つ project の config があるときで、`INSTALLED_APPS` を読み切れなかったとき (literal でない要素、同じ file での 2 度目の言及 (`+=` / `.append()`)、読めない settings file、辿らない star import、`include()` などの呼び出し) も入っているとみなす (ADR 0005)。`register` / `Command` を束縛しない module は Django が読み込めないので import graph に任せ、parse できなかった module は束縛しうるので entry にする。app の package の中のどの module でも、`@register` (`@checks.register()`、`@admin.register(Model)`) で装飾した名前は used にする。`@receiver` で装飾した名前は、app に属さない module (netbox の `netbox/search/signals.py`) でも import されれば signal につながるので、settings の無い reusable app を含むどの module でも used にする (#698 の Django signal の分)。`signals.py` / `checks.py` は `AppConfig.ready()` の中の import から辿る。third-party の app、project の package も config も指さない値、`__init__.py` の無い directory (layout が package とみなさず、もともと CHK001 にならない) は何も足さない。`INSTALLED_APPS += [...]` や `.append()` で足した app は読まない (admin が入っているとみなすだけ)。templatetags に `TEMPLATES` の Django backend の有無は問わない。

pytest pluginの例。

```text
**/test_*.py / **/*_test.py をtest contextのentry rootにする
  (testpaths 未設定のpytestはrootdir配下から集めるため、pandas/tests/ や
   monorepo memberの tests/ も含む)
  (pytestのtest discoveryに相当。conftest.pyはtest filesをimportしないため、
   test file自体をrootにしないとtest内のimportが依存使用として数えられない)
conftest.py をentryにする
pytest_plugins = ["..."] をmodule referenceにする
Python sourceのmodule-level pytest_plugins = "..." / [...] / (...) の文字列literalを
  そのfileからのimportにする (conftest以外も対象。pluginとして読み込まれたmoduleの
  pytest_pluginsも有効なため。解決できない名前はunresolved警告にしない)
[tool.pytest] (pytest ≥ 9 の native table) または [tool.pytest.ini_options] を読む
  (ini_options 以外のキーがあれば [tool.pytest] を優先。testpaths / python_files が
   あれば上記globを上書き。testpaths の "." はrootdirとして扱う)
workspace member 内の test context の file には testpaths に関係なく python_files を
  適用する (member の suite はディレクトリを明示して pytest に渡すため。airflow の
  providers/*/tests/**/example_*.py)
pytest command usageをbinary usageにする
```

FastAPI/Uvicorn pluginの例。

```text
uvicorn acme.api:app を module:symbol reference として読む
src/main.py / src/asgi.py をentryにする
```

root 直下と `src/<package>/` 直下の `main.py` / `asgi.py` などは §8 の自動推定が entry にするため、plugin は §8 が拾わない `src/` 直下の module だけを足す (`SRC_APP_ENTRY_CANDIDATES`)。`@router.get` / `@router.websocket` などの handler を externally used にするのは plugin ではなく §12 の decorator 判定。

Celery pluginの例。

```text
@shared_task / @app.task decorated functionをexternally used扱いにする
autodiscover_tasks() の対象を推定する
```

CI/config pluginの例。

```text
.github/workflows/*.yml の run: から python -m, uv run, pytest, mypy, ruff, alembic 等を拾う
.pre-commit-config.yaml の hook id と、repo: local の hook の entry を拾う (remote hook は pre-commit が環境を用意するので、その binary は CHK008 で提供済みとみなす)
tox.ini / pyproject [tool.tox] (`legacy_tox_ini` の文字列と tox 4 native の `env_run_base` / `env_pkg_base` / `env.<name>` table、#724) / `tox.toml` の commands / commands_pre / commands_post を拾う (deps / description / allowlist_externals は command として拾わない。deps の distribution 名は CHK008 の宣言として数える)
noxfile.py の @nox.session をentry扱いにし、session.install("...") の文字列リテラルを CHK008 の宣言として読む (parse のみで実行しない)
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
**/tests/** / root・workspace member直下の test/** / conftest.py / *_test.py / test_*.py -> test
docs/** / workspace member直下の docs/**                    -> docs
noxfile.py / root・member 直下の setup.py / 各tool設定が参照するscript (hatch の build hook 等) -> dev
scripts/**                                                  -> dev (設定で変更可)
examples/** / workspace member直下の examples/**          -> dev
package 外の notebook (.ipynb)                             -> dev
plugin / [tool.chokkin] のcontext指定が上記を上書きする
```

`examples/` は wheel に入らないデモなので dev とする (#681、#694)。notebook も手で実行する解析やデモで wheel に入らないので、root と各 workspace member の配布 package（src / 不明 layout では `src/`、それ以外は `packages` の directory。不明 layout では `__init__.py` のある directory も配布 package とみなす。llama_index の member は namespace package `llama_index` を配る）の外にあれば、上の表で runtime になるときに限り dev とする (#1023)。test / docs の notebook はその context のまま。package 内の notebook は runtime のままで、`--production` でも残す。import は CHK001 の到達性には効くが、CHK003 / CHK004 / CHK005 では dev 使用として扱う。ただし `examples/` 配下の workspace member（pydantic-ai の `examples/`）は独自 distribution の runtime code なので、member 内の位置で判定する。runtime file の `if __name__ == "__main__":` ブロック内の import は dev context (`ImportContext::Dev`) とする。直接実行したときだけ通る dev script だから。ただし `__main__.py` (`python -m pkg`) と notebook (Jupyter は全 cell を `__main__` として実行する) では通常経路なので対象外。docs / test など runtime 以外の file では file の context を優先する (#681)。

root直下の `tests/` / `test/` / `scripts/` / `docs/` に `__init__.py` があれば `tests.*` などとして first-party import に解決する (`LayoutInfo::local_packages`、#359)。context は上表のまま、flat layout の配布パッケージ候補には入れず、CHK006/CHK007 の対象にもしない。ただし参照元としては数えるため、tests/ からだけ import される symbol は CHK006 にしない (`__init__.py` の有無によらない、#410)。pytest の `testpaths` が指す root 直下の directory も同じ扱いにする (#978)。celery の `testpaths = "t/unit/"` のように、各 entry は `__init__.py` が続くかぎり上の directory まで広げ (`t/unit` → `t`、config のある directory より上には行かない)、広げた先を test context の glob にし、root 直下で `__init__.py` があれば `t.*` を first-party に解決する。suite は package 名で自分を import し、隣の `t/integration` も同じ suite だから。配布パッケージと重なる entry (pandas の `testpaths = "pandas"`) は従来どおり捨てる。flat layout の候補からは project 名で選べなかったときの推測でだけ外し、project 名と一致する package はそのまま選ぶ。entry は literal に読み、`tests/*` のような glob は展開しない。複数の entry はそれぞれ独立に広げる。

package 内の `tests/`（`pandas/tests/`）も test context とし、test context の file（pytest plugin が test root にした file を含む）はすべて CHK006/CHK007 の対象外・参照元のみとする (#490)。単数形の `test/` は root 直下と workspace member 直下だけを test とし（#571）、root の `test/` は既定 glob でも走査する（sqlalchemy の `test/` から参照される symbol を CHK006 にしないため）。`test_` 接頭辞と `_test` 接尾辞は pytest の既定どおり大文字小文字を区別し、拡張子 `.py` / `.pyi` だけを区別しない（#570）。package 内の `test/` / `testing/` は `django.test` / `sqlalchemy.testing` のように配布される API なので runtime のままにする。

本体 package のディレクトリ (`LayoutInfo::package_root`) は次の順で決める (#487)。build backend の宣言を読まずにディレクトリ名から推測すると、`lib/sqlalchemy` を持つ sqlalchemy で `examples/` を本体と取り違えるため。

1. build backend の宣言: setuptools `packages` / `package-dir` / `packages.find.where`、hatch `packages`、flit・pdm・maturin の module 指定、poetry `packages` (`from` + `include`)。宣言 path は親に `__init__.py` が続く限り遡って top-level package にする。`__init__.py` が無くても `.py` を直下に持つ宣言 path は PEP 420 の namespace package として配布するものなので package にする (locust の hatch `include = ["pytest_locust"]` は `plugin.py` だけを持つ、#977)。`.py` を直下に持たない宣言 path (`include = ["src"]`) は package を束ねるディレクトリなので package にせず、3 の heuristic に任せる。宣言が異なる root にまたがる場合は使わない
2. `[tool.uv.sources]` の `path` source のうち、project 名 (`streamlit-dev` なら `streamlit_dev` / `streamlit`) と同名で root 配下にあるもの (`streamlit = { path = "lib" }`)。その tree に 3 を当てる
3. heuristic: `src/` 配下の package、なければ root 直下と `lib/` 直下の package 候補。project 名と一致する候補を優先し、一致しなければ先頭候補を選んで `GuessedPackageDir` (候補 1 つ、`[project].name` あり) か `AmbiguousFlatLayout` (候補複数) を warning に出す (`--probe` も同じ)
4. 候補が無ければ layout 不明として `**/*.py` を走査する

`tests` / `scripts` / `docs` / `build` / `dist` / `examples` / `benchmarks` / `e2e*` などは本体候補にしない。`v1.0/` のように名前に `.` を含むディレクトリは import できないので、`packages.find` と heuristic の package 候補にしない (#569)。宣言の無い root 直下のディレクトリで、`__init__.py` が無く `.py` を直下に持つもの (宣言漏れの namespace package か、手で実行する script 置き場) は本体候補にせず既定 glob も `.py` を走査しないが、黙って落とさず `--probe` の `Unpackaged:` 行に並べる (#977)。どちらかを決める根拠が静的には無いため、warning にはしない。本体候補にしないディレクトリと import できない名前は並べない。layout の種類は `package_root` で決まる。空なら `flat`、`src` なら `src`、それ以外 (`lib` など) は `package-root` で、`--probe` の Layout 行には `package-root (root: lib, packages: sqlalchemy)` のように root を出す。module 名は `package_root` からの相対 path で決める。`src/` は tree 全体を既定 glob にするが、それ以外の `package_root` は package だけを入れる (`lib/tests` を runtime にしないため)。workspace member が namespace package を直下に持つ場合も、`e2e*` などここに挙げたディレクトリからは module 名を付けない。entry や command の target の module 名は、`package_root`、root (`python -m` や app runner は作業ディレクトリを `sys.path` に置く)、各 workspace member の順で file に解決する。どの member でもない場所の `src/` は探さない。`path` source が本体を持つ場合、その package への import は first-party だが、CHK001 では `workspace = true` source と同じくその依存を used として数える。

pytest の既定 `--import-mode=prepend` も模す (#360)。test context の file に限り、(a) その file の basedir(`__init__.py` が無ければ自ディレクトリ、あれば最上位 package の親)、(b) 自分と同じか祖先ディレクトリにある `conftest.py` の basedir、(c) `[tool.pytest]` / `[tool.pytest.ini_options]` / `pytest.ini` / `setup.cfg [tool:pytest]` の `pythonpath` を、この順で `sys.path` 先頭にあるものとして扱う。そこにある module は stdlib 以外の同名 distribution より優先して first-party に解決し、到達性でもその file へ辿る (`tests/e2e/conftest.py` の隣の `lifecycle.py` を `from lifecycle import X` で読む構成)。`addopts` に `--import-mode=importlib` があれば (a)(b) を使わず (c) だけにする。workspace member が自分の pytest 設定を持てば、その member 配下の test file には root の代わりにそれを使う (#775。pytest は member の suite を走らせるときその設定を拾うため)。member が入れ子なら最も内側の設定を使い、`pythonpath` は設定のある member のディレクトリ基準で解決する (airflow の `scripts/pyproject.toml` の `pythonpath = [".", "ci/prek", "in_container"]` は `scripts/`・`scripts/ci/prek/`・`scripts/in_container/`)。member 以外のディレクトリにある設定は読まない。

相対 import は、layout が module 名を付けない file でも解決する (#896)。django の `tests/runtests.py` が `tests/` を `sys.path` に置いて `tests/admin_views/` を top-level package として読むように、`tests/` や `scripts/` の package は runner がその親を `sys.path` に置いて使う。相対 import は自分の package の中にしか届かないので、package 名は探索した `__init__.py` の連鎖で決める (自ディレクトリから `__init__.py` のある祖先を遡り、最上位 package の親を `sys.path` 上とみなす。pytest の basedir と同じ規則)。runner がどのディレクトリを `sys.path` に置くかに依らず、届く先の file は同じになる。`__init__.py` の無いディレクトリの file、連鎖の最上位 package の名前が layout の配布 package と同じ file (`tests/acme/` と `src/acme/`。配布 package の module と取り違える)、名前に `.` を含む file は、従来どおり package context の無い相対 import として warning にする。`importlib.import_module(".x", package=__package__)` の動的 import にはこの規則を使わない。相対 import は名前が依存や stdlib と同じでも常に first-party にする (`scripts/requests/main.py` の `from .api import get` は `requests` を使わない)。到達性では、script と test context の file に限り、basedir からその file の package (basedir の直下の最上位 package) の中の module を探す。pytest の `--import-mode=importlib` でも、file 自身の package への import はこれで辿る。

`sys.path` を実行時に足す file も扱う (#719)。`sys.path.insert` / `append` / `extend`、`site.addsitedir`、`*.syspath_prepend` を呼ぶ file に限り、その file の path らしい string literal (英数字と `_-./` だけで絶対 path でないもの。`..` と `.` の segment は落とし、`"."` だけは自ディレクトリ) をすべて hint として集める。足す path は変数や別スコープで組まれることが多い (`Path(__file__).parents[1] / "utils"`) ので、呼び出しの引数に限らない。hint が指す候補は、hint で終わる探索済みディレクトリと、file のディレクトリかその祖先 (root まで) の下に hint を繋いだ実在ディレクトリ (探索外でも可)。その file、または `conftest.py` の hint ならその配下の file で、他のどこでも解決しない import root が候補ディレクトリにある module なら first-party とし、探索済みならその file へ到達性も辿る。候補に無い名前 (typo) は従来どおり CHK010。複数の候補ディレクトリに同じ module があるときは先勝ちで、その file 自身の hint、その file が import する module の hint (#779)、上の `conftest.py` の hint (発見順) の順に、hint の中では文字列順 (file に書いた順には依らない)、同じ hint に当たる探索済みディレクトリは path 順に探し、最初に見つかった file に解決する。実行時の `sys.path` の順は literal からは決まらないため、決定的な順を選んでいる。hint を足しても、それまで解決していた import が解決しなくなることはない (解決先の file は変わりうる) (#861)。Sphinx の `extensions` もこの解決で `docs/conf.py` の hint を使う (`sys.path.insert(0, os.path.abspath("../exts"))`)。`docs/conf.py` が top level で import する first-party module (`from docs.utils.conf_constants import ...`) の hint も `docs/conf.py` の hint に加える (#779)。airflow はその module で拡張のディレクトリを `sys.path` に足すため。辿るのは 1 段だけで、import 先がさらに import する module の hint は加えない。conf.py が top level で star import する first-party module (airflow の `from provider_conf import *` に当たる `devel-common/src/docs/provider_conf.py`) は、それ自身が import する first-party module の hint も conf.py の hint に加える (#1062)。star 先が `extensions` を組む定数をそこから import し、拡張のディレクトリもそこで `sys.path` に足すため。star 先は conf.py の一部として読むので、これも conf.py から 1 段と数える。Sphinx は conf.py を実行してから `extensions` の module を名前で import するので、拡張は conf.py の `sys.path` で自分の import を解決する (#1086)。conf.py の `extensions` (star 先の分を含む) の名前が conf.py の隣か conf.py の hint の候補ディレクトリの file に解決するとき、その file は conf.py の hint (conf.py が import する module から加えた分を含む) も持つ。airflow の `sphinx_exts/operators_and_hooks_ref.py` が同じディレクトリの `provider_yaml_utils` を import する形で、`conf_constants` がそのディレクトリを足す。first-party package の module として解決する拡張と、拡張がさらに import する module には加えない。zip egg を足す構成 (Django の `egg_module`) は追わない。

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

CHK002 が import を数える file は、到達可能な file に、library mode の orphan のうち wheel の公開 surface にあるもの（R-05 の再採点後も CHK001 が maybe に留まる file）を加えた集合にする (#501)。library の公開 module は利用者が直接 import しうる entry なので、script も test も持たない library で runtime 依存がすべて未使用に見えるのを防ぐ。CHK003〜CHK005 は到達可能な file だけを見る。orphan の import は、その module が読み込まれる根拠にならないため。CHK002 の `--explain` の evidence と、読めなかった入力による降格 (§11) も同じ集合を見る。verdict と別の file を見ると、evidence が verdict の根拠を説明できず、降格が verdict の読めなかった file を見落とすため (#550)。skip した orphan は CHK001 の候補にしないが、同じ再採点を `ReachabilityReport::unread` で受け、公開 surface にあれば `UnreadPublicModule` gap として distribution の CHK002 を likely 以下にする。CHK001 は下げない。公開 module が import できるのは wheel に入る module だけで、それらは公開 surface にあって CHK001 が既に maybe に留まるため。読めた公開 orphan の中で distribution に解決できない import も、同じ理由で `UnresolvedPublicImport` gap として distribution の CHK002 だけを likely 以下にする (#942)。opaque dynamic import は到達可能な file でも distribution を下げず、公開 orphan では CHK001 も下げないので gap にしない。notebook は全て entry (#514) なので公開 orphan にならず、読めない cell は到達可能な notebook の gap だけになる。

判定の優先順位を固定する。宣言されていないimportは、**lockfileの推移閉包で解決できればCHK004、できなければCHK003**とする。lockfileが存在しない場合(requirements.txtのみの環境など)はtransitive判定が不可能なため、CHK004はCHK003に縮退し、その旨をmessageに含める。「直接宣言されていない」こと自体は確かなので、縮退したCHK003もconfidenceは下げない。import元のworkspace memberが自分のlockfileを持つ場合(rootにlockfileがなく `libs/core/uv.lock` のようにmemberごとにlockfileを持つlangchainの構成)は、そのimportの推移判定にmemberのlockfileを使い、BFSの起点もmemberの宣言にする。memberのlockfileはmemberの宣言から解決されたものだから(#653)。rootのlockfileも判定に加え、強い方のevidenceを採る。rootのlockfileを辿るときも、member の import ならBFSの起点はmemberの宣言にする(memberがlockfileを持たない場合も同じ)。memberが依存する他memberは lockfile 上のpackageとしてedgeで辿れる。rootの宣言(rootのextraなど)からしか到達できないpackageは、memberを単体でinstallしたときに入るとは限らないので transitive edge の根拠にせず、lockfileに載っているだけの扱い(CHK004 Likely、optional / platform-guarded なら conditional CHK003)にする。root自身のcode(memberに属さないimport)は従来どおりrootの宣言から辿るので、rootがmemberを束ねるだけのメタパッケージか、自身のcodeを持つかで規則を分けない(#649)。CHK004のevidenceは2種類に分ける。宣言依存からlockfileのedgeで到達できる場合は「transitive edge」(Certain)、lockfileにpackageとしては載っているが宣言依存から到達できない場合は「lockにあるが未宣言」(Likely)とし、messageとexplain detailで区別する。

CHK005 は distribution 単位で 1 件にまとめ、その distribution の runtime import をすべて見てから confidence を決める。最初に見つかった import では決めない。トップレベルの import (class body を含む) が 1 つでもあれば warning / Certain。どれも関数本体の中 (`ImportRef.deferred`) なら warning / Likely。どれも optional / platform-guarded なら info / Likely とする。「既に import 済み」を確かめる分岐 (`if "x" in sys.modules:`、`sniffio.current_async_library() == "x"`) の本体にある、条件と同じ top-level module の import も optional とする。その分岐を通るときは呼び出し側がそのライブラリを入れているため (#681)。`is_x` / `is_x3` / `is_x_…` という名前の flag や述語 (`if is_psycopg3:`、`if is_sympy_expression(body):`、属性の `type_util.is_pandas_df(x)` も含む) も、最初の `_` までの `x` を import 済みと確かめる分岐とする (#781)。import root とは `x` のままか末尾の数字を除いた形で照合する (`is_psycopg3` は `psycopg`、`is_boto3_client` は `boto3`)。値がそのライブラリの型であること、またはそのライブラリを読み込めたことを確かめているため。`not` で否定した形の本体と、object 自身の機能を表す `self.` / `cls.` の属性 (mlflow の `self._is_azure_ad`) は対象外。`if not is_psycopg3:` の `else` は import 済みのときだけ通るので、そこにある `psycopg` の import は optional とする。`and` / `or` / `not` と `elif` / `else` の組み合わせは「分岐条件の読み方」(下記) に従う (#861)。関数内の import は関数が呼ばれるまで実行されず、optional な import は「入っていれば使う」ものなので、どちらも runtime 必須とは言い切れない。`--fix` の MoveToRuntime は Certain だけが対象なので、こうした依存を runtime に昇格させない (#583)。import 元の file が `ReachabilityReport::eager` に入らない場合は、トップレベルの import でも関数内と同じ扱いにする。関数内や `TYPE_CHECKING` 配下からしか読み込まれないモジュールの import は、そのモジュールが読み込まれるまで実行されないため (#610)。eager には入るが `ReachabilityReport::exposed` に入らない場合は optional と同じ扱いにする。optional な import 経由でしか読み込まれないモジュールは、無くてもパッケージが動くため (#614)。exposed には入るが `ReachabilityReport::certain` に入らない場合は、トップレベルの import でも warning / Likely とし、detail に `imported only by public modules that the package itself does not load` を添える。library の公開モジュールのうち package 自身の `__init__` も entry point も読み込まないもの（mlflow の `mlflow/otel/__init__.py`、`mlflow/data/polars_dataset.py`）は、利用者が import したときにだけ必要になる framework 連携であることが多く、runtime に移すと全利用者に入れさせてしまうため。`--fix` の MoveToRuntime はこれを移さない (#802)。origin には confidence を決めた import を出す。

sniffio の結果は変数経由の比較でも同じ扱いにする (#731)。`library = sniffio.current_async_library()` のあとに `if library == "trio":` と比べる形で、比較と同じスコープ (module / class / 関数の本体。入れ子の関数は別スコープ) がその名前に sniffio の呼び出しだけを代入している場合に限る。同じスコープにほかの値の代入や `for` / `with` / `:=` / `del` などの束縛が 1 つでもあれば、またはその名前が関数の引数なら、比較時の値が分からないので guard としない。

ホストが自分で読み込むモジュールにある、そのホスト自身の import は info / Likely とする (#731)。pytest plugin (`pytest11` entry point の target module、またはトップレベルに `pytest_*` 関数を定義するモジュール) での pytest の import と、IPython 拡張 (トップレベルに `load_ipython_extension` 関数を定義するモジュール) での IPython の import が対象。そのモジュールは pytest / IPython が動いているときにしか読み込まれないため。ホストとの照合は §7 で解決した distribution (`pytest` / `ipython`) で行うので、`_pytest` や `IPython.core` の import も含む。同じモジュールにあるほかの dev 依存の import と、ホストが読み込まないモジュールでの pytest / IPython の import は従来どおり判定し、後者が 1 つでもあればそちらが confidence を決める。

import root と同じ名前、または import root が `<名前>_` で始まる subpackage の配下にある file の import (`mlflow.pyspark` の `pyspark`、`mlflow.langchain` の `langchain_community`) と、`example_dags` subpackage (深さによらない) 配下の file の import も info / Likely とする (#781)。トップレベルの package 自身は名前の照合に使わない。`dagster` から import する `dagster_shared` のように、同じ名前で始まる兄弟 distribution は package 本体の依存だから。前者はそのライブラリを使うときにだけ読み込まれる integration、後者は airflow が `load_examples` を有効にしたときだけ読み込むデモ DAG で、どちらも opt-in なので package 本体が必要とする依存ではないため。`example_dags/` は wheel に入るので file context は runtime のままにし、CHK003 / CHK004 / CHK010 は従来どおり判定する。ほかの file に同じ distribution のトップレベル import が 1 つでもあれば、そちらが confidence を決める。

関数本体の import しかない distribution でも、同じ workspace member のどこかでそれを optional と扱っていれば info / Likely とする (#808)。optional と扱う根拠は 2 つ。1 つはその distribution の optional な import (`try:` 本体の import を `except ImportError` で受けて install を案内する、werkzeug の `cryptography`) が 1 つでもあること。platform guard だけの import は、その platform では必須という意味なので根拠にしない。もう 1 つは、その member の runtime で到達するモジュールが上の `is_x…` という名前の関数を定義し、`x` が import root に当たること (streamlit の `is_polars_dataframe` に対する `polars`)。そうした述語は import せずに値の型を調べるので、その package は入っていなくても動く前提で書かれている。述語は定義した member にだけ効き、別 member の述語と、test や到達しない file にしか無い述語は根拠にしない。どちらも無い関数内の import は従来どおり warning / Likely のまま (mlflow の `torch` / `httpx`)。また、トップレベル package の下の `test` / `tests` / `testing` subpackage (django の `django/test/selenium.py`、`django/contrib/admin/tests.py`) にある import も info / Likely とする。そこは利用者の test suite が読み込む test 支援なので、package 本体の runtime には要らないため。ただし package 自身が起動時に読み込む (`ReachabilityReport::certain` に入るトップレベルの import) なら従来どおり warning / Certain とし、`test.py` という名前の module (dbt の `task/test.py` のような `test` コマンド) は test 支援とみなさない。file context は上の配布 API の扱いのまま runtime にし、変えるのは CHK005 の強さだけ。CHK004 には関数内 import で強さを変える段階がないので、この扱いは CHK005 だけに入れる。

environment markerとextrasの扱いも定める。

```text
marker付き依存 (例: pywin32; sys_platform == "win32")
  -> 解析環境では未使用に見えても誤検知になりやすい
  -> unused判定のconfidenceを1段下げ、defaultではwarning、--strict時のみerror
  -> ただし python_version / python_full_version だけの marker が、project の
     対応する interpreter すべてで成り立つなら無条件の宣言と同じに扱う (#919)。
     対応する interpreter は requires-python と Poetry の python の共通部分
     (読めない方は使わない)。例: requires-python >=3.9 での
     foo; python_version >= "3.8"
  -> 模型にしない形 (ほかの変数、in、===、pre/post/dev/local/epoch を含む
     version、4 要素目以降が 0 でない version) を含む marker は条件付きのまま。
     常に成り立つ組み合わせでも誤って無条件にしない方を選ぶ

extra指定 (例: psycopg[pool])
  -> lockfileでextraが追加する配布物 (psycopg-pool) は、その宣言と同じcontextで
     宣言済みとみなす (CHK003/CHK004にしない)。extraはユーザーが明示的に要求したため (#516)
  -> extraを付けていない宣言 (psycopg) からは従来どおりCHK003/CHK004
  -> extraの展開を読むのはuv.lockのみ。lockfileがない場合は展開しない

stub package (types-* / *-stubs)
  -> importされないため素朴にはCHK002になる
  -> 対応するruntime packageの使用があればtype contextでused扱い
  -> runtime package自体が未使用なら、stubも併せてunused報告する

runtime peer (例: python-multipart)
  -> 他の library が実行時に遅延 import するので、project 自身は import しない
     (starletteの request.form() が python_multipart を読む)
  -> 同梱の表で対応する provider (starlette / fastapi) の使用があればused扱い
     (accelerate は transformers。device_map="auto" で transformers が読む、#984)
     transformers の model/tokenizer/audio backend、evaluate の metric backend、
     datasets の archive backend、torchmetrics の COCO backend、inspect-ai の
     provider/plugin も同じ表で扱う。project と PEP 723 の両 scope に適用する
  -> provider自体が未使用なら、peerも併せてunused報告する


metapackage (wheel target が一致する file を持たない、または hatch の bypass-selection)
  -> 配布する依存そのものが中身なので、import されなくても未使用ではない
  -> project.dependencies / extras は CHK002 の対象外 (#529)
  -> dependency group は配布しないので従来どおり判定する
```

CHK009 (duplicate_dependency) は、同じ distribution の宣言のうち片方が他方を冗長にする組だけを報告する (#494, #507, #629)。

```text
同じ context に同じ marker・同じ specifier・同じ extras で 2 回 (dependency-groups.test に pytest が 2 つ)
  -> CHK009 "pytest is declared more than once in group:test"
  -> specifier が違う場合 (pytest>=7 と pytest>=8) は両方が制約なので報告しない
  -> extras が違う場合 (httpx と httpx[http2]) も片方を消すと extras を失うので報告しない
runtime 宣言と group / optional extra / build の宣言
  -> CHK009 "requests is declared in multiple contexts: group:dev, runtime" (info / likely、--fix しない)
  -> group は単独で install されることがあり (CI の uv sync --only-group lint)、extras は
     自動生成で他 package の extras を写すこともある。どちらも静的には区別できないので、
     group 名で分けず一律に info にする (#696)
  -> 同じ distribution に同じ context の重複もあれば、その重複だけを warning / certain で
     報告する (runtime との重複は message に含めない)。--fix が消すのもその重複だけ
  -> group / extra 側が runtime 宣言から導けるときだけ報告する。次の場合は refinement なので報告しない
     - runtime 宣言に無い extras を足す (runtime: streamlit、dev: streamlit[auth,charts])
       (extras は PEP 503 正規化で比較し、同じ marker の runtime 宣言すべての extras の和集合に
       含まれるかで見る。runtime: celery と celery[redis]; sys_platform == 'linux' に対する
       dev: celery[redis] は報告しない)
     - marker が runtime 宣言と違う (runtime: requests>=2; python_version >= '3.10'、legacy: requests>=2)
     - specifier が空でも runtime 宣言と同じでもない (runtime: click>=7、mcp: click!=8.3.0。
       runtime: rich、lint: rich>=13.0)。狭い・緩いの PEP 440 包含判定はしない
       (runtime より緩い pydantic>=2.0 も報告しない)
  -> specifier は `,` で区切った各 specifier の集合で比べる (`<9,>=7.0` と `>=7.0, <9` は同じ)。
     marker は記述どおりの文字列で比べる
  -> Poetry の依存表は version 制約 (文字列値か table の `version`) を specifier として比べる。
     PEP 440 として読めるものは PEP 508 側と同じ形に正規化し、`^` / `~` などは記述どおり比べる
     (PEP 440 に変換しない)。複数制約の配列は各 version を `||` でつなぐ。
     `*` と version の無い path / git 宣言は空とする (#682)。marker は `python` / `platform` /
     `markers` から作った PEP 508 の形 (§4) で比べる
extra 同士 (s3 と sqs の両方に boto3)、group 同士 (dev と test の両方に pytest)、group と extra、build と group
  -> 報告しない。extra は独立に install されるものなので両方に必要なら両方に書く
project 自身への参照 (all = ["pkg[s3,sqs]"])、marker が異なる宣言、opaque な宣言
  -> 報告しない
同じ origin (file:line) を別経路で 2 回読んだもの
  (requirements-dev.txt の -r requirements.txt と、直接読む requirements.txt)
  -> include 側の context が違っても 1 つの宣言なので報告しない
  -> --fix は行ごと消すので、消してよいのはその行を読むすべての context に別の行の重複が
     ある行だけ。requirements.txt と requirements-dev.txt が両方 -r base.txt し、
     requirements.txt にも requests がある場合、runtime の重複として報告するが、base.txt の
     行を消すと requirements-dev.txt だけの install から消えるので requirements.txt 側を消し、
     issue の位置もその行にする。
     消せる行が 1 つも無ければ context 間の重複として info / likely で報告する (#860)
setup.py で評価したリストに同じ文字列が 2 回入ったもの
  (extras["dev"] = extras["all"] + extras["testing"] で両方に datasets)
  -> 1 つの宣言として読むので報告しない (#726)。setup.py の要素は行を持たず
     連結と literal の書き重ねを区別できないため、文字列が完全一致する要素は一律にまとめる
```

origin と `--explain` の details には重複に関与した宣言だけを出す。`--fix` もこの関与した宣言の中からだけ削除対象を 1 つ選ぶ (runtime と他 context の組では runtime 宣言を残す)。同じ distribution を別の制約で持つ兄弟の extra (http: httpx と pinned: httpx!=0.28.0 のうち pinned) は消さない。context の label は `runtime` / `group:<name>` / `optional:<extra>` / `build` で、setup.py の `extras_require` も `optional:` にそろえる。

`TYPE_CHECKING` 配下のimportはtype contextにする。`import typing as t` や
`from typing import TYPE_CHECKING as TC` の alias も静的に追跡し、runtime dependency
としては扱わない。

```python
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pandas import DataFrame
```

この場合、`pandas` がruntime依存にある必要はない。ただし、type checkerを実行する環境で必要なら `type_groups = ["types", "mypy"]` 側にあるべき、という判断にできる。v0.4 では CHK003 の default 表示を runtime import に絞り、type/test/docs/dev context の未宣言 import は `--strict` 時だけ報告する。

`try import` はoptional扱いにする。

```python
try:
    import orjson
except ImportError:
    orjson = None
```

`try` の `else:` 節の import も optional とする(`else:` は try 本体が例外なく通ったときだけ実行されるため)。module トップレベルの try 本体で `has_x = True` のように `True` を代入した名前は flag として覚え、module トップレベルの `if has_x:` 本体の import も optional とする(関数内の `if` は同名の局所変数があり得るので対象外)。`except` / `finally` 節の import は optional にしない (#580)。失敗した import を捕まえる `except` 節とは、`ImportError` / `ModuleNotFoundError` かその基底の `Exception` / `BaseException` を名前(`builtins.Exception` のような属性も可)で指すもの(タプル指定はどれか 1 つを含めばよい)と、型を書かない素の `except:` を指す。それ以外の名前や名前でない式(`except errors:` のような変数)は捕まえないものとして扱う (#967)。`try:` 本体と `with suppress(ImportError):` の中で定義した関数(`lambda` も)の本体は呼ばれたときに実行され、そのとき外側の `try` はもう例外を捕まえないので、その中の import は外側の `try` / `with` を理由には optional にしない。クラス本体と `lambda` の引数の既定値は定義時に評価されるので外側を引き継ぐ。`else:`・flag や availability check の分岐・`raise ImportError` の後の文の中で定義した関数は、package が入っているときだけ定義されるので、その本体の import も optional のままとする。`except` 節の中で定義した関数の import も、節と同じく fallback として扱う (#968)。失敗した import を捕まえる `except` 節で `x = None` のように `None` を代入した名前も同じ flag とする。ただし素の `except:` と `Exception` / `BaseException` は本体の他の失敗も捕まえるので、try 本体に import 文があるときだけ flag にする(`try: token = os.environ["TOKEN"]` / `except Exception: token = None` の `token` は flag ではない)。flag は `x` を「package が無ければ偽」の条件として読む。`None` を代入した flag は `x is not None` も偽、`x is None` を真として読み、`True` を代入した flag の `is` / `is not None` は読まない(package が無ければ `False` で、`None` ではないため)。`not` / `and` / `or` と `elif` / `else` は下の「分岐条件の読み方」に従う。`if x:`、`if x is not None:`、そのどちらかを含む `and` 連鎖の本体と、`if not x:` / `if x is None:` の `else` は optional とする。`or` 連鎖、`not x`、`x is None` の本体と、`if x:` の `elif` / `else` は package が無くても実行され得るので optional にしない (#730、#861)。

package が入っているかを確かめる分岐も optional とする。`importlib.util.find_spec(...)` か `is_<name>_available()` (先頭の `_` は可。transformers の `is_torch_available()` など)を「入っている」側で確かめる `if` / `elif` の本体(`find_spec(...) is not None`、`is_x_available() and ...`)、「入っていない」側で確かめる分岐(`not is_x_available()`、`find_spec(...) is None`、`not a or not b`)の後に続く `elif` / `else`、`if not is_x_available(): raise ImportError(...)` (`ModuleNotFoundError` も可)の後に続く同じ body の文(条件は `if` 本体と同じく下の「分岐条件の読み方」で読み、package が無ければ必ず真になる条件だけを数える。module トップレベルなら `if not HAS_X:` のような try の flag も含む。#958)、そうした呼び出しを含む関数の本体のうち、最初の呼び出しを含む文から後ろにある import (入れ子の関数・クラス・lambda 内の呼び出しは数えない。`import_module(x) if find_spec(x) else None` のように同じ文の中なら前でもよい。そうした関数の中で定義した関数は check の後に呼ばれ得るので、その import は位置によらず optional)がこれにあたる。`find_spec` は bare の `find_spec(...)` と `importlib.util.find_spec(...)` だけを数え、`finder.find_spec(...)` のような他オブジェクトの method は数えない。`is_<name>_available` も bare 呼び出しか module 属性(`utils.is_torch_available()`)だけを数え、`self.` / `cls.` から始まる method(クラス自身の機能の確認)は数えない (#713)。`if is_x_available(): import x else: import fallback` の fallback や `if not is_x_available(): import fallback` の本体、package が入っていても真になり得る条件(`not HAS_X and strict` のように読めない葉を `and` でつないだものなど)や読めない条件の `raise ImportError` の後の import は optional にしない。`torch.cuda.is_available()` のように package 名の入らない `is_available()` は対象外 (#695)。

この場合、未宣言でも即 `missing_dependency` にはしない。`orjson` がoptional extraにあるならOK、main dependencyにあるならOK、どこにもなければ conditional CHK003 candidate としてdefaultはinfo、`--strict` 時はwarningにする。`sys.platform` 分岐配下の未宣言 import も同じ扱いとし、message では optional try-import と platform-guarded import を区別する。

**分岐条件の読み方** (`src/parser/condition.rs`、#861)。`if` / `elif` の条件は、`not` / `and` / `or` の下にある式を葉として読む。type context、`__main__` block、platform-guarded、optional の判定はそれぞれ、葉ごとに「その判定の前提では常に偽」「常に真」「判定の側で値を選べる」のどれに当たるかだけを知る。どれにも当たらない葉はどちらの値も取り得るものとする。分岐の本体が前提のもとで決して通らないと示せたときだけ、その判定を本体に付ける。`elif` / `else` は前の条件がすべて偽のときに通るので、前の条件のどれかが前提のもとで真になり得るなら、その分岐も同じ判定にする。

- type context: `TYPE_CHECKING` は実行時に常に偽。`if TYPE_CHECKING and x:` の本体や、`if not TYPE_CHECKING:` の `else` も type context にする
- `__main__` block: `__name__ == "__main__"` と `TYPE_CHECKING` は import されたときに常に偽
- platform-guarded: `sys.platform` の比較は platform を選べば偽にできる。`if sys.platform == "win32":` の `elif` / `else` もその platform では通らないので platform-guarded にする。platform の検査として読む葉は次の 3 種類 (#957)。
  - `sys.platform`、`os.name`、`platform.system()`（引数なし）を `==` / `!=` / 大小比較 / `in` / `not in` で比べる式
  - `sys.platform.startswith(...)`（引数あり）

  受け手の名前は、その scope のすべての binding が同じ stdlib module の `import`（`import sys`、`import os.path`、`import sys as s`）であるときだけ数える。関数の引数や代入、`from x import sys`、`except ... as sys`、`match` の捕捉（`case sys:`、`case [*sys]`、`case {**sys}`、`case X() as sys`）、同名の `def` / `class` で束縛し直した名前は値の分からない葉とする。`from x import *` と、関数の中で `global sys` を宣言して束縛し直す形は追わず、`import sys` のまま数える。関数の中は外側の scope の名前を引き継ぎ、関数自身の引数と binding で上書きする。class 本体の binding もその中の関数に引き継ぐので、class 本体で束縛し直した名前はメソッドの中でも数えない（実行時には global を見るが、数えない側に倒す）。`platform.system().lower()`、`os.uname()`、`sysconfig.get_platform()` などは認識しない式として値の分からない葉になる
- optional: availability check と flag は、package が無いときの値が決まる。「import 済み」を確かめる判定は、確かめた module の import にだけ効く

それぞれの判定は自分の葉しか知らない。optional と platform-guarded の判定では `TYPE_CHECKING` も値の分からない葉なので、type-only の import は optional にも platform-guarded にもならず、CHK003 の扱いは type context の規則で決まる。認識しない式（`sys.version_info` の比較、`os.environ` の参照、任意の関数呼び出し）も値の分からない葉で、判定を付ける根拠にならない。読めない条件は import を runtime 必須のまま残す（ADR 0005）。`if sys.version_info >= (3, 11): import tomllib else: import tomli` はどちらも runtime import のままになる。同じ式が 2 回出る条件も、別々の葉として読む。値の分からない葉が重なると判定は付きにくくなる。`sys.platform == "a" or sys.platform != "a"` のように platform の比較が重なると、実際には常に真でも platform-guarded になる。

ただし lockfile の edge で宣言依存から推移的に到達できる場合は、optional / platform-guarded でも CHK004（transitive edge、Certain）を優先する。`try:` で包むだけで「推移依存への直接 import」の指摘が消える抜け道を作らないため (#504)。lockfile に載っているだけで edge で到達できない場合は根拠が弱いので、conditional CHK003 のままにする。ただし optional / platform-guarded の import は「入っていれば使う」ものなので、この CHK004 の severity は error ではなく warning とする(`--strict` でも同じ)。confidence は lock edge の証拠なので Certain のまま (#582)。

`[build-system].requires` / `build-backend` は build context (v0.5 R-05) として `ProjectMetadata.build_requires` / `build_backend` に inventory し、CHK002/CHK003 の宣言集合には入れない (build requires は runtime import を満たさず、未使用扱いにもならない)。hatch-vcs / setuptools-scm のような build plugin が project / dev 依存にも宣言されて未使用なら、CHK002 を出すかどうかは既存の context 方針のままにし、evidence に `also in build-system.requires` を添える。`--probe` の Manifest 欄に backend と requires を表示する。ただし root の build requires の名前は、§7 のどの段でも解決できなかった import root を third-party とみなす根拠にだけ使い(`extra_requirements` と同じく完全一致または §7 step 8 の接頭辞・接尾辞の一致)、CHK010 を出さない。sqlalchemy の `if cython.compiled:` 配下の `from cython.cimports... import` のように、build 環境にだけある module を import するため。distribution は付けないので CHK003–CHK005 には使わない (#720)。

PEP 723 inline script metadata (v0.5, R-02) は script 単位の dependency scope として扱う。

```text
検出   : probe (step 4) で source の .py を file text から走査する (AST は使わない)
         `# /// script` 〜 最後の `# ///` を TOML として読む。CRLF 可
         block が 2 つ以上 / TOML が壊れている / 型が違う -> warning、通常 source 扱い
entry  : script file 自体を entry root (origin: script) にする。script から到達する
         first-party file は CHK001 にならない。--production では production context の
         script だけを entry にする
照合   : script file 自身の third-party import は script block の `dependencies` と照合し、
         project manifest の CHK002/CHK003/CHK005 判定からは除外する
         script 経由の first-party import とその先の file は従来どおり project scope
target : script の `requires-python` 下限を、その file の parse と stdlib 判定の
         target version にする (stdlib 判定の上限も script の `requires-python`)。
         未指定なら project の target version
report : subject は `script:<path>:<distribution>`。CHK003 は Error/certain
         (optional / platform-guarded import は対象外)。CHK002 は project CHK002 と
         同じく marker 付きを default で除外し、--strict で報告する
         (script の `requires-python` で常に成り立つ marker は無条件、#919)
         既知の runtime peer と stub は project scope と同じく provider の使用で
         used 扱い。subprocess を import する file の command string と literal な
         argv list/tuple の先頭も tool の使用として読む。計算した program 名は
         使用の根拠にしない。provider 自体が未使用なら peer も未使用のまま
         CHK002 は一律 likely (default は warning、--strict で error)。block は
         `uv run` が作る環境の宣言で、script が import しない runtime peer (marimo
         notebook の polars / duckdb、transformers の torch) を並べるため (#984)。
         extra 名やコマンド名が単にソースの文字列に現れるかで confidence を
         変える判定は入れない。
         動的に組んだ名前や別 process の起動を拾えず、拾えなかった宣言を
         certain のまま残すため。optional extra は default で対象外のまま、
         --strict では certain (--fix で消す) のままにする
         到達しない script (--production で dev context 等) は判定しない
fix    : script block は書き換えない。--fix は UnsupportedTarget で skip する
cache  : block は file text の一部なので parse cache の file fingerprint で無効化される
         (target version も file text から導出するため key 不整合は起きない)
```

## 11. unused files判定

unused filesはimport graphの到達可能性で判定する。

```text
entry roots
  -> import edges
  -> config/plugin edges
  -> reachable files

project files - reachable files = unused file candidates
```

`from pkg import name` は `pkg` に加えて、`pkg.name` が first-party module に解決できればその file への import edge としても辿る（Python は `name` が submodule ならそれを読み込むため）。相対 import の `from . import name` と同じ規則になる。

`import pkg.sub.mod` は `pkg.sub.mod` の file に加えて、親 package の `pkg/__init__.py` と `pkg/sub/__init__.py` にも到達させる（Python は submodule を import する前に親 package を初期化するため）。dynamic import の literal と plugin の module reference にも同じ規則を適用する。

framework glob に一致する file（Django の `migrations/**/*.py` など）は framework-used の root として扱う。entry roots からの走査が終わった後にこれらを queue に入れ、その import も辿る。entry から既に到達している file の trace は上書きしない。

ただし、以下はデフォルトで除外または低confidenceにする。

```text
__init__.py
*.pyi
migrations/**/*.py
tests/**/*.py
docs/conf.py
generated files
vendored code (`vendored` 設定、§5)
namespace package fragments
plugin-marked files
```

test tree（`tests/` と root の `test/`）の配下で、path に `data` / `fixtures` / `testdata` / `test_data` の directory を含む file はテストの入力データ（black の `tests/data/cases/*.py` など）とみなし、到達しなくても CHK001 にしない（`sources::is_test_data_path`、#593）。test から import されていれば通常どおり到達扱いになる。除外するのは CHK001 の候補だけで、`--strict` では従来どおり報告する。

`unused_file` のconfidenceはこう決める。

```text
certain:
  app modeで、明確なentryから到達せず、dynamic importもなく、plugin対象でもない

likely:
  dynamic importはあるが対象module名と一致しない
  (到達可能なfileに対象module名を静的に決められないdynamic importが1つでもあれば、
   全unreachable fileをlikelyにする。unreachable file自身のdynamic importは
   実行されないので判定に使わない)

maybe:
  library mode、namespace package、wildcard importがある
```

デフォルトでは `confidence = "likely"` 以上だけ表示する。`--confidence maybe` で全部出す。

module top-level（関数の外）で `warnings.warn` を呼び、第2位置引数か `category=` が `*Deprecat*` / `Removed*Warning` に当たる名前の module は、import 時に自身の非推奨を告げる互換 shim とみなす (`src/parser/deprecation.rs`、#778)。airflow の `stats.py` (`DeprecatedImportWarning`) や `security/permissions.py` (`RemovedInAirflow4Warning`) のように外部の利用者のために残しているので、この file の CHK001 と、この file で定義された symbol の CHK006 は info にする。category は AST 上の名前（`exc.RemovedInAirflow4Warning` なら末尾）だけで判定し、class は解決しない。`warn` は module top-level の `import warnings`（別名を含む）か `from warnings import warn` で束縛したものに限る。関数内の `warn` はその関数の非推奨なので対象外で、`if` などの block 内の `warn` も条件付きの非推奨なので数えない。

confidenceとseverityは独立した2軸として扱う。severityはissue種別の重み(error / warning / info)、confidenceは解析の確からしさ(certain / likely / maybe)。表示filterは `confidence >= 設定値`、exit code 1の対象はdefaultで `severity >= error` かつ `confidence >= likely` にする。`--strict` ではwarning以上かつmaybe以上まで広げる。

読めなかった入力による降格は 1 か所 (`src/rules/gaps.rs`、ADR 0005 の降格段) にまとめる。gap は「どの file の何が読めなかったか」を表す値 (`Gap { file, kind, member, site, target }`) で、pipeline が parse・reachability・manifest・plugin から集め (`collect_gaps`)、emit が ignore・severity・confidence filter の前に finding へ当てる。gap の種類ごとに、どの finding を下げるかを 1 つの表で決める。

```text
到達可能な file を skip した (UnreadModule)            -> CHK001 と distribution の CHK002 を likely 以下に
discovery に無い entry を skip した (SkippedEntry)           -> CHK001 と distribution の CHK002 を likely 以下に
どこからも到達しない library の公開 module を skip した (UnreadPublicModule) -> distribution の CHK002 を likely 以下に
到達可能な notebook の一部の cell を読めなかった (UnreadNotebookCells) -> CHK001 と distribution の CHK002 を likely 以下に
到達可能な file に opaque dynamic import (OpaqueDynamicImport) -> CHK001 を likely 以下に
runtime 依存が読めない (RuntimeDependenciesUnknown)        -> import の CHK003 / CHK004 と distribution の CHK005 を info / maybe に
extras を読み切れない (UnknownExtras)                     -> import の CHK003 / CHK004 と distribution の CHK005 を likely 以下に
名前の読める invalid requirement の行がある (InvalidRequirement) -> その名前の distribution に解決した import の CHK003 / CHK004 を info / maybe に
runtime 側 (runtime・extra・runtime group) の名前の読める invalid requirement の行がある (InvalidRuntimeRequirement) -> 上に加えて、その名前の distribution の CHK005 を info / maybe に
requirements ファイルに file 名から名前を読める archive の行がある (ArchiveRequirement) -> その名前の distribution に解決した import の CHK003 / CHK004 と、root を正規化するとその名前になる未解決の import の CHK010 を info / maybe に
runtime 側の同じ行がある (ArchiveRuntimeRequirement) -> 上に加えて、その名前の distribution の CHK005 を info / maybe に
到達可能な file の import を distribution に解決できない (UnresolvedImport) -> distribution の CHK002 を likely 以下に
どこからも到達しない library の公開 module の import を distribution に解決できない (UnresolvedPublicImport) -> distribution の CHK002 を likely 以下に
PEP 723 script の import を distribution に解決できない (UnresolvedScriptImport) -> その script の CHK002 を likely 以下に
task file / tool config の command が、どの entry も届かない Python code を実行する (UnreadCommandTarget) -> CHK001 を likely 以下に
Django project の settings を見つけられないか読めない (UnreadDjangoSettings) -> CHK001 と distribution の CHK002 を likely 以下に
Sphinx の `extensions` を読み切れない (UnreadSphinxExtensions) -> CHK001 と distribution の CHK002 を likely 以下に
root の uv path source の directory に `[project]` を持つ `pyproject.toml` が無い (UnreadPathSource) -> その directory の下の file の CHK001 を likely 以下に、その下の import の CHK003 / CHK004 と、その下から import された distribution の CHK005 を info / maybe に
同じ directory の下の file が distribution を import する (UnreadPathSourceImport) -> その distribution の CHK002 を likely 以下に
root より下の Dockerfile の server app を、root からは解決できず Dockerfile の directory から解決した (UnreadImageTree) -> その directory の下の import の CHK003 / CHK004 / CHK010 と、その下から import された distribution の CHK005 を info / maybe に
`requires-python` を読めず py311 を仮定した (AssumedTargetVersion) -> その file の、範囲外の version だけが stdlib に持つ module の import の CHK003 / CHK004 / CHK010 と、その import を持つ script の CHK003 を info / maybe に
PEP 723 script の `requires-python` を読めず project の範囲で読んだ (AssumedScriptTargetVersion) -> その script の、同じ module の import の CHK010 と script の CHK003 を info / maybe に
PEP 723 script の block に名前の読める invalid な `dependencies` 行がある (InvalidScriptRequirement) -> その script の、その名前の CHK003 を info / maybe に
project の file を dotted name で名指しする文字列 literal がある (StringModuleRef) -> その file の CHK001 を info / likely に
```

解決できない import の gap は、CHK010 が warning で出す site と同じ条件で集める (#911)。到達可能な file にあり、`TYPE_CHECKING` の下でも optional import でもなく、file context が docs / dev でなく、first-party でもない import root が、どの distribution にも解決できないとき 1 件にする。import map で解決できない module は、宣言済みのどれかの distribution が提供しているかもしれない。その distribution の CHK002 (unused) は「使っていない」と言い切れないので certain から外す。どの distribution かは分からないので、root manifest の CHK002 をすべて下げる (CHK002 は root manifest だけで判定するため member では絞らない)。PEP 723 script の import は、script の block が環境を決めるので、その script の block の CHK002 だけを下げ、file context を問わず数える。script が import する同じ project の helper module の解決できない import は project 側の gap になり、script の block は下げない。`site` は `--explain` が file の代わりに出す module と行で (`` `zz_widgets_core` in `acme/main.py:1` ``)、同じ file の同じ module は最初の行の 1 件にまとめる。CHK010 自体は変えず、この gap を利用者に届ける warning として使う。

task file と tool config の command (Makefile・justfile・Dockerfile・Procfile・GitLab CI・PDM scripts・poe tasks・hatch scripts・pre-commit の local hook・tox の commands・`scripts/` と `bin/` の shell script) が実行する Python code のうち、script・`-m` の module (#912) と server の app (#1004) は entry にする。それ以外（task runner が読み込む module、`-c` の code が import する module、import root を動かした `-m`）と、`--production` で entry が落ちた dev の command の target から届く file の CHK001 は「どこからも使われていない」と言い切れないので、その command を gap にする (#913)。command から読む形は `python X.py` (`uv run` と `$(PYTHON)` などの変数を含む)、`python -m pkg.mod`、`python -c '...'` の `import` / `from`、`gunicorn` / `uvicorn` / `hypercorn` / `daphne` / `waitress-serve` / `granian` の `module:app` と `celery -A app`。path や module に `$` `{` `}` `%` `*` を含む computed な形、`-c` の code が `__import__` / `importlib` / `runpy` / `exec(` を使うか変数で渡される形、`python -` (stdin)、app を読めない runner、`cd`・`--app-dir`・`PYTHONPATH=` などで import root を動かした server の app は opaque として必ず gap にする。読めた script と module は、project の file に解決できて、どの entry からも届かないときだけ gap にする。server の app は解決できれば entry になるので gap にしない（走査から外れた file は entry path の warning になり、`--production` で落ちた dev context の file は解析の対象に無い）。module は `pkg.mod.__main__`、`pkg.mod` の順で解決する。解決できないもの (typo、`python -m json.tool` のような third-party や stdlib) は project の file を実行しないので gap にしない。ただし datamodel-codegen の formatter 名は、解決できないときも、同じ名前の file が複数あるときも gap にする (#1063)。tool は実行した directory の `sys.path` から import するが、その directory は config に書かれないので、inventory の外の file や別の同名 file が formatter で、project の file を import しているかもしれないため。script の path は command の `cd` と file の場所から組み、絶対 path (Dockerfile の `/app/x.py`) は末尾の component 列が project の file に一致するものを探し、root より上に出る `..` は捨てる。範囲は OpaqueDynamicImport と同じ project 全体の CHK001 にする。entry にしない command がどの file に届くかは分からないため。workflow の `run:` は対象外。`site` は command の text と行で、同じ file の同じ text は最初の行の 1 件にまとめる。root より下の Dockerfile は server の app しか読まないので UnreadCommandTarget を作らない (#1061)。その app を root から解決できず Dockerfile の directory から解決したときは、その directory を image 自身の tree とみなして gap にする (UnreadImageTree)。image は自分の requirements を `RUN pip install` で入れ、`COPY` / `WORKDIR` で import root を作るが、どちらも読まないので、その下の import の CHK003 / CHK004 / CHK005 は「宣言していない」と、CHK010 は「解決できない」と言い切れない。airflow の informatica simulator は自分の `requirements.txt` で `fastapi` を入れるが、file が entry から届くようになると informatica provider の manifest に対して CHK004 になっていた。範囲を directory の下に絞り、root から解決した app (`CMD gunicorn acme.wsgi`) を対象にしないのは、それが root の code で root の宣言で動くため。CHK001 は下げない。image の entry は server の app だけを読み、それ以外の file は image の中でも届かなければ使われていないため。image の requirements を宣言として読む案は採らない。どの requirements を入れるかは `RUN` の shell を読まないと決まらず、root の manifest とは別の宣言の組を持つことになるため。`site` は app の module と `CMD` の行 (`` `app.main` in `dev/sim/Dockerfile:4` ``)。

root の `[tool.uv.sources]` の path source のうち root 内のものは §5 で member になるが、`[project]` を持つ `pyproject.toml` が無い directory (`setup.py` だけの sub-project など) は member にならない (#1000)。その下の code は root の inventory として解析されるが、uv はその directory を別の distribution として install するので、そこの entry (console script や `__main__`) も依存の宣言も読んでいない。その下の file の CHK001 は「どこからも使われていない」と、その下の import の CHK003 / CHK004 / CHK005 は「宣言していない」と言い切れないので、directory を gap にする (UnreadPathSource)。範囲を directory の下に絞るのは、root の entry と宣言は読めているため。宣言の欠落を hint にするのは、directory の宣言の一覧全体を読んでいない点で RuntimeDependenciesUnknown と同じだから。root の宣言の CHK002 は、directory の下の file が import する distribution だけを下げる (UnreadPathSourceImport)。root の entry から届かない file の import は使用に数えないが、読んでいない directory の entry から届くかもしれないため。他の distribution は directory の下のどの file も import しないので、directory の entry を読んでも使用は増えず、下げない。directory そのものが無い path source (`missing-dir`) は解析する code も無いので gap にせず、`does not exist` の manifest warning だけを出す。root 自身を指す path source と archive (`dist/foo.whl`) は gap にしない。member になった path source と、`[project]` を持つが member にならない directory (§5 の入れ子の project として root の走査が除く) も対象外。warning は path source ごとに 1 回、directory の code を root のものとして解析したことと finding を下げることを添える (`` manifest: `sub` (`tool.uv.sources.subpkg` in `pyproject.toml`) has no `pyproject.toml` with a `[project]` table, so its code is analyzed as the root's without its own entries or dependencies; findings under it are lowered ``)。`site` は UnreadPathSource では出さず (directory を file として出す)、UnreadPathSourceImport では distribution 名と最初の import の行 (`` `click` in `sub/tools/gen.py:1` ``)。

Django の settings は、`INSTALLED_APPS` の app、`MIDDLEWARE`、`ROOT_URLCONF` の URLconf を通じて、project の file と distribution を読み込む。settings を読めない project では、その file の CHK001 も、app を提供する distribution の CHK002 も言い切れないので、Django plugin が記録した読めなかった settings を gap にする (#914、条件は §9)。範囲は project 全体の CHK001 と distribution の CHK002 で、member では絞らない。settings はどの member の app も読み込みうるため。`site` は見つからない settings module (`` `mysite.settings.prod` in `manage.py:3` ``)、辿れない star import (`` `configurations` in `mysite/settings/dev.py:1` ``)、読まない呼び出し (`` `include()` in `mysite/settings/__init__.py:3` ``)、literal でない設定 (`` `DJANGO_SETTINGS_MODULE` in `manage.py:3` ``) で、settings が何も無いときと読めないときは file だけを出す。

Sphinx の `extensions` は、conf.py が読み込む拡張として project の file と distribution を読み込む。conf.py (§19 の Sphinx の conf.py すべて) か、それが star import して読んだ module が `extensions` を読まない形で組むとき、その拡張の file の CHK001 も distribution の CHK002 も言い切れないので gap にする (#1062)。読まない形は、文字列でも文字列 list / tuple の literal でも、それを束ねる名前でもない値 (`EXTS + ["a"]`、comprehension、関数呼び出し) と、読めない要素を含む list / tuple。top level の `extensions` への代入・`append` / `extend` / `insert`・`+=` がこれにあたる。辿らない star import (third-party か存在しない module、star 先がさらに star import する module) も gap にするが、conf.py か読んだ star 先が `extensions` を代入していれば gap にしない。代入は star import の値を置き換えるため。`if` の下など top level 以外の文は読まず、gap にもしない (airflow の `provider_conf.py` は `PACKAGE_NAME` が fab のときだけ swagger 拡張を足す。build ごとに環境で変わる分岐を gap にすると airflow の provider docs 全体が下がる)。範囲は UnreadDjangoSettings と同じ project 全体の CHK001 と distribution の CHK002 で、member では絞らない。拡張は `sys.path` の hint でどの member の file も読み込みうるため。`site` は読まない形のある file と行 (`` `extensions` in `docs/conf.py:3` ``、star 先なら `` `extensions` in `docs/shared_conf.py:5` ``) か、辿らない star import (`` `sphinx_base_conf` in `docs/conf.py:1` ``)。多くの conf.py が star import する module の gap は site ごとに 1 件にまとめる。

module を dotted name の文字列で名指しして実行時に読み込む形は、entry にも import edge にもしていない (#1026)。airflow の `lazy_load_command("airflow.cli.commands.cheat_sheet_command.cheat_sheet")` や gunicorn の `"config": "python:airflow.api_fastapi.gunicorn_config"` がそれで、名指しされた file の CHK001 は「どこからも使われていない」と言い切れないので、その文字列を gap にする (StringModuleRef)。読む形は、文字列 literal 全体が identifier の segment を `.` でつないだ 2 つ以上の dotted name であるもので、先頭の `python:` は外す。dotted name の後に `:attr` が付く形 (celery の `BACKEND_ALIASES` の `"celery.backends.redis:RedisBackend"`、cyclopts の lazy command の `"pkg.cli.deploy:app"`) も読み、`:` より前を module 名にする (#493)。1 segment の `"json"`、`:` の前が 1 segment の `"main:app"` や `"localhost:8080"`、path、文は読まない。f-string や名前との連結は literal でないので見えず、その module の finding は下げない（recall の不足で、silence ではなく certain のまま報告する）。文字列の名前は `pkg.mod.attr` のように末尾に属性を含むことがあるので、名前そのもの、無ければ末尾の segment を 1 つずつ外した 2 segment 以上の接頭辞の順に project の file に解決し（`pkg` だけには落とさない）、解決できない名前 (third-party・stdlib・typo) は project の file を読み込まないので gap にしない。文字列はどの file にあっても数える（entry から届かない file の文字列も、その file が読まれたときには動くため）が、自身を名指しする文字列と、既にどれかの entry から届く file を名指しする文字列は gap にしない。範囲はその file の CHK001 だけで (`target`)、OpaqueDynamicImport と違い名指しが file を 1 つに決めるため。文字列が確かにその file を読み込むとは限らない（設定の既定値や log の文言かもしれない）ので、likely 以下に下げるだけでなく hint にする。import edge にしないのはそのためで、名指しされた file が import するものは到達に数えず、その file の中の finding も変えない。`site` は文字列そのものと最初の行で (`` `acme.commands.cheat_sheet.run` in `acme/cli.py:2` ``)、同じ file の同じ文字列は最初の行の 1 件にまとめる。

loader API が最初の位置引数の文字列で名指しした module は、`importlib.import_module` と同じ動的 import にする (#493)。loader API は werkzeug / django の `import_string`、celery / kombu の `symbol_by_name`、uvicorn と DRF の `import_from_string`、scrapy の `load_object` と、`pkg.mod:attr` (`:` の前が 2 segment 以上) を渡したときの cyclopts の `command`、それに `module_loaders` 設定に書いた関数 (§5) で、呼び出しの関数名 (属性呼び出しなら属性名) だけで判定する。airflow の `import_string` のように project が同名の関数を自前で持っても同じ意味なので、import 元は見ない。`pkgutil.resolve_name` は host 名を解く関数 (`resolve_name("google.com")`) と同じ名前なので組み込みにせず、`module_loaders` に任せる。`command` に `:` の前が 1 segment の文字列 (`"db:migrate"`) を読まないのは、click や typer の command 名がその形をとりうるため。文字列は `pkg.mod.Cls` か `pkg.mod:Cls` (`pkg:Cls` も可) の形で、名前そのもの、無ければ末尾の segment を外した接頭辞の順に解決するのは StringModuleRef と同じだが、こちらは import edge にして名指しされた file を到達させる。loader は module を import した後に残りの segment を属性として取り出すので、`pkg.mod.Cls` の `Cls` を `pkg.mod` の symbol の使用に数え、CHK006 にしない。解決できない名前は CHK010 にしない。werkzeug や pydantic の test は loader が失敗する名前 (`"os:foobar"`、`"a.aa"`) をわざと渡すため。`:` の無い 1 segment の名前 (`"pkg"`)、literal でない引数、`dotted_path=` などのキーワード引数、`loaders[0](...)` のような名前でない callee は読まない。その file は §11 の StringModuleRef と同じ規則で hint になるか、どこからも名指しされなければ unused として報告する。非 literal の引数は opaque dynamic import にしない。Django の `import_string(settings.X)` のように設定を経由する形がよくあり、project 全体の CHK001 を下げると gap が広すぎるため。parse cache は設定に依存しないので、parser は関数名ごとに候補を残し、設定を読んだ後に loader API の名前で絞る。

invalid requirement の行は宣言として数えないが、先頭の名前の distribution を宣言していたかもしれない。その distribution に解決した import の CHK003 / CHK004 は「宣言していない」と言い切れないので、名前の読める行を gap にする (#928)。名前は行の先頭の名前文字 (`[A-Za-z0-9._-]`) の並びから、PEP 508 の名前が始まりにも終わりにも置けない `-` `_` `.` を除いて正規化したもの (`rich_ >=13` は `rich`) で、後ろに別の語が続く行 (`not a requirement!!`) は requirement の形でないので名前を取らない。pip が file として読む path や archive の形 (`wheels/foo.whl`、`C:\pkg`、`foo-1.0-py3-none-any.whl`) も名前を取らない (requirements ファイルではこの形は §4 の opaque 依存になって invalid にならないので、PEP 508 field に残る invalid の行が対象。#963)。`#egg=` を持つ行は egg の値から同じ規則で名前を取り (`#egg=acme;python_version<"3"` は `acme`、`#egg=with space` は名前を取らない)、持たない URL の行は名前を取らない (#929)。名前を取れない行 (`>=1.0`、`{{ name }}>=1`) と、pip がオプションとして読む `-` で始まる行 (`-r` だけの行、`-example-pkg`) は何も下げない。どの distribution も名指ししておらず、manifest 全体を読めなかったとみなすと、1 行のために全 import の欠落が hint になるため。warning は宣言と `-c` の制約を区別しないので、制約の行も同じく下げる。runtime 側 (runtime 依存・extra・`runtime_groups` の group) の行は、dev / type にしか宣言のない distribution の runtime 宣言だったかもしれないので、その名前の CHK005 (misplaced) も info / maybe にする (`InvalidRuntimeRequirement`、#935)。dev / type 側の行は、CHK005 が欠けているとみなす runtime 宣言になりえないので下げない。runtime 依存全体を読めない `RuntimeDependenciesUnknown` も同じ理由で CHK005 を下げる。PEP 723 script の block の invalid な行は、その script の block だけが環境を決めるので、その script のその名前の CHK003 だけを下げる (`InvalidScriptRequirement`、#936)。script に CHK005 は無いので、warning は `undeclared imports of` とだけ添える。`site` は名前と行で (`` `pyyaml` in `requirements.txt:1` ``)、行を持たない pyproject の field では行を省く (`` `requests` in `pyproject.toml` ``)。名前を取れた行の warning は、その名前の欠落が hint になることを添える (`` manifest: invalid requirement at `requirements.txt:1`: PyYAML>=; undeclared or misplaced imports of `pyyaml` are reported as hints ``)。group の行は、warning が config の `runtime_groups` を見ないので `undeclared imports of` とだけ添える。

requirements ファイルの path / archive / URL の行は §4 の opaque 依存で、何を install するかを読まない。ただし archive の file 名は名前を持つ形が決まっているので、名前を読める行を gap にする (#988)。wheel は PEP 427 の `{name}-{version}(-{build})?-{python}-{abi}-{platform}.whl` で、`-` で分けて 5 つか 6 つの欄があれば先頭を名前にする。sdist (`.tar.gz`・`.zip` など pip が archive とみなす拡張子) は拡張子を除いた最後の `-` の前を名前にし、後ろが数字で始まる (version の形の) ときだけ取る。どちらも query・fragment・末尾の `[extras]`・marker を除いた最後の path 要素から読み (`https://x/baz-2.0.tar.gz#sha256=…` は `baz`)、PEP 508 の名前でなければ取らない。名前を取れない行 (`.`、`./vendor/foo`、`-e .`、`git+https://…`、`latest.tar.gz`) は #928 と同じ理由で何も下げず、warning も出さない。`-e .` は project 自身を入れる普通の行で、manifest を読めなかったとみなすと 1 行のために全 import の欠落が hint になるため。名前は推測にとどまる (file 名を変えた archive もある) ので宣言にはせず、unused / duplicate / `--fix` は従来どおりこの行を見ない。下げるのは invalid requirement と同じく、その名前の distribution に解決した import の CHK003 / CHK004 で、runtime 側の行 (bucket のどれかが runtime か extra) は CHK005 も下げる。加えて、import root を PEP 503 で正規化するとその名前になる未解決の import の CHK010 も下げる。vendored の wheel は map に無い private package であることが多く、その import は CHK003 ではなく CHK010 になるため。§7 step 8 の `py` / `python` の接頭辞・接尾辞の一致は使わない。opaque 依存には manifest warning が無いので、gap を集めた後に 1 run 1 回、最初の行を名指しして出す (`` manifest: `requirements.txt:2` installs `bar` and 1 more distribution from archives chokkin does not read; findings on imports of them are reported as hints ``、ADR 0005 §3)。

`member` は gap が下げる finding を 1 つの workspace member に絞る。root manifest と module の gap は `None` で、表の範囲の finding をすべて下げる。member manifest の `RuntimeDependenciesUnknown`・`UnknownExtras`・`InvalidRequirement`・`InvalidRuntimeRequirement`・`ArchiveRequirement`・`ArchiveRuntimeRequirement` は、その member の id を持ち、`workspace_member` がその member の finding だけを下げる (#910)。member の宣言が読めなくても、宣言できるのはその member の import だけで、root や他の member の宣言の欠落は変わらないため。root の gap は member の finding も下げる。member の import は root の宣言でも満たされうるので、root の宣言が読めなければ member の欠落も確かではない。`--explain` が数えるのは、その finding に届く gap だけにする。

不在を言う finding (CHK001 / CHK002) は certain から外し、宣言の欠落を言う finding (CHK003 / CHK004) は hint にする。名指しの読める文字列 (StringModuleRef) は、どの file かは決まるが読み込むかは決まらないので、その file の CHK001 を likely の hint にする (`Lowering::Hint(Likely)`)。上限として当てるので confidence を上げることはない。exit code は下がった後の severity / confidence で決まり、gap 自体は exit code を変えない (§7)。下げた finding の `--explain` には、gap の種類ごとに理由と最初の file を 1 行出す (`` a reachable module could not be read: `acme/a.py` (and 1 more) ``)。gap はどれも warning として利用者に届く (ADR 0005 §3)。skip した file と読めなかった cell は parse の warning、skip した entry は entry の warning、読めない runtime 依存・invalid requirement の行・読めない `requires-python` は manifest の warning、読めない Django の settings と読み切れない Sphinx の `extensions` は plugin の warning が名指しする (`` plugin: sphinx: `extensions` could not all be read (`extensions` in `docs/conf.py:3`); unused files and dependencies are reported below certain ``、site ごとに 1 回)。opaque dynamic import には記録した段の warning がないので、gap を集めた後に 1 run 1 回だけ、最初の file と残りの数を出す (`` parse: `acme/main.py` imports a module by a name computed at runtime; unused files are reported below certain ``、#909)。command の target には記録した段の warning がないので、gap を集めた後に command を書いた file ごとに 1 回、最初の target と残りの数を出す (`` plugin: `Makefile:3` runs `scripts/gen.py` as Python code no entry reaches; unused files are reported below certain ``、#913)。module を名指しする文字列にも記録した段の warning がないので、gap を集めた後に 1 run 1 回だけ、最初の文字列と残りの数を出す (`` parse: `acme/cli.py:2` names `acme.commands.cheat_sheet.run` in a string that may load `acme/commands/cheat_sheet.py`, which no entry reaches; that file is reported as unused at info ``、#1026)。

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

外部参照として追跡する import パターンは次の通り。

```text
from acme.utils import helper          # certain
import acme.utils; acme.utils.helper() # likely（attribute access）
```

`import module; module.name` 形式は parser が同一 file 内の attribute access を収集し、
symbol usage analysis が `module.name` を外部参照として扱う。`from pkg import module` (alias、
`from . import module` を含む) で既知の first-party submodule を束縛した場合も同様に扱う (#411)。chain access や動的 attribute
は v0.2 では追跡しない。

参照先の module が `from m import *` で名前を取り込んでいる場合、その参照は `m` の同名 symbol にも届くとみなす (#576)。
`pkg/__init__.py` が `from .sub.impl import *` するとき、`import pkg; pkg.name` と `from pkg import name` は
`pkg.sub.impl.name` の参照になる。star 先がさらに star import していれば連鎖を辿り、各段で `__all__` のある module は
列挙した名前だけ、無い module は `_` で始まらない名前だけを渡す (Python の `import *` と同じ)。`__all__` を読み切れなかった
module (Partial / Unknown) はどの名前も渡しうるとみなし、`_` で始まる名前も含めて渡す (#951)。読めた list だけを渡すと、
読めなかった名前の参照が届かず CHK006 を certain で出してしまうため。star import した module が
同名を自身で定義していれば、そちらが優先され star 先には届かない。外部参照かどうかは
元の importer で判定するので、定義元 module 自身が package 経由で自分の名前を読んでも外部参照にはならない。
star import 文そのもの (`pkg/__init__.py` の `from .sub.impl import *`) は従来どおり参照にならない。

import 先の名前がどの module 名でもなく、importer と同じディレクトリに同名の file があれば、その file の module の参照とみなす (#977)。
`python path/to/script.py` は script のディレクトリを `sys.path` 先頭に置くので、airflow `scripts/` の
`from common_prek_utils import X` や `from in_container_utils import X` は隣の file を読む。到達性の script fallback と同じ扱いで、
外すと package として配布される script 置き場の helper をすべて CHK006 にしてしまう。

例外として、literal の接頭辞で組み立てた動的 import (§14、`import_module("pkg.commands." + name)` / `f"pkg.commands.{name}"`) を
代入や注釈付き代入で名前に束縛し、同じスコープ (関数・class 本体・module) で `getattr(module, <非literal>)` に渡している場合は、
`pkg.commands` 配下の module で定義された公開 class をすべて外部参照済みとし CHK006 にしない (#728)。poetry の command loader
(`getattr(module, name.title() + "Command")`) のように class 名を組み立てて取り出すため。関数・定数、`getattr` の属性名が
literal の場合、束縛後に別の値を代入した名前、入れ子の関数など別スコープからの `getattr` は対象外とする。
先に `getattr(module, "__all__")` を読むスコープは `__all__` に列挙された名前を順に取り出す import smoke test とみなし、これも対象外とする。

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

decorator 由来の used 判定は `src/rules/symbols/external.rs` の `REGISTRATION_DECORATOR_SUFFIXES` 1 か所で持つ (`get` / `post` / `put` / `delete` / `patch` / `route` / `websocket` / `fixture` / `shared_task` / `task` / `command` の suffix、`pytest.mark.*`、最後の属性名が `hookimpl` / `hookspec` で終わる pluggy の marker (`@hookimpl`、`@pm.hookspec(firstresult=True)`、airflow の `local_settings_hookspec = pluggy.HookspecMarker("airflow.policy")` で作った `@local_settings_hookspec`、#777))。parser は静的に名前の付く decorator をすべて記録し、Flask route / Celery task の module reference は各 plugin が自前の述語で判定する。suffix を足すのは、`tests/fixtures/symbols/` にその誤検知を再現する fixture を置いてからにする。

`__all__` は module top-level の文を上から読む (`src/parser/exports.rs`)。`__all__ = [...]` / `__all__: T = (...)` は文字列 literal の list か tuple で置き換え、その後の `__all__ += [...]`・`__all__.extend([...])`(tuple も可)・`__all__.append("x")`・`__all__.insert(i, "x")`・`__all__ = __all__ + [...]`(`[...] + __all__` も) は追記する (#567、#876)。`if` / `try` / `with` の block は経路ごとに読み、経路の合流点で名前の和集合をとる (#906)。block 内の literal 代入はその経路の list を置き換える。どの分岐が走るかは分からないが、mlflow の `if` 内の `__all__ += [...]`、django の `try/else`、`try: from .x import Auth; __all__ = ["Client", "Auth"] except ImportError: __all__ = ["Client"]` のように optional 依存で増える export も公開 API なので、全経路の名前を足す。`else` のない `if` は block を飛ばす経路も合流させる。`try` の body と `with` の body は途中の文で止まりうる (例外、`suppress(ImportError)`) ので、`except` は body の各文の後の状態をすべて合流させた状態から読み、`with` の後もその合流とする。`match` は case ごとの経路を `if` / `elif` と同じく合流させ、guard のない irrefutable な case (`case _:`) がなければ、どの case も走らない経路も合流させる (#875)。`for` / `while` の本体と `else` は literal な追記だけを読む。何回回っても literal の追記で増える名前は同じなので 1 回足せば足り、`break` で後の追記が飛んでも和集合はその上位集合になる。一方、loop 内の代入は回数と `break` の位置で残る list が変わるので読まない。経路の 1 つでも読めない (Unknown) なら結果も Unknown にする。

block の前に list がなく、一部の経路だけで代入する block (`if TYPE_CHECKING: __all__: list[str] = []`、`try` 内だけの代入) の後は、`__all__` が未束縛のままの経路が残る。そのときは読めた名前の Partial にし、block の行に `__all__ is assigned on only some paths through this block, so it may be unset` の warning を出す。束縛された経路ではその名前を持つが、未束縛なら `__all__` なしと同じ (`_` で始まらない名前すべてが公開) になるため。後の top-level literal 代入で Known に戻る。

読めた結果は ADR 0005 の状態で持つ (`Exports`)。`__all__` を束縛しない module は Absent、すべて読めたら Known、literal の list は読めたがそれ以外の更新を読めなかったら Partial (読めた名前に加えて、他の名前もあるかもしれない)、list を読む前に読めない形で束縛・追記したら Unknown にする。読めなかった更新ごとに `unsupported __all__ assignment form` の warning を出す。

- literal でない代入 (`sorted(__all__)` など)、`from .sub import __all__`、literal でない追記 (`__all__ += sub.__all__`、`.extend(names)`、`.insert(0, name)`、`__all__ = __all__ + names`)、それ以外の追加しうる method 呼び出し、`__all__[...] = ...`、`for` / `while` の本体と `else` にある代入、関数の本体にある更新は、読めた list を保ったまま Partial にする。list がなければ Unknown にする。後の top-level literal 代入で Known に戻る。読めない代入でも名前を残すのは、list を組み直す形 (`sorted(__all__)`) では名前が残り、捨てると finding を下げる段 (後述の step 3) より前にそれらを CHK006 にしてしまうため。関数本体を読まないのは、`export` decorator の `__all__.append(fn.__name__)` のように呼ばれたときだけ走るため。
- class 本体と、`global __all__` を直下に持たない関数の本体で `__all__` を束縛する文 (代入・`+=`・`from x import __all__`) は local 名を作るだけなので読まない。method 呼び出しや `__all__[...] = ...` は module の list を書き換えるので、上と同じく Partial にする。
- list を読む前 (Absent や Unknown のとき) の literal な追記は捨て、Absent なら Unknown にする。読めない list に足した名前だけが `__all__` 全体と扱われ、他の公開名が隠れるのを避けるため。
- `remove` / `pop` / `clear` / `sort` / `reverse` と、読むだけの `copy` / `index` / `count` は追わない。実行時より広い list は名前を公開扱いにするだけなので、finding を増やさない。

rule は今のところ読めた名前 (`Exports::names`、Known / Partial の list。Absent / Unknown は空) だけを見て、空なら `__all__` なしと同じに扱う。例外は `from m import *` が渡す名前で、Partial / Unknown の module はどの名前も渡しうるとする (`Exports::may_hand_on`、#951)。star の参照解決 (上記 #576)、library mode の star 到達による公開 API、設定エントリの star import の 3 か所がこれを使う。Partial / Unknown で finding を下げるのは、ADR 0005 の移行 step 4 (gap の種類ごとの接続) で、§11 の降格段に gap の種類を足して行う。

`__all__` があるmoduleでは、`__all__` をpublic API宣言として扱う。`__all__` にあるが内部から使われないものは、app modeではwarning (confidence certain) にする。library modeでは、次のいずれかに当てはまるsymbolを公開APIとみなし、CHK006 / CHK007 を出さない (`src/rules/symbols/public.rs`、#489)。それ以外の未参照symbolはinfoにする。wheel target から public surface が求まる場合、surface 外 module の symbol は library mode でも app mode 相当 (warning、公開API扱いなし) にする (§8)。

```text
__all__ に載っている
from ._x import Foo as Foo で再exportしている (PEP 484 の明示的再export)
公開module (`_` 始まりの要素を含まないdotted name) から from m import * で連鎖的に到達できる (m に __all__ があればその名前だけ。読み切れない __all__ ならどの名前も)
公開packageの __init__.py で定義・再exportしている
```

`--production` では、外した tests と、tests だけが到達していた公開 module が参照する名前も、library の外部の利用者が使う名前の根拠にする (#588)。根拠にする module は、tests の import から production で到達しない module をたどって届く公開 module に限る (#950)。たどる辺は first-party の import と literal の dynamic import と `pytest_plugins` (いずれも親 package を含む)、`from pkg import sub`、prefix 付きの dynamic import (`"pkg." + name`) が名指す package の下の module で、plugin の参照や framework glob のような別の形で tests が届くだけの module はたどらない。tests からも届かない orphan module (default でも CHK001) は dead code で、外部の利用者ではないため。たどれない形で届く module は orphan と同じく根拠にしない。根拠が default で到達する module に収まるので、default の CHK006 が `--production` で消えるのは定義元の file が CHK001 になるときだけになる。private module は根拠にしないが、その先の公開 module へはたどる。

library modeでは、定義元module自身がその名前を読んでいるsymbol (parser の `SymbolDef::used_in_module`。注釈・関数本体・class本体の `Name` load を含む) も CHK006 にしない (#540)。TypeVar、型alias、属性やメソッドの戻り値の型としてだけ外に出るclass、module内のhelperやloggerがこれに当たる。外部の利用者はこれらに属性経由で届き得るので、library では「private にできる」と言い切れない (knip の `ignoreExportsUsedInFile` 相当)。app modeでも、`__all__` に載っていない `used_in_module` のsymbolは CHK006 にしない (#564)。Python には `export` がないため、`__all__` にない top-level 名は TS の非 export 宣言に当たり、knip の `exports` が対象にしないものと揃える。`__all__` に載っている名前は明示的な export として扱い、module内で読まれていても外部参照がなければ報告する (knip の `ignoreExportsUsedInFile: false` 既定と同じ)。ただし wheel が配布する file の `__all__` は CHK007 と同じく公開 API として報告しない (#727、後述)。判定は名前単位なので、同名のlocal変数や組み込み名 (`min` など) の読み取りでも used になる。

ツールが import ではなく規約で読むものも報告しない (`src/rules/symbols/conventions.rs`、#655)。protoc の生成物 (`*_pb2.py` / `*_pb2_grpc.py`) と、同じディレクトリに `modeling_x.py` がある `modular_x.py` (transformers の codegen 元) は、test context と同じく CHK006 / CHK007 の対象外・参照元のみとする。alembic plugin が有効なとき、祖先に `versions/` ディレクトリがあり `revision` と `down_revision` を両方定義する module は revision file とみなし、`revision` / `down_revision` / `branch_labels` / `depends_on` / `upgrade` / `downgrade` を CHK006 の外部利用扱いにする。django plugin が有効なとき、親ディレクトリが `migrations/` の module (`__init__.py` を除く) が定義する `Migration` も CHK006 の外部利用扱いにする (#926)。Django の migration loader は app の `migrations` package の各 module から `Migration` を名前で読むので、どの importer もその名前を書かない。`MIGRATION_MODULES` で別名にした package や、`migrations/` の下の階層は loader が読まない形なので対象にせず、従来どおり CHK006 にする。いずれも CHK001 の判定には使わない (生成物が到達不能なら、消してよいかは別に確かめる必要がある)。alembic plugin が `script_location` から entry にしたファイル (`env.py` と revision) は import されず alembic が path で読むので、test context と同じく CHK006 / CHK007 の対象外・参照元のみとする (#667)。framework が top-level の名前を自分で読む entry も同じ扱いにする (#401): airflow plugin の DAG file (DAG processor が path で読み、束縛された DAG を集める)、dagster plugin の entry (束縛された definitions を集める)、scrapy plugin の settings module (設定名を読む) と `SPIDER_MODULES` の下の module (spider class を集める)、sphinx plugin の conf.py (`project` や `html_extra_path` を名前空間から読む。package の中にある root document の隣の conf.py (airflow の `devel-common/src/docs/rtd-deprecation/conf.py`) も、公開 module ではなくこの扱いにする、#814)。prefect の entrypoint は関数だけが読まれるのでこの扱いにせず、その関数を使用済みにするだけにする。scrapy の settings が path で名指す component も、名前の付いた symbol を使用済みにするだけにする。airflow の `airflow_version` のように、project 自身の tool が revision module から読む名前もこれで外れる。

設定エントリが `from m import *` で取り込む名前も CHK006 の外部利用扱いにする (`config_star_symbols`、#729)。設定エントリは、ホストが module の名前空間を設定として読む plugin entry で、sphinx plugin の conf.py (root と各 workspace member の `docs/conf.py`、root document の隣の `conf.py`、§19) と django plugin の settings module を指す。Sphinx は `html_theme` を、Django は `INSTALLED_APPS` を名前空間から読むので、どの importer もその名前を書かない (airflow の各 provider の `docs/conf.py` が star import する `provider_conf.py`)。star 先がさらに star import していれば連鎖を辿る。`__all__` のある module は列挙した名前だけ、無い module は公開名すべて、`__all__` を読み切れなかった module はすべての名前を対象にする (連鎖の途中にある `__all__` で先の module の名前を絞ることはしない)。対象は star 先で定義された symbol だけで、設定エントリ自身が定義する名前、`__init__.py` の再export (CHK007)、`manage.py` や `ROOT_URLCONF` の module など設定エントリ以外の entry からの star import は従来どおり扱う。通常コードの star import は従来どおり参照にならない。CHK001 の判定には使わない。

ホストが規約名で呼ぶ関数も CHK006 の外部利用扱いにする (`host_hook_symbols`、#777)。対象は module level の関数で、次のいずれか。(1) pytest plugin の `pytest_*` 関数。plugin とみなすのは、いずれかの module の `pytest_plugins` と pyproject の `[tool.pytest.ini_options] pytest_plugins` に挙がった module、`pytest11` entry point の module、pytest (`pytest` / `_pytest`) を関数内でも `TYPE_CHECKING` 下でもない位置で import する module (`-p` で opt-in する mlflow の `mlflow/pytest/plugin.py` のように登録元が無い plugin も拾う)。(2) sphinx plugin が読む `extensions` の名前を、reachability と同じく `docs/conf.py` から解決した file の `setup`。`sys.path` の hint で見つかる拡張は package を付けない名前で挙がる (`sphinx_exts.docroles` を `docroles`) が、同じ末尾を持つだけの別 module は当たりにしない (#779)。(3) build backend の entry（origin `build backend: PEP 517` と `manifest: [build-system.backend-path].path`）の module にある PEP 517 / PEP 660 の hook（`build_wheel` / `build_sdist` / `build_editable` と、その `get_requires_for_build_*` / `prepare_metadata_for_build_*`）。frontend が呼ぶため (#782)。これらの file が package の `__init__.py` で、hook を submodule から再exportしている場合 (`from .core import setup`) は、再exportした名前 `package:setup` も同じ集合に入れる。ホストが読むのは package の名前空間のため (#873)。これらに当たらない module の同名関数は従来どおり報告する。pluggy の hook は decorator で判定する (上記)。CHK001 の判定には使わない。

引数・戻り値・`AnnAssign` の注釈にある文字列 (`x: "list[T]"`、`list["Foo"]` の内側) は式として parse し直し、中の `Name` も読み取りに数える (#545)。`Literal[...]` の中の文字列と `Annotated[...]` の2番目以降の引数は対象外。

注釈以外でも、typing が型式として読む位置の文字列は同じく読み取りに数える (#577)。`cast("T", x)` の第1引数 (`typ=`)、`TypeVar("M", "A", "B", bound="Foo", default="Bar")` の2番目以降の位置引数と `bound=` / `default=`、`X: TypeAlias = "list[Foo]"` の右辺、`type X = "Foo"` の右辺が対象。`TypeVar` の第1引数は変数自身の名前なので数えない。呼び出し先は `cast` / `TypeVar` という名前そのものか、`typing` (`import typing as t` の別名を含む) / `typing_extensions` の属性 (`t.cast`、`typing_extensions.TypeVar`) に限る。`Literal` / `Annotated` と違い末尾の名前だけでは判定しない。`col.cast("float")` や `memoryview.cast("B")` のように同名のメソッドがあるため。

それ以外の呼び出しの文字列引数は名前として読まない。sgqlc の生成コード (`sgqlc.types.Field("UserConnection", graphql_name=...)`) のように任意の callee が文字列で class を指す書き方は、識別子に見える文字列すべてを参照に数えると無関係な文字列 (`name="deploy"`、`kwargs.pop("arerank")` など) まで拾うため対象にしない。生成コードは `vendored` に入れるか (走査と参照元には残したまま issue を出さない)、`[tool.chokkin.ignore]` の `CHK006 = ["path/to/graphql_schema.py:*"]` で外す。`exclude` は file を走査から外し、そこからの import も参照に数えなくなるので、生成コードが他 module を使う場合には向かない。

CHK007 は、再exportした `__init__.py` 自身がその名前を読んでいる import (parser の `ParsedModule::used_import_bindings`) を対象外にする。`from .x import a as b` の再export名は `b` とする。

CHK007 は、CHK006 の外部利用扱いの集合に `package:name` が入っている再exportも対象外にする (#743)。`[project.scripts] instr = "instr:main"` のように entry point が package の再export を指す場合や、plugin が `package.name` を参照する場合が当たる。console script やホストはコードの import を経ずにその名前を読むため。この集合のうち再exportと一致するのは、entry point と plugin の symbol 参照、および再exportされた host hook (上記、#873) である。entry からも plugin からも名指しされない再exportは、従来どおり報告する。

entry point と plugin の symbol 参照は、target `module:object.attr [extras]` の先頭の `object` を外部利用扱いの名前にする (#872)。`tool = "tool.cli:Cli.main"` なら `tool.cli:Cli` を、再exportを指す `tool:Cli.main` なら CHK007 の `tool:Cli` を対象外にする。loader は module を import して `object` を取り出し、そこから属性を辿るため。extras (`[cli]`) は名前に含めない。属性の先 (`Cli.main` の `main`) は module level の symbol ではないので扱わない。同じ file を指す entry が複数あれば、それぞれの `object` をすべて外部利用扱いにする。entry root は file ごとに 1 つへまとめるが、symbol は和集合で持ち、後から来た entry の symbol で上書きしない (#884)。

CHK007 は mode によらず、wheel が配布する `__init__.py` で明示的に公開した再export（`__all__` に載っている、または `import a as a`）も対象外にする (#678)。app signal を持つ workspace root と CLI を持つ member は app mode になる (§8) が、その wheel の利用者はこれらの名前を package から import できるため。配布するかは root の wheel target から求めた public surface で判定し、workspace member の file は library / app を問わず最も深い member 自身の wheel target（member 相対）で判定する。wheel target を宣言せず `[project].name` と `[build-system]` の `build-backend` を持つ member は、build backend の自動検出（hatchling の `src/<name>` 等）と同じく layout が推定した package を配布するものとみなす (#733、llama-index-instrumentation)。`[build-system]` のない member は uv が package にしないので何も配布しない。この推定は配布判定だけに使い、CHK001 の到達性を絞る library surface は宣言された wheel target からだけ求める。wheel target を宣言していない root の file や、surface に含まれない file（`examples/` の app 内 `__all__` 等）は従来どおり app mode で warning にする。

同じ理由で、wheel が配布する file の `__all__` に載っている名前は、module 内でしか使われていなくても CHK006 にしない (#727)。app mode の workspace root (§8) では、fastmcp の `fastmcp_slim` の配布 module の `__all__` 定数が warning / certain になっていた。`__all__` に載っていない名前と、配布されない file の `__all__` は従来どおり app mode の判定に従う。CHK001 の判定は変えない。

import 時に非推奨を告げる互換 shim (§11、#778) で定義された symbol の CHK006 は info にする。

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
曖昧なmissing dependencyの自動追加
dynamic import周辺の変更
```

未使用ファイル削除は明示フラグ必須。v0.2初期実装では `CHK001` のうち
confidence が `certain` の root-relative file だけを、project root containment
check 後に削除する。`--dry-run` では削除せず preview のみ返す。

`--fix` の書き込み先は、`..` や絶対パスを含まず、symlink を辿った実体も project
root 配下にあるものに限る。外れたものは `unsupported-target` として skipped に
回す。`--dry-run` も preview の前に同じ検査をするので、実行で拒否される編集を
`planned` と表示しない（#980）。`--init` も追記する前に `pyproject.toml` に同じ検査
をし、root 外を指していれば何も書かずに exit code 2 で終了する。

```bash
uvx chokkin --fix --allow-remove-files
```

missing dependency追加は別フラグにする。

```bash
uvx chokkin --fix --add-missing
```

v0.2初期実装では Certain CHK003 で distribution が一意に解決済み、かつ対象 manifest の
non-Poetry `pyproject.toml` が `[project]` テーブルを持ち、`project.dependencies` を dynamic にしていない場合だけ、
`[project].dependencies` に正規化済みdistribution名を追加する。workspace member finding は
`ProbeReport.workspace_inputs` で member manifest が inventory 済みなら member 側
`pyproject.toml` に追加する。Poetry形式、requirements系、optional try-import、候補が複数ある場合は
skipped fix として報告し、suggestion/手動修正に留める。`[project]` の無い `pyproject.toml`
(依存を setup.py / setup.cfg で宣言し、pyproject には `[tool.*]` だけを置く構成)に `[project]` を
作ると setuptools がそちらを正とみなしてビルドが壊れるため、UnsupportedTarget で skip する (#581)。CHK005 の `[project].dependencies` への移動も同じ条件で skip する。Poetry 1.x の `pyproject.toml` に `dependencies` だけの `[project]` を作ると、PEP 621 が必須とする `name` を欠いた表になるため。

manifest編集はformat保持が重要。Rustなら `toml_edit` を使い、commentsと順序を極力維持する。requirements系はline-based編集で、hash付きrequirementsやconstraintsは原則自動編集しない。`--hash` は行内のどの形(`--hash=`・`--hash `・継続行)でも検出してファイルごと編集しない。pipが拒むオプションを含む行も、`--hash` の文字があれば hash 付きとみなす。`\` で継続した requirement は、つないだ行として名前を照合し、属する物理行をまとめて消す (#927)。`setup.cfg` もline-based編集だが、対象は宣言のラベル（`options.install_requires[i]` / `options.extras_require.<extra>[i]`）が指すsection・option・itemに限る。itemの数え方はmanifest抽出と同じ行分類器による（重複sectionは合流、コメント・空行はitemではない）。継続行のitemは行末ごと消し、インライン値のitemは `key =` と行末を残すので、空になったoptionも宣言として残る。他の行のバイト列と行末（CRLF）は変えない。そのindexのitemがもう対象のdistributionを指さなければ編集しない。複数行の `project.dependencies` に追加するentry（CHK003 `--add-missing`、CHK005 の移動）は既存のindentで独立した行に置き、直前のentryの行コメントはそのentryの行に残す。

書き込み安全性: manifest編集は同一ディレクトリへの一時ファイル作成→`rename` でアトミックに置換する(`fix/write.rs`)。既存ファイルのpermissionsは可能な範囲で引き継ぐ。fix対象パスはproject root内に収まることを検証し、ルート外への書き込みはskipする。requirementsの `-r`/`-c` インクルードとDjango `settings.py` 探索も同様にルート封じ込めする。

lockfileとの整合にも注意する。`pyproject.toml` を編集するとuv.lock / poetry.lock等は古くなるが、`chokkin` はlockfileを直接編集しない。fix適用後に、読んだlockfileの種類に応じて `uv lock` / `poetry lock` / `pdm lock` の実行を促すメッセージを出力する(poetry管理のprojectはlockfileがなくても `poetry lock` を促す)。

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
|dynamic importを完全には解けない       |`importlib.import_module(name)`                                |literalは式中(代入・return・引数など)も解く。`from importlib import import_module as im` などの別名と `name=` キーワード引数も認識する。非literalや `map(importlib.import_module, names)` のような関数値での受け渡しはopaque dynamic importとしてconfidenceを下げる。`[sys.executable, "-m", "pkg"]` は `pkg` の使用として数えるが、未宣言でも CHK003 にはしない。`"pkg.commands." + name` や `f"pkg.commands.{name}"` のようにliteralの接頭辞で組み立てた名前は、`pkg.commands` 配下の first-party module すべてへ到達させる(confidenceはopaqueのまま下げる)。その結果を同じスコープで `getattr(module, <非literal>)` に渡すと、配下の class を CHK006 にしない(§12、#728)。loader API (`import_string("pkg.mod.Cls")` など) の文字列も動的 import として解く (§11、#493)                                 |
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
    parser/      # 実装済み: pipeline step 6 (`parse_file`, `parse_project_sources_with_cache`; cold parse は `std::thread::scope` でファイル単位並列、出力は discovery 順)
    resolver/    # 実装済み: pipeline step 7 (`resolve_imports`, bundled maps, venv RECORD/entry_points, versioned stdlib; module 単位で worker 並列、warning は module 順に合流)
    entry/       # 実装済み: pipeline step 8 (`build_entry_roots`)
    reachability/ # 実装済み: pipeline step 9 (`analyze_reachability`, `trace_to_file`)
    rules/       # 実装済み: step 10 `rules/deps/` (`reconcile_with_context`, CHK002–CHK009);
                 #           step 11 `rules/symbols/` (`analyze_with_context`, CHK006–CHK007, CHK010; step 10 と並行);
                 #           step 12 (`emit_issues`, `explain_issue`, ignore/filter)
    reporters/   # 実装済み: default / compact / json / markdown reporter
    fix/         # 実装済み: step 13 (`apply_fixes_with_workspace` — pyproject/requirements/setup.cfg; atomic write, root containment)
```

`pyproject.toml` は概ねこうする。

```toml
[build-system]
requires = ["maturin>=1.8,<2"]
build-backend = "maturin"

[project]
name = "chokkin"
version = "0.2.0"
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
Windows arm64
```

上記に加え、sdist からの Rust build を避けるため次の target も配布する: manylinux i686 / armv7 / ppc64le / s390x、musllinux i686 / armv7、Windows i686。

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

v0.2 時点の JSON reporter / baseline file は draft schema として扱い、互換性方針と migration note は `docs/dev/schema-migration-notes.md` に置く。v0.3 (Phase 3) で `schema_version: "1"` と公開 JSON Schema (`docs/schema/`) を追加し、v0.2 baseline reader 互換を維持する。完全な semver 契約は v1.0 で凍結する。import 地点を subject に持つ CHK003 / CHK004 / CHK010 の JSON issue は、`path` に import 元 file、`symbol` に import した dotted module、`distribution` に resolver が選んだ distribution (CHK010 は `null`) を入れる (#363)。 宣言を subject に持つ CHK002 は、message を distribution 名で始め (`` rich is declared in project.dependencies[2], but … ``)、`manifest.line` に宣言の行を入れる (#983)。SARIF の `message.text` と compact の一覧は subject を別に出さない読者が見るため。pyproject.toml の宣言は `toml` で読むと位置が残らないので、同じ text を `toml_edit` で読み直し、宣言の label (`project.dependencies[2]`、`tool.poetry.dependencies."zope.interface"` の key) を辿って行を取る。辿れない label は従来どおり `line: null` にする。 `INSTALLED_APPS` や Sphinx `extensions` のように plugin が設定から読んだ import は、その module を名指す文字列の行を `line` にする (Sphinx の `extensions` が constants module から import した名前なら、`conf.py` でその名前を書いた行)。`import` 文と区別するため、message と explain は `import` ではなく設定名を書く (CHK010 `` `extensions` entry `myst_parser` in `docs/conf.py:4` could not be resolved ``、CHK003 `` referenced requests from `INSTALLED_APPS` in mysite/settings.py:1 ``)。inline の `# chokkin: ignore[CHK010]` もその文字列の行に書けば効く。位置を持たない参照は `line` を `null` にし、inline ignore は効かない (#835)。fingerprint と target は `file:module` のままで行を含めないので、行が付いても baseline は変わらない。`--explain` は `CHK010:file:module` でも `CHK010:file:line:module` でも選べる (#830)。 JSON reporter は stderr に出す非致命的な warning を top-level の `diagnostics` (`[{"message": …}]`) にも入れる (#486)。他の reporter も warning を落とさない (#888)。default / compact は stdout の最後に `Warnings: N (see stderr)` と件数だけを出す。端末では同じ内容が stderr にすでに出ているので、二重に並べない。markdown は CI の step summary として stderr を見ない読者が開くので、`- Warnings: **N**` と末尾の `## Warnings  N` 節に warning を並べる (N は全件数で、並べるのは上の規則でまとめたもの)。github は workflow run の画面に出るのが annotation だけなので、まとめた warning ごとに `::warning title=chokkin::…` を 1 行出す。sarif は `runs[0].invocations[0].toolExecutionNotifications` に `level: "warning"` で入れ、warning が無いときは `invocations` を出さない。warning が開ける file を指すとき (parse の skip と diagnostic、manifest の読めなかった file、Django settings や pytest 設定など)、github は `file=` と、行があれば `line=` を付け、sarif は notification に `locations` (行があれば `region.startLine`) を付ける (#895)。PR の diff の上に annotation が出るようにするため。存在しない path (見つからない entry や constraints file)、directory、絶対 path、候補が複数ある warning、まとめた行は位置を付けない。notebook の行番号は連結した code cell の行で JSON の行ではないので、notebook は `file=` だけにする。JSON の `diagnostics` は `message` だけのままにする。 file 名・venv の METADATA・manifest の値は解析対象の project から来るので、default / compact / markdown / github と `--fix` / baseline の要約・stderr の warning・`--probe`・`--trace`・`--explain`・致命的な error は、その中の制御文字 (`U+0000`–`U+001F`、`U+007F`–`U+009F`) を `\u{1b}` のように書き出す (#973, #996)。生のまま出すと端末の色を変えたり、改行入りの file 名で finding の行を偽造できたりするため。bidi の埋め込み・上書き・分離 (`U+202A`–`U+202E`、`U+2066`–`U+2069`) と行・段落区切り (`U+2028`、`U+2029`) も同じく書き出す。制御文字ではないが、端末の行を並べ替えたり分けたりするため。TOML の parse error は該当行を引用して複数行で出るので、行の区切り (`\n` と `\r\n`) だけは残す。github は Actions が戻す `%0A` / `%0D` を先に当て、残りを同じく書き出す。JSON と SARIF は serde が制御文字を escape する。bidi と行・段落区切りは制御文字ではないので生のまま残るが、読むのは tool で端末ではない。 `summary.files` には runtime context の source file 数 (`runtime`) と、そのうち entry root から到達できる数 (`reachable_runtime`) を入れる。package root や entry point の見落としは `reachable_runtime` の急減として現れる (#495)。

v0.5で入れるもの (モダン packaging 追従。詳細は `docs/dev/roadmap-gap-analysis.ja.md`)。

```text
- PEP 735 include-group 展開
- PEP 723 inline script metadata (script 単位の entry + dependency scope)
- pylock.toml (PEP 751) / poetry.lock / pdm.lock の読み取り
- [tool.uv] sources / constraint / override / default-groups の読み取り
- [build-system].requires の build context
- binary usage 情報源の拡充 (PDM scripts / Makefile / justfile / Dockerfile / GitLab CI / pytest addopts / mypy plugins)
- 依存宣言からの plugin 自動有効化
- parser 移行の決定 (Python 3.12+ 構文、#140)
```

v0.6〜v0.7で入れたもの (実 OSS での精度と monorepo 対応)。

```text
- --fix の安全化 (stale な行番号・配列 index をエラーにする、改行コード保持)
- Rust library API の整理
- 宣言のない monorepo の nested pyproject.toml を workspace member として扱う (--no-auto-workspace)
- library mode の member symbol 判定と、宣言済み public API を CHK006 / CHK007 から外す
- in-package tests / vendored code を報告対象から分離 (vendored 設定)
- 動的 setup.py の読み取りと、依存不明時の CHK003 降格
- 定義元 module 内で読まれる symbol を CHK006 から外す (library mode と、app mode の `__all__` 外)
- root の conftest.py / pytest testpaths の探索、文字列 annotation 内の名前の読み取り
- CHK008 を command word に、CHK009 を同一 context の重複に絞る
- 接頭辞・接尾辞付き distribution 名の照合、PEP 508 / 440 の受理を packaging に揃える
```

v0.8で入れるもの (Knip 相当の運用性)。

```text
- framework / format plugin 拡充 (Django 周辺 / Typer / Click / Streamlit / Airflow / Dagster / marimo など)
- configuration hints (未使用 ignore / stale baseline / 空振り glob / 未使用 map entry)
- --workspace / --max-issues / --performance
- codeclimate reporter、chokkin 自身の pre-commit hook / GitHub Action、[tool.chokkin] JSON Schema
- public API 宣言 (__all__ / pragma / public 設定) と symbol 単位 trace
```

v1.0で安定させるもの。

```text
- plugin API (外部loading; v0.3ではRFCのみ。v1.0 は process boundary)
- stable JSON schema (v0.3で schema_version + JSON Schema 公開済み)
- stable exit code
- stable ignore syntax (v0.3で回帰テスト固定)
- safe autofix contract (ADR 0003)
- large monorepo performance
- editor/LSP連携 (--watch / LSP / MCP server)
```

v0.3 で実装済み: `[tool.chokkin.severity]` による rule severity override、SARIF rule metadata 安定化。

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

検証ハーネスは `scripts/oss-clones.manifest` (20 project、commit SHA pinned) と
`scripts/clone-oss-fixtures.sh` / `scripts/oss-metrics.py` (`make oss-clones` /
`make oss-metrics`)。`findings.tsv` は CHK001–CHK010 全 finding を JSON
`target` キーで出力し、`report.md` に per-rule label coverage 表を載せる
(Phase 3.x Step 0、2026-08-11 完了。棚卸しは `docs/dev/v0.3-stocktake-coverage.md`)。
誤検知の ground truth は `scripts/oss-fixtures.labels.tsv` に `fp`/`tp`/`deferred`
で記録し、**CHK002** の未分類 (unknown + deferred) が残ると FP gate は通らない。
manifest の app mode CHK001 corpus の節 (CHK001 を測るために足した Django app、
#1065) の CHK002 は FP 率を記録するだけで gate に入れない。その誤検知の大半は、
settings が文字列で名指す backend (`STORAGES`、`CACHES`、`DATABASES` の `ENGINE`、
`PASSWORD_HASHERS`) と、docker や shell からだけ起動する CLI の依存で起きる。どちらも
chokkin がまだ読まない形なので、gate に入れると既知の gap を測ることになるため。
orchestration plugin の節 (Dagster / Prefect / Scrapy の利用側 project、#1057) も
同じ理由で記録だけにする。誤検知は `profiles.yml` の `type:` が名指す dbt adapter と、
`dg dev` が起動する `dagster-webserver` で、どちらも chokkin がまだ読まない。
最新の §17 CHK002 scorecard は `docs/dev/oss-validation-report.md` に残す。

severity が `info` の finding (例: optional try-import の CHK003) はヒットとして
数えない。期待どおり info になるものは `info-expected` ラベルで記録し、hit に昇格
した場合は `fp` 扱いになる。CHK001 / CHK004 / CHK006 / CHK010 は件数が大きいため
`scripts/sample-precision-labels.py` で project ごとに上限付きで層別サンプリング
(rule あたり約 50 件、project あたり最大 6 件、順位は sha256(slug, code, target)
で固定) し、人手で `tp`/`fp` を付ける。上限は上限より前に付けたラベルにも効かせる:
`--prune` は project ごとに hash 順で最初の上限件数だけを残し、残りのラベル付き
ヒットを消す (#1065)。1 project が全体の precision を決めないようにするため。
`report.md` は rule ごとの precision (tp / (tp + fp)) と、今回の findings に
現れなくなった **stale** ラベル数を記録する (#656)。precision は info を除くヒットのうち `tp` / `fp` のラベルが付いたものだけで
計算し、**CHK001 は 25%、CHK003 は 90%、CHK004 は 95%、CHK006 / CHK010 は 90% を
下限**として gate にかける (#701、#1065)。件数の gate では、件数を変えずに誤検知が増える精度低下を検出
できないため。下限は計測時の precision を 5 の倍数に切り下げた値。CHK002 は上の
FP 率 gate (未分類 0 件で FP 率 < 5%) がそのまま precision > 95% の下限なので、
重ねて置かない。ラベル付きのヒットが 0 件の rule は測れないので通す (何も出さなく
なった rule は recall gate が捕まえる)。CHK001 は既定では app mode でしか出ないので、
ラベル付きの app mode project が 8 本以上そろってから下限を置いた。1 project の
慣習 (airflow の DAG file、#1062) で gate が決まらないようにするため。下限が低いのは、
Django の URLconf の `include("pkg.urls")` が import edge にならず、app の module の
多くが到達不能に見えるため。下の oracle でも測る (#1065)。CHK005 / CHK007 / CHK008 / CHK009 はラベル付きのヒットがまだ少ないので
記録のみにする (#862)。既定で隠れる `maybe` の CHK001 は `--maybe` で `--strict`
の report から別 pool として抽出し (`scripts/oss-maybe.labels.tsv`)、そのうち本当に
dead な割合 (隠したことによる FN 率) を記録だけする。clone の revision を上げて
stale が出たら該当行を消し、同スクリプトを再実行してサンプルを補充する。計測は
キャッシュの影響を避けるため `--no-cache` で行う。

CHK001 の実世界 precision は opt-in の remove-and-test oracle
(`scripts/oss-remove-and-test.py` / `make oss-oracle`) で測る。flag されたファイルを
使い捨てコピーで削除し、project 自身の pytest を baseline と比較する。解析対象コードを
実行するため chokkin 本体・release・既定 CI からは呼ばない
(`docs/dev/chk001-remove-and-test.md`)。

FP gateは「報告された CHK002」を分母にするため、**何も報告しなければ自明に通る**。
これを塞ぐため、ハーネスは `scripts/oss-recall.manifest` の **recall sentinel**
(意図的に未使用依存を含む in-repo fixture) を OSS clone と一緒に計測し、`tp`
ラベルが findings に現れなければ **recall gate を失敗**させる (`pass_recall`)。
これが Phase 1.5 の誤検知是正が「全件抑制」へ退化していないことの保証になる。

manifest 末尾の **failure-pattern corpus** (#495: sqlalchemy / streamlit /
llama_index / transformers / openai-python / langchain、commit SHA pinned) は
誤検知や見落としが起きやすい構造の回帰検知に使う。`scripts/oss-expectations.tsv`
に project ごとの `reachable_runtime` の下限 (直近の実測の 90%) と rule 別 issue 数を
固定し、下限割れか rule 別件数が base + max(5, base/5) を超えると expectations gate が
失敗する。exit 2 (CLI/config error) も gate で 0 件を要求する。この corpus の CHK002
も他の project と同じくラベル付けを必須とし、FP 率の分母に入れる (#529)。

cold 実行の基準は medium project の 2s だけで、large project の遅化はどの gate
にも掛からなかった (v0.7.4 / v0.7.5 の transformers 5.7 倍、
`docs/dev/v0.7.x-uvx-regression-report.ja.md` §5)。`oss-metrics.py --baseline` は
同じマシンで測った baseline と medium / large project ごとの `median_ms` を比べ、
**1.5 倍以上かつ 500 ms 以上**遅い project を cold 3 回で測り直し、なお両方を超えれば
gate を失敗させる (#1025)。片方だけでは足りない理由は、同じバイナリの `-r 1` 2 回で
project ごとに最大 1.13 倍、release 間の regression でない遅化も 1.3 倍未満
(v0.7.0 → v0.7.5 の sqlalchemy +26%) 揺れ、数十 ms の project では比が意味を持たない
ため。small project と recall sentinel は対象外、どちらかの run で exit 2 / 3 の
project は crash / config error の基準に任せる。比べる相手は直前の release で、
`oss-metrics.yml` の baseline (v0.4.1、CHK003 件数の基準用) は解析範囲が今より
ずっと狭い (llama_index 660 → 2,210 ms) ので、CI は `RELEASE_REV` (直前の release)
のバイナリを `--time-bin` で渡し、medium / large project ごとに HEAD と交互に測る。
続けて別々に測ると、マシンの状態の変化がそのまま差に乗る (同じ main を v0.7.7 と
比べて large 合計 1.15 倍 / 1.09 倍)。交互なら 5 回で 1.10〜1.14 倍、同じバイナリ
同士で 3 回 1.00〜1.02 倍だった。

project ごとの基準は 1 release で +10% ずつ遅くなっても毎回通る。v0.7.8 では large
project 15 件中 13 件が 7〜24% 遅くなったが、どれも掛からなかった (#1088)。そこで
large project の `median_ms` を両方の run で終わったものについて合計し、HEAD の
合計が time baseline の **1.05 倍**を超えれば large project を cold 3 回で
(`--time-bin` では両方のバイナリを交互に) 測り直し、なお超えれば gate を失敗させる。
1.05 倍は上の雑音 (最大 1.02 倍) と既知の退行 (最小 1.10 倍) の間に置いた。medium / large project で 1.25 倍以上かつ 100 ms
以上遅いものは `compare.md` に warning として並べる (gate にはしない)。
`RELEASE_REV` は tag を打った後の `ci:` PR で新しい release に移す。意図した遅化は
`CHANGELOG.md` に書き、その PR の merge commit に `RELEASE_REV` を付け替える。

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
     - PEP 735 [dependency-groups]・Poetry/PDM/Hatch group・requirements-*.txt を
       dev contextとして解釈する。
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
    GitHub Actions は single-line `run:` と block scalar `run: |` / `run: >` に対応し、
    `python -m pytest` のような module invocation も既知binary利用として扱う。
    notebook parsing は `.ipynb` discovery と Python code-cell extraction を初期実装済み。
    Flask/Celery decorator由来 module refs は step 6 の decorator sites から抽出する形で初期実装済み)
  - JSON reporter / baseline draft schema と migration 方針 (`docs/dev/schema-migration-notes.md`)
exit   : 10k files級monorepoでwarm 2s以内、baseline運用でCI導入事例を作る
```

### Phase 3: v0.3〜v0.x 安定化(継続) — ✅ v0.4.0 リリース済み

```text
目標   : v1.0で凍結する契約の準備
v0.3   : Contract Stabilization
v0.4   : CHK003 Reliability + Contract Formalization
成果物 :
  - JSON reporterに schema_version を追加し、JSON Schemaを公開する ✅
  - baseline fileに schema_version を追加し、v0.2 draft baseline reader互換を維持する ✅
  - SARIF rule metadata (help text / help URI / default severity) を安定化する ✅
  - rule severity override設定を追加する ✅
    (例: CHK006 = "info" / CHK001 = "off" / CHK002 = "error")
  - inline/file-level ignore構文 (`# chokkin: ignore[...]` /
    `# chokkin: file-ignore[...]`) をdraft凍結する ✅
  - plugin API RFCを作り、外部plugin試作のI/O境界を文書化する ✅
  - default CHK003をruntime import中心にし、conditional importをinfo候補にする ✅
  - alias付きTYPE_CHECKINGを追跡し、parse cacheをversion更新する ✅
  - wheel metadataのoffline deterministic harvest flowを追加する ✅
  - safe autofix / semver契約をADR 0003 / 0004で明文化する ✅
  - 誤検知報告から package-module-map / binary map / plugin database を継続更新する
非目標 :
  - v0.3では外部plugin loadingを安定APIとして公開しない
  - v0.3では新規大規模検出より契約安定化を優先する
exit   :
  - v0.2 JSON / baselineを読み続けられる ✅
  - v0.3 JSON Schemaがsnapshot/regression testで固定される ✅
  - severity overrideがconfig / reporter / exit codeに反映される ✅
  - make check と make oss-metrics ARGS=--gate が合格する
  - v1.0条件「2 minor version連続でbreaking changeなし」の起点をv0.3にする
```

### Phase 4: v0.5 モダン packaging 追従(+6〜8週) — ✅ v0.5.0 リリース済み

```text
目標   : 2026年の典型的な uv project (dependency-groups / inline script / lockfile) で
         zero-config の誤検知・取りこぼしをなくす
背景   : Phase 3 までで Knip の中核 issue 種別 (files / dependencies / unlisted /
         binaries / unresolved / exports / duplicates) は揃った。残る誤検知源は
         「chokkin がまだ読まない manifest・設定・構文」に移っている。
成果物 : (ID は docs/dev/roadmap-gap-analysis.ja.md §3)
  - R-01 PEP 735 include-group 展開
  - R-02 PEP 723 inline script metadata (script 単位の CHK002 / CHK003)
  - R-03 pylock.toml / poetry.lock / pdm.lock を CHK004 の transitive 判定に使う
  - R-04 [tool.uv] sources / constraint / override / default-groups
  - R-05 [build-system].requires の build context と wheel target の public surface
  - R-06 binary / plugin usage 情報源の拡充 (PDM scripts / Makefile / justfile /
         Dockerfile / Procfile / GitLab CI / pytest addopts / pytest11 / mypy plugins)
  - R-07 依存宣言・設定ファイル存在からの plugin 自動有効化 (Knip enablers 相当)
  - R-14 parser 再選定の ADR 0001 改訂 (移行先 `ruff_python_parser`、2026-09-25)。
         PoC (#320) と 7 target の wheel build (#321) の実測で pin を `=0.0.15` に確定
検証   : OSS corpus に uv-native / PEP 723 / 各 lockfile / PEP 695 project を追加し
         (gap analysis §4)、recall sentinel に R-01〜R-05 の fixture を足す
exit   : 拡充 corpus で CHK002 誤検知率 5%未満 (未分類0)、recall sentinel 全件検出、
         crash 0、cold 実行 medium 2s 維持、CHK003 件数 (info severity と tp label
         を除く、#773) が Phase 4 着手前 (commit `573ff37`) 比で増えない
状態   : exit 達成 (PR #367、docs/dev/oss-validation-report.md)。CHK002 誤検知率
         3.7% (1/27、未分類0)、recall 48/48、crash 0、CHK003 288 → 283
         v0.5.0 で再測定し合格 (docs/dev/v0.5-release-validation.md、CHK003 288 → 170)
```

### Phase 4.5: v0.6〜v0.7 実 OSS での精度と monorepo 対応 — ✅ v0.6.0 リリース済み

```text
目標   : Phase 5 に入る前に、§17 corpus と failure-pattern OSS で見つかった誤検知・
         取りこぼしを潰す
背景   : v0.5.0 後の実測で、--fix の誤削除、宣言のない monorepo、library の公開 API、
         package 内 tests / vendored code が主な誤検知源だった。Knip 相当の運用性
         (Phase 5) より先に直す
成果物 :
  - v0.6.0: --fix の stale 行番号・配列 index 検出、requirements の改行コード保持、
            Rust library API の整理 (#436-#438, #446)
  - v0.7.0: auto-workspace (#488)、library mode の member symbol 判定と宣言済み
            public API (#489, #525)、in-package tests / vendored (#490)、
            動的 setup.py (#539)、path source / extras / metapackage の依存判定、
            CHK006 の used_in_module (#540, #564)、root conftest.py / testpaths
            (#544)、文字列 annotation (#545)、CHK008 / CHK009 (#494)、
            marker 付き uv path source (#508)
exit   : JSON schema / SARIF / exit code の既存契約を壊さない (Rust library API の
         breaking は ADR 0004 の対象外)、Phase 4 の gate を維持
```

### Phase 5: v0.8 Knip 相当の運用性(+6〜8週)

```text
目標   : 導入・抑制・説明・CI 連携を Knip と同じ水準にそろえる
成果物 :
  - R-08 framework / format plugin 拡充 (Django templatetags / management commands /
         admin / AppConfig.ready、Typer / Click、Streamlit、Airflow / Dagster /
         Prefect、Scrapy、Litestar、marimo、Jupytext)
  - R-09 configuration hints (未使用 ignore directive / stale baseline fingerprint /
         未使用 map・ignore entry / 空振り entry・project glob)。exit code には
         影響させず、明示フラグで error 扱いにできる
  - R-10 --workspace <member> / --max-issues N / --performance
  - R-11 codeclimate reporter、chokkin の pre-commit hook と GitHub Action、
         [tool.chokkin] JSON Schema (SchemaStore 登録)
  - R-12 public API 宣言 (__all__ / `# chokkin: public` / `public` 設定)、
         entry file export の検査、`--trace path:symbol`
  - R-13 Import-Name / Import-Namespace を持つ wheel の harvest を bundled map 更新
         pipeline に組み込む
  - R-14 parser 移行の完了 (Python 3.14 t-string / PEP 758 を parse できる)。
         backend 差し替え (`ruff_* =0.0.15`、MSRV 1.96) → PEP 695 / 750 / 758 / 810
         対応の 2 段階 (ADR 0001)
exit   : 追加した CLI flag・hint・reporter が JSON schema / SARIF / exit code の
         既存契約を壊さない (v0.3 起点の「2 minor 連続 breaking なし」を継続)、
         Phase 4 の gate を維持
```

### Phase 6: v0.9 検出範囲の拡張(preview)

```text
目標   : v1.0 で凍結しない preview rule と editor 連携の試作
成果物 :
  - R-15 依存宣言の整合性 (stdlib を依存宣言 / uv sources・constraint・override の
         stale entry)
  - R-16 unused class / enum members (app mode 限定 preview)
  - R-17 型文脈専用 export の区別 (Knip types 相当)
  - R-18 --watch と LSP の試作 (incremental 解析、--fix を code action で提供)
  - R-20 conda environment.yml / pixi.toml の pypi 依存
  - R-21 duplicate exports (Knip duplicates 相当。同じ object の複数名再 export)
  - R-22 循環 import (Knip cycles 相当、既定 off)
非目標 : preview rule を default on にしない。JSON schema の rule 一覧には載せるが
         semver 契約 (ADR 0004) の安定対象に含めない
```

### Phase 7: v1.0(条件達成次第)

```text
目標   : §16 v1.0 list の安定性保証
内容   :
  - stable JSON schema / exit code / ignore syntax / safe autofix contract
  - ADR 0003 / 0004で合意したsafe autofix / semver契約を安定保証として運用する
  - R-18 LSP / MCP server の正式提供
  - R-19 外部 plugin loading (ADR 0002 の process boundary)
  - large monorepo performance の最終チューニング (parser 移行後の cold parse 再計測)
条件   :
  - v0.3 起点で 2 minor version 連続 breaking change なし
  - Phase 4 の拡充 corpus で §17 gate が合格
```

### 横断work(全phase継続)

```text
- package-module-map / binary map のデータ収集と自動生成pipeline
  (Import-Name metadata を持つ wheel の harvest を含む)
- 誤検知報告template (--explain出力の添付を必須にする)
- 検証用OSS project setの拡充とregression test化
  (20件のharnessは `scripts/oss-*` / `make oss-metrics` として整備済み。
   各リリース前に再計測し `docs/dev/oss-validation-report.md` を更新する。
   Phase 4 で uv-native / PEP 723 / lockfile 別の project を追加する)
- Knip と Python packaging 仕様の追従 (新しい issue 種別・PEP を
  `docs/dev/roadmap-gap-analysis.ja.md` に反映し、優先度を見直す)
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

config ignore。keyはrule codeに統一する。値のpattern構文はruleの対象に依存し、dependency系ruleではdistribution名のglob、file系ruleではpath glob、symbol系ruleでは `path:symbol_glob` 形式とする。distribution名のglobは、glob記号を残したまま PEP 503 で正規化してから正規化済みの distribution 名と照合するので、manifest と同じ綴り (`typing_extensions`、`Boto3`) でも効く (#1106)。`[...]` の文字クラスは書いたとおりに残す (`[!A-Z]` を小文字にすると意味が反転する)。PEP 723 script の `script:<path>:<name>` は name の部分だけを正規化する。CHK008 の binary 名は書いたとおりに照合する。

inline / file-level ignore と vendored は、複数の import site をまとめた finding (CHK010 の import 行、CHK005 の distribution ごとの runtime import) では、まとめた全 site が抑制されるときだけ抑制する。CHK005 は ignore された site を重み付けの前に除き、残った site のうち最も強いものを位置と confidence にする。ignore 済みの top-level import が、ignore していない関数内 import を certain に引き上げることはない。全 site が ignore されているときだけ finding は ignore 済みの site に立ち、抑制される (#1107)。

```toml
[tool.chokkin.ignore]
CHK001 = [
  "src/acme/migrations/**/*.py",
  "src/acme/generated/**/*.py",
]
CHK002 = ["boto3", "google-cloud-*"]
CHK003 = ["setuptools"]  # module名 (pkg_resources) ではなく distribution名
CHK006 = ["src/acme/public_api.py:*"]
```

baselineも導入しやすさに効く。

```bash
uvx chokkin --baseline chokkin-baseline.json --update-baseline
uvx chokkin --baseline chokkin-baseline.json
```

baselineは既存issueを黙らせ、新規issueだけCIで落とすためのもの。大規模既存projectでは必須。fingerprintは `rule_id + stable target` を基本とし、workspace member付きissueでは stable target に `member:` prefix を含める。file pathは `/` 区切りに正規化し、dependency/file issueではline numberをkeyに含めない。

## 19. パフォーマンス目標

Rustで作るなら、体感はRuff寄せにする。サイズの目安は、small ≈ 100 files以下、medium ≈ 1,000 files、large ≈ 10,000 files以上とする。

```text
small project      < 300ms
medium project     < 2s
large monorepo     < 10s cold
large monorepo     < 2s warm cache
```

cache は project root 配下の固定 directory `.chokkin/cache`（`DEFAULT_CACHE_DIR`）に置き、project root 外へ書き出さない。`--no-cache` は cache read/write を両方無効化し、stale疑いのcache unitが解析結果を変えないようにする。v0.2初期は `CacheOptions` のpolicyを先に通し、その上に parse / manifest extraction / generic config scanの各unitを保守的に追加した。parse cache のkeyは `CacheKeyContext` と `SourceFingerprint` を組み合わせる。`SourceFingerprint` は既定で `stat` のみを読み、`(size, mtime)` が一意ならcontent hashを空のままにする。mtimeが取得できない場合と、mtimeが直近2秒以内（coarse mtime粒度でのfalse hitを避ける racy window）の場合だけfile bytesを読んでstable content hashを計算する。「直近」の基準はローカルの時計ではなくfilesystemの時計で、run開始時に `.chokkin/cache/clock` へprobeを書いてそのmtimeを使う（`CacheOptions::filesystem_now`）。NFS/SMBのようにfilesystemの時計が遅れていても、書いた直後のsourceをsettledと誤判定しないため。probeを書けない場合（read-only等）はローカルの時計に戻る。これによりwarm runで全sourceを読み直さずにkeyを組み立てられる。`--no-cache` ではkey自体を作らないので、stat・hashも行わない。parse cache はdisk bundleだけで、in-memory storeは持たない（1 runで各sourceを1回しかparseしないため）。disk永続化は `.chokkin/cache/parse/bundle-<context hash>.json` に `CacheKeyContext` 単位の `ParseCacheBundle`（`ParseCacheKey::entry_id()` をkeyにした `ParsedModule` のmap）をまとめて保存する。bundle は run開始時に1回読み、run終了時に1回だけatomic writeするため、cold runのfile I/Oはfile数に比例しない。書き戻すのはそのrunで実際に触れたentryだけなので、変更・削除されたsourceのparse結果はbundleから落ちる。書き戻しはparseが発生したか、entry idの集合がbundleと異なる場合に限る。entry idは64 bit hashなので、bundleから返す前に `ParsedModule::path` とkeyのpathを照合し、不一致（衝突）はmiss扱いにする。bundleから読んだ `ParsedModule` はcloneせずmoveで取り出し、書き戻しは借用のまま（`ParseCacheBundleRef`）直列化するので、disk warm runでは `ParsedModule` のcloneが発生しない。bundleを書いた時点で `parse/` 内の他の `*.json`（別contextのbundleや旧形式）を削除する。削除はbest-effortで、失敗しても解析は失敗させない。拡張子のない一時fileは並行runのatomic write中のものなので残す。corrupt JSON はmiss扱いにしてsourceを再parseする。並列parseで読めないsourceが複数あるときは、discovery順で最初のもののエラーを返す。

parse cache keyは以下を使う（`CacheKeyContext` + `SourceFingerprint`）。

```text
chokkin version
config hash         # effective globs の hash
manifest hash       # layout (`LayoutInfo::cache_key_hash`) の hash
python target version
unit version        # parse-v11。ParsedModule の形や key 規則を変えたら上げる
file path
file size
file mtime
file content hash   # mtime 不明か racy window 内のときだけ。それ以外は空
```

lockfile や plugin の version は parse の出力に影響しないため key に含めない。

config/manifest scan cache は `ScanInputFingerprints` で、実際に読んだ config file と manifest file の `SourceFingerprint` を保持する。uv workspace だけを持つ `pyproject.toml` も config input として扱う。requirements の再帰読取り結果は `ManifestSources.requirements_files` を通じて fingerprint 対象にする。`ScanCacheKey` と `ScanCacheRecord` は `.chokkin/cache/scan/<unit version>/<key>.json` のmetadata envelopeとして使う。scan recordはinput fingerprintをkeyに含むため入力を編集するたびに新しいfileになる。書き込み時に同じunit directoryの他の `*.json` と、旧形式の `scan/*.json` を削除し、unitごとに最新の1件だけを残す。`read_scan_payload` / `write_scan_payload` が typed payload を record に直接埋め込んで読み書きする disk backend は初期実装済みで、corrupt JSON、key mismatch、schema_version mismatch、payload shape mismatch はmiss扱いにする。generic config scanner (`ConfigScanResult`) と full manifest extraction (`LoadedManifest`) のpayload wiring は初期実装済み。manifest payload は extraction 後に判明する recursive requirements 入力 fingerprint も payload 内に保持し、cache hit 時に再検証して stale hit を避ける。`-r` / `-c` の解決で探したが存在しなかった候補パスは `ManifestSources.requirements_missing` に残し、不在 fingerprint として同じ inputs に含める。これにより、後から作られた制約ファイルや include 元ディレクトリ側のシャドーイングファイルも hit 時に検出する。module index は cache しない。graph のパス列に対する文字列ループで毎回構築する方が、全パスを hash する cache key の計算より安いため。

parallelize対象は、file discovery、parse、import extraction、symbol extraction、plugin config parse。graph resolutionだけは集約後に行う。

`benches/pipeline.rs` は約 2KiB の Python module（関数・ループ・条件式）を使い、
`analyze_cold` / `analyze_warm`、`parse_cold_no_cache` /
`parse_cold_with_disk_cache` / `parse_disk_warm`、`reachability`、
`discover_populated_cache` を測る。
通常は 1k modules、`CHOKKIN_BENCH_LARGE=1 cargo bench --bench pipeline` で
5k / 10k も追加する。cold は解析 cache が空の状態であり、OS page cache は消さない。
fixture 作成・cache 削除・graph clone は計測外。warm parse は CLI と同じ disk reuse を測る。
同じ target の `workspace_members/analyze_no_cache` は、`sdist` / `wheels` 行を持つ
`uv.lock` 付き nested member を 200 個並べた monorepo（llama_index の integrations 型）を
cache 無効で解析し、member 数に比例する probe / resolution の退行を拾う（#513）。
`uv.lock` は byte の大半を占める `sdist` / `wheels` 行を落としてから TOML parse する。全群は `make bench-save` / `make bench-cmp` の対象になる。

warm cache の性能確認は `benches/cache.rs` の `parse_cache_warm`（disk bundle からの warm parse、100 / 1k / 5k / 10k files）を使う。`make bench` は Criterion の全bench（`manifest` / `sources` / `cache` / `resolver` / `pipeline`）を走らせ、baseline比較は `make bench-save BASELINE=main` → `make bench-cmp BASELINE=main` で確認する。PR では `verification.yml` の `bench-gate` job が、同じ runner で base commit の baseline を保存してから `scripts/bench-gate.py --confirm 3` で head と比べ、結果を step summary に出す（runner のノイズを見て `--threshold` を詰めるまでは non-blocking、#836）。base に無い bench は比較せずに走らせる。2026-06-15 の v0.2 release validation 実測では 10k warm cache median が 186.85–204.21 ms で、large monorepo の <2s 目標を満たした（当時の値は in-memory store 経由。in-memory store 削除後の disk warm は 10k で約 61 ms）。baseline CI 導入事例は chokkin repo 自身の dogfood job (`.github/workflows/ci.yml` の `chokkin-baseline`) と checked-in `chokkin-baseline.json` で記録した (`docs/dev/v0.2-release-validation.md`)。

tox/nox/pre-commit/GitHub Actions は v0.2 plugin 拡充の初期実装として `src/plugins/devtools.rs` に集約し、`tox.ini` / `noxfile.py` / `.pre-commit-config.yaml` / `.github/workflows/*.yml` または対応する `[tool.*]` から binary usage を出す。GitHub Actions は single-line `run:` と block scalar `run: |` / `run: >` を `src/plugins/commands.rs` の共通 command 解析に通す (`python -m <module>` は `<module>` が既知binaryなら利用として扱う)。

R-06 で binary / plugin usage の情報源を拡充した。共通の command 解析 (`\` 継続の結合、`;` `|` `&` と改行での分割、`@` `-` / `exec` `env` などの wrapper、`uv run` / `poetry run` などの runner、`python -m` の剥がし、`$…` や `{{…}}` で始まる語は変数展開として追わない、pip / uv などの環境管理ツールは binary 扱いしない、command 位置の先頭語だけを binary とみなし引数は読まない) は `src/plugins/commands.rs`、行番号付きで TOML / INI / YAML を読む補助は `src/plugins/config_text.rs` に置く。

- `src/plugins/task_files.rs`: `[tool.pdm.scripts]` (文字列 / `cmd` 文字列・配列 / `shell` / `composite`、`call` は module reference、`_` は無視)、`GNUmakefile` / `makefile` / `Makefile` の tab 付き recipe 行、`justfile` / `Justfile` / `.justfile` の recipe 本体 (`sh` 以外の shebang recipe は除外)、`Dockerfile` / `Containerfile` / `*.Dockerfile` / `Dockerfile.*` の `RUN` / `CMD` / `ENTRYPOINT` (shell form と exec form。root の file は全部、root より下の file は server の app だけ)、`Procfile`、`.gitlab-ci.yml` の `script` / `before_script` / `after_script` (list・inline 配列・block scalar、`!reference` は無視)。`[tool.poe.tasks]` (文字列と配列は `[tool.poe]` の `default_task_type` / `default_array_item_task_type` に従う。`cmd` / `shell` は command、`script` は module reference、`expr` の `imports` は module、`sequence` / `parallel` / `switch` は中の task を読み、`ref` などそれ以外は無視)、`[tool.hatch.envs.<env>.scripts]` (文字列・配列、先頭の `- ` は外す)。`hatch.toml` と poe の `include` は読まない。Makefile / justfile の変数・include は追わない。
- `src/plugins/config_scan.rs` (generic scanner): `tox.ini` / `[tool.tox]` / `tox.toml` は `commands*` キーの値だけを command として読み (`{envpython}` は python、`{envbindir}/x` は x、他の `{...}` 置換は落とす)、`deps` / `description` / `allowlist_externals` のような一覧の語は command ではないので拾わない。`.pre-commit-config.yaml` は hook `id` と `repo: local` の `entry` だけを読む。`deps` の distribution 名 (`-r` や `{[env]deps}` 参照は除く) と `noxfile.py` の `session.install(...)` の文字列リテラル (`-` で始まる option は除く) は CHK008 の provider 宣言として集める。`scripts/` / `bin/` 配下は `.sh` 拡張子か sh 系 shebang を持つ file だけを shell script として command 解析し (`#` コメント以降は無視)、Python script は parser が読む。いずれも語が binary map にあるだけでは usage にしない (#494)。
- `src/plugins/tool_plugins.rs`: pytest `addopts` (`[tool.pytest.ini_options]` / `pytest.ini` / `tox.ini [pytest]` / `setup.cfg [tool:pytest]`) の `-p mod` を module reference (`-p no:x` は無視)、`--cov` / `-n` / `--benchmark-*` などの plugin option を `src/plugins/plugin_map.rs` の表で distribution に対応づける。mypy `plugins` (`[tool.mypy]` / `mypy.ini` / `.mypy.ini` / `setup.cfg [mypy]`) は module reference (`mypy_django_plugin` → `django-stubs` などは表で対応づけ)、`[tool.ty]` / `ty.toml` / `[tool.pyright]` / `pyrightconfig.json` / `[tool.basedpyright]` は該当 type checker を used にする。root と各 workspace member の `pyproject.toml` の `[tool.datamodel-codegen]` の `custom-formatters` (datamodel-codegen は key の `-` を `_` にして読むので `custom_formatters` も) は、tool が code 生成中に import する module 名の配列として読む (#1063)。名前が import root で解決できればその file、できなければその pyproject の directory の下で同じ名前の module (`x.py` か `x/__init__.py`) が 1 つだけあればその file を、dev の command target (#912) にする。それ以外 (配列でない値、文字列でない要素、dotted identifier でない名前、該当する file が無いか複数ある名前) は opaque な command target にし、#913 の UnreadCommandTarget gap で CHK001 を likely 以下にする。該当する file が project に無い名前は install 済みの formatter かもしれないので module reference にもする (providing distribution を used にする)。`[tool.datamodel-codegen.profiles.*]` は読まない。member の pyproject と inventory で結果が決まるので、config-scan cache の外で毎回読む。
- `.venv` の `pytest11` entry point を持つ distribution は pytest 自体が used のとき used とする (`ResolutionIndex.pytest_plugin_distributions`)。venv が無くても読めるよう、宣言名が `pytest-` で始まる distribution も plugin の命名規約に従うとみなして同じく used にする (#809)。
- 読むファイルはすべて (member の `[tool.datamodel-codegen]` を除き) config-scan cache key (`scan_input_paths`) に入り、`--explain` の evidence は `file:line` で origin を示す。

Flask/Celery は `src/plugins/flask.rs` と `src/plugins/celery.rs` で初期実装し、`.flaskenv` の `FLASK_APP`、script内の `flask --app`、`project.scripts` / scripts / bin にある `celery -A` / `celery --app` から symbol reference と binary usage を出す。値を囲む shell の引用符は外し、Flask の `module:factory(args)` は `factory` を指すとみなす。コロンの無い dotted module (`celery -A pkg.mod`、`FLASK_APP=pkg.mod`) は module reference として entry root にし、両ツールが名前で探す属性 (Celery は `app` / `celery`、Flask は `app` / `application` / `create_app` / `make_app`) を entry root を増やさない used symbol (`PluginContribution.used_symbols`) にする。Celery は値を `pkg` と属性 `mod` としてまず解決するため、`pkg` の `mod` も used にする (#851)。Flask の値が `.py` で終わるか `/` / `\` を含むときは、`flask.cli.prepare_import` と同じくファイルパスとして project root から読む (`hello.py`、`src/hello`、`src/hello.py:app`。`path:attr` は Flask の `ScriptInfo` と同じく区切り文字が続かない最初の `:` で分ける)。`<p>/__init__.py` と `<p>.py` の順に discovered source を探し、layout が module 名を与えるファイルなら module 形と同じ reference にする。module 名の無いファイル (package の無い project の root にある script など) は Flask が自分のディレクトリから import するので、そのファイルを、そのファイルの context の entry root にする (`scripts/app.py` は dev の entry root で、mode 推定の app signal にならない) (#866)。空の属性 (`hello.py:`) は Flask では import error になるが、属性無しとして読む。`prepare_import` は module 名でも作業ディレクトリを `sys.path` に入れるので、module 形 (`--app hello`) も layout が module 名を与えないファイルを指すときは同じく entry root にする。discovered source を指さないパス、絶対パス、root の外に出るパス、変数を含むパスは何も足さない。command target と同じく、何も指さない値は project のファイルを隠さないので gap にもしない。Celery の `-A` はパスを受け付けないので module 名としてだけ読む。`$APP` など dotted identifier でない値は読まない。Flask は literal route decorators (`@app.route("/")`, `@bp.get(...)` など、receiver 付きの呼び出し形のみ。bare の `@app.route` は対象外)、Celery は literal task decorators (`@shared_task`, `@app.task` など) を持つ module を module reference として扱う。step 6 の parse 結果の decorator site を使い、構文エラー（`ParseSeverity::Error`）のある module では行単位のテキスト走査にフォールバックするが、どちらも同じ正規化名と判定関数で判定する。

Sphinx/MkDocs は `src/plugins/doctools.rs`、Alembic は `src/plugins/alembic.rs` で実装し、Sphinx の conf.py を plugin entry にし、`mkdocs.yml` / `mkdocs.yaml`、Sphinx の conf.py、root の `alembic.ini` から binary usage を出す。Alembic は `alembic.ini` (root、各 workspace member 直下、`__init__.py` のある各 package ディレクトリ) の `[alembic] script_location` / `version_locations` と、root と各 member の `pyproject.toml` の `[tool.alembic]` を静的に読む (#667)。`%(here)s` は設定ファイルのディレクトリ、`pkg.sub:dir` は package ディレクトリからの path、それ以外の相対 path は設定ファイルのディレクトリ、無ければ root (alembic を実行する場所) から解決する (`%(here)s` を含む path は root に戻らない)。`version_locations` は `path_separator` (1.16) / `version_path_separator` に従って分割し、未指定なら空白とカンマで分ける。`<script_location>/env.py` と `<script_location>/versions/*.py` (`version_locations` があればその直下) のうち探索済みのファイルを、そのファイルの context の plugin entry にする。サブディレクトリの revision は `recursive_version_locations` が真のときだけ含める。設定から entry が 1 つも出ないときは従来どおり `alembic/env.py` を dev context の entry にする (auto entry の `alembic/env.py` は設定と無関係に残る)。Sphinx `extensions = [...]` と、その後の top-level の `extensions.append("x")` / `extensions.extend([...])` / `extensions += [...]` の literal string は module reference として扱う (#719)。右辺や引数が名前なら、`docs/conf.py` が top level の `from m import NAME [as alias]` で取り込んだ first-party module の `NAME = [...]` (文字列か文字列 list の literal) を読む (airflow の `extensions = BASIC_SPHINX_EXTENSIONS`、#779)。import 先の定数がさらに別の名前を参照する場合は追わない。conf.py 自身が top level で代入した定数 (`EXTS = [...]`) も同じく読み、import より優先する。`extensions.insert(i, "x")` も `append` と同じく読む。conf.py が top level で `from X import *` し、X が first-party module なら、X の top-level の `extensions` の代入・`append` / `extend` / `insert`・`+=` も読み、conf.py の名前と和集合にする (#1062)。airflow の各 provider の `docs/conf.py` は `from provider_conf import *` だけで、`extensions` は `devel-common/src/docs/provider_conf.py` が組む。X の中の名前は X の top-level の代入と X の import (`from docs.utils.conf_constants import BASIC_SPHINX_EXTENSIONS`) で 1 段だけ解決する。`extensions = BASIC_SPHINX_EXTENSIONS` は、conf.py に書けば conf.py の import で、X に書けば X の import で解決し、どちらも名前の先が文字列か文字列 list の literal のときだけ読む (定数の先の定数は追わない)。読めない名前は gap にする (§11)。X の名前は module reference にし、行は conf.py の star import の行、origin は conf.py にする (#830 と同じく参照は conf.py に留める。inline ignore はその行に書く)。conf.py の代入が star の値を置き換える順序は追わず、和集合で使用を数える (使用の数え漏れより過剰の方が安全)。辿るのは 1 段だけで、X がさらに star import する module は辿らず gap にする。conf.py は `docs/conf.py` に限らない (#814)。Sphinx の conf.py の場所は `sphinx-build` の引数で決まり、mlflow は `docs/api_reference/source/conf.py`、airflow は `providers-summary-docs/conf.py` や `devel-common/src/docs/rtd-deprecation/conf.py` に置く。`sphinx-build` は root document (既定の `index`) を conf.py と同じ source directory から読むので、`index.rst` か `index.md` と同じディレクトリにある探索済みの `conf.py` を Sphinx の conf.py とし、docs context の plugin entry にして同じく読む (`--explain` の origin は `conf.py beside index.rst`)。探索で runtime context になった conf.py (airflow の member root にある `providers-summary-docs/conf.py`、package の中の conf.py) は docs context に付け替え、それが読む extension を runtime の使用にしない (docs group だけで宣言した `sphinx-jinja` が CHK005 にならない)。runtime と docs の file は同じ import context で parse するので、parse cache はそのまま使える。root document の無い `conf.py` (任意の package の `conf.py` モジュール) は Sphinx の conf.py と見なさない。`.readthedocs.yaml` の `sphinx.configuration` を読む案、Makefile や tox の `sphinx-build` の引数を読む案、慣習的な path の一覧を足す案は採らない。前者 2 つは変数や Python で組まれた path (airflow の `build-docs`) を読めず、どの場合も root document の規則が拾い、一覧は mlflow のような任意の場所を拾えないため。探索外の場所 (layout の glob の外) にある root document の隣の `conf.py` は探さない。enabler は `docs/conf.py` のまま変えない。`html_theme` の literal string も、同じ正規化名の distribution が宣言されていれば (`furo`、`sphinx_rtd_theme`) module reference にする。theme は import ではなく名前で登録され、Sphinx 同梱の theme (`alabaster` など) は distribution を持たないため、宣言の無い名前は参照にしない (#809)。Sphinx は root に加え各 workspace member の `docs/conf.py` も同じく扱い、binary usage の origin はその member の conf.py にする (#612)。member の docs 配下（と test 配下、`examples/*.ipynb`）は member が probe 末尾で確定してから docs（test、dev）context に付け替え (`apply_member_context`)、`--production` では root の `docs/` と同様に解析対象から外す。member の宣言 package 外でも reachability が file に対応付ける import root（`devel-common/src/docs/`）は first-party に解決する。MkDocs は static config scan で `material` theme と既知 plugin (`mkdocstrings`, `autorefs` など) を used distribution として扱う。

Airflow / Dagster / Prefect / Scrapy は `src/plugins/{airflow,dagster,prefect,scrapy}.rs` で実装し、tool が import ではなく path・設定の文字列で読む定義を entry にする (#401)。いずれも YAML / INI / TOML を text として読み、Python は literal だけを読む (解析対象を実行しない)。読めない値は opaque な command target とし、§8 の `UnreadCommandTarget` gap で CHK001 を likely 以下にする。存在しない file を名指す値は project の file を隠さないので entry にも gap にもしない。絶対 path や drive 付きの path は project の外を指しうるので gap にする。inventory に無く disk で見つけた file の context は path が §10 で示すものにする。

- Airflow: root と各 workspace member の `airflow.cfg` の `[core] dags_folder` (`{AIRFLOW_HOME}` / `$AIRFLOW_HOME` は設定ファイルのディレクトリ、相対 path も同じ) と、`dags/` および名前が `dags` / `dags_*` / `*_dags` (`-` 形を含む) のディレクトリ (test suite や example が `DagBag` を向ける folder、airflow 本体の `tests/dags_with_system_exit/` など) の下の `.py` を、DAG processor の safe mode と同じく text に `airflow` と `dag` を両方含むときだけ entry にする。`dag_discovery_safe_mode = False` の `dags_folder` と、名前がちょうど `dags` のディレクトリ (大文字小文字を区別し、どの深さでも、その下の subdirectory も含む) の下は全 `.py` を entry にする (#1064)。test や e2e は `dags/` を folder ごと、または file を名指して DAG processor に渡し、safe mode で読み飛ばされる file (airflow の `tests/unit/dags/no_dags.py`) や `.airflowignore` で外れる file (`subdir2/subdir3/should_ignore_this.py`) も test data として読むため。`__init__.py` や DAG を定義しない helper も entry になり、runtime の `dags/` にある使われない helper は CHK001 の見落とし側に倒れる。部分一致の名前 (`example_dags` の subpackage、`dags_corrupted` など) は safe mode の判定を残す。context はどちらも §10 の path どおり。inventory に無い入れ子の `dags/` は walk しない (root と各 member 直下の `dags/` と `dags_folder` だけを disk で探す。inventory に無い file は CHK001 にならないため)。絶対 path・他の置換・`~` の `dags_folder` は gap。同じ `airflow.cfg` の `[dag_processor] dag_bundle_config_list` (Airflow 3、#1058) は値を JSON として読み (indent した継続行は連結する)、classpath が `airflow.dag_processing.bundles.local.LocalDagBundle` の item の `kwargs.path` を `dags_folder` と同じ規則で DAG folder にする。safe mode も `dags_folder` と同じく `[core] dag_discovery_safe_mode` に従う (DAG processor の設定で bundle 共通のため)。`path` の無い (または `null` の) local bundle は `dags_folder` を読むので追加しない。空・`null`・`[]` は bundle を定めないので何もしない。JSON でない値や list でない値、`classpath` (文字列) と `kwargs` (object) を持たない item、文字列でない `path` は JSON text を gap にし、他の class の bundle (`GitDagBundle` や S3 など、checkout の外から DAG を読む) は classpath を、絶対 path・置換・`~`・root の外の `path` はその path を gap にする。origin は key の行。環境変数 (`AIRFLOW__CORE__DAGS_FOLDER` / `AIRFLOW__DAG_PROCESSOR__DAG_BUNDLE_CONFIG_LIST`) や docker-compose・Helm chart の値は `dags_folder` と同じく読まない (解析する host の環境は project の設定ではないため)。root 自体を指す `dags_folder` は discovery の tree そのものなので disk を walk せず、inventory の file だけを見る (virtualenv や build 出力を読まないため)。`.airflowignore` は読まない (除外される file も entry になり、CHK001 の見落とし側に倒れる)。
- Dagster: `[tool.dagster] module_name` (文字列か文字列 list)、dg project (`[tool.dg.project]` / `dg.toml` の `[project]`) の `defs_module` (既定 `<root_module>.defs`、その下の全 module) と `code_location_target_module` (既定 `<root_module>.definitions`、file があるときだけ)、`workspace.yaml` の `load_from` の `python_file` (設定ファイルのディレクトリからの path) / `python_module` / `python_package` (inline か `relative_path` / `module_name` / `package_name`) を読む。`grpc_server` は別 process なので無視し、それ以外の kind・template・dotted でない値、`load_from:` と同じ行の flow sequence や alias、list の item でない行は gap。project に file の無い module は module reference として import resolver に渡す。加えて `dagster` から `Definitions` / `repository` / `definitions` を import するか `import dagster` の属性として使う inventory 内の module を entry にする。dagster が束縛された definitions を名前空間から集めるので、これらの entry は API surface から外す (§12)。
- Prefect: root と各 member の `prefect.yaml` の `entrypoint:` を読み、`path.py:flow` は設定ファイルのディレクトリからの file と symbol の entry、`:` の無い `module.flow` は symbol reference にする。`null` / 空は名指しが無いので無視し、template・alias・それ以外の形は gap。旧形式の `*-deployment.yaml` は読まない。
- Scrapy: root と各 member の `scrapy.cfg` の `[settings]` の値を settings module (設定ファイルのディレクトリを `sys.path` に置いて import される) として entry にし、その file を literal として読む。`SPIDER_MODULES` (文字列の list / tuple、またはカンマ区切りの文字列) の package の下の全 module を entry にし、それ以外の値や dotted でない要素は gap。`__init__.py` の無い namespace package も `pkgutil` と同じく walk し、project に file の無い要素は module reference にする。settings の `from X import *` (相対 import を含む) は、取り込んだ module も settings として同じように読み、top-level package より上を指す相対 import は gap にする。大文字の top-level 設定 (`ITEM_PIPELINES`、middleware、`*_CLASS` など) の dict key / 値、list、tuple にある文字列のうち、project の module か module の名前を指すものはその module を entry に、名前を symbol reference にする。project を指さない文字列は何も隠さないので無視し、literal でない設定値は読まない (その component は未到達のまま残る)。settings に file の無い値は module reference にする。`NEWSPIDER_MODULE` は `genspider` だけが使うので entry の根拠にはしない。

notebook parsing は v0.2 plugin 拡充の初期実装として、source discovery が `.ipynb` を `FileKind::Notebook` として拾い、parser が `cells[].cell_type == "code"` の `source` だけを連結して既存の Python static parser に渡す。markdown/raw cell と outputs は無視し、notebook JSON が壊れている場合は file を skip する (§6、#893)。読めない code cell は warning を出して読み飛ばす (§6、#898)。

step 11 symbol usage analysis (`src/rules/symbols/`) は、CHK006/CHK007 の参照有無判定に `ReferenceIndex` (`graph.rs`) を使う。旧実装は登録済みシンボルごとに参照一覧を線形走査しており project size に対し二乗コストになっていた (10k/20k合成fixtureで実行命令数の約4割を占有)。`ReferenceIndex::build` が reachable module 群を一度だけ走査して `HashMap<SymbolId, bool>` (値は「他moduleから参照されたか」) を構築し、以降のlookupをO(1)にする。2k/10k/20k合成fixtureでissue出力が修正前後で完全一致することを確認済み。

### ルール解析の内部入力

step 10/11 は借用 `RuleContext` で resolution / reachability / graph / sources /
parse を共有する。設定と strict は依存ルール固有の `DependencyRuleContext` にまとめる。
pipeline もライブラリ利用者も公開 `reconcile_with_context` / `analyze_with_context` に
context を直接渡す（位置引数を並べる互換 wrapper は持たない）。候補の安定 sort（rule code → subject）は
step 10/11/12 で同じ helper を使い、同一キーの入力順を保持する。

### bundled resolver map の再利用

bundled import reverse map と binary map は `LazyLock` でプロセス内に一度だけ構築する。
import map は静的参照を持ち、user map は解析ごとに構築する。binary map は公開返却型
`BTreeMap` を維持するため静的 map を clone し、user → venv の順で上書きする。
候補の sort / dedup、user import override の優先順位は変えない。

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
- Knip configuration hints: <https://knip.dev/reference/configuration-hints>
- PEP 723 Inline script metadata: <https://peps.python.org/pep-0723/>
- PEP 751 pylock.toml: <https://peps.python.org/pep-0751/>
- PEP 810 Explicit lazy imports: <https://peps.python.org/pep-0810/>
- ロードマップのギャップ分析: [`roadmap-gap-analysis.ja.md`](./roadmap-gap-analysis.ja.md)
