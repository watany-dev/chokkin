# Dogfooding: aws-cli で v0.4.0 と v0.5.0 を比較 (2026-09-28)

## 条件

- 対象: `aws/aws-cli` `develop` @ `4a54791` (2026-09-18)。Python 949 file (runtime 310 / test 638 / dev 1)
- chokkin: PyPI の `chokkin==0.4.0` / `chokkin==0.5.0` wheel (`uv tool run --from`)
- 実行: リポジトリ root で `chokkin --reporter json --no-cache`。設定なし (`Config: defaults`、`Mode: auto` → app)
- aws-cli は `setup.py` が非 static のため `install_requires` を読めない (両版とも `manifest: skipped non-static setup.py`)。
  `[project]` もないので、runtime 依存は未宣言扱いになる

## サマリ

| rule | v0.4.0 | v0.5.0 | 差分 |
|---|---:|---:|---|
| CHK001 | 161 | 136 | 25 file が到達可能に。残り 134 は `certain` → `likely` |
| CHK003 | 106 | 117 | CHK010 だった `dateutil` が CHK003 (`python-dateutil`) に。新たに到達可能になった file の import 分も増加 |
| CHK005 | 2 | 2 | 変化なし (`colorama` / `docutils` が dev のみ宣言) |
| CHK006 | 2579 | 985 | tests/ の symbol 1364+555+128 件が対象外に。一方で 471 件増 (後述の回帰を含む) |
| CHK007 | 3 | 3 | 変化なし |
| CHK008 | 7 | 7 | 変化なし |
| CHK010 | 318 | 6 | `from tests...` 234 件、`dateutil` 25、`awscrt` 6、stdlib 誤判定 3 件が解消 |
| **合計** | **3176** | **1256** | **−60%** |

実行時間 (5 回、同一マシン):

| | cold (`--no-cache`) | warm |
|---|---:|---:|
| v0.4.0 | 990–1193 ms | 273–383 ms |
| v0.5.0 | 252–296 ms | 229–317 ms |

cold が約 4 倍速い。parser を `ruff_python_parser` に切り替えた効果と見られる。

## 改善 (意図どおり)

1. **`from pkg import submodule` の到達性 (#397)**: `from awscli import text` や
   `from awscli.customizations import globalargs` が submodule への edge になった。
   v0.4 で CHK001 だった 25 file (`awscli/text.py`、`awscli/shorthand.py`、`awscli/errorhandler.py`、
   `customizations/emr/*` など) は偽陽性で、すべて解消した。
2. **root `tests/` パッケージを first-party として解決 (#359/#371)**: `from tests import ...` の CHK010 234 件が解消。
   tests/ 自身の symbol (テストクラス等) が CHK006 対象から外れ、ノイズ約 2000 件が消えた。
3. **package map 拡充 (#362)**: `dateutil` → `python-dateutil`、`awscrt` を解決。
   `msvcrt` / `ntpath` / `unicodedata` の stdlib 誤判定も解消。
4. **CHK001 confidence (#266)**: 到達可能なコードに opaque な dynamic import があるため
   `likely` に下がった。aws-cli は plugin を動的 import するので妥当。

## 回帰: test からだけ参照される symbol が CHK006 になる (v0.5.0)

v0.5.0 で増えた CHK006 471 件のうち 193 件は、新たに到達可能になった file に属する (これは想定内)。
残り 278 件の大半は **tests/ からは参照されているが、runtime コードからは参照されていない symbol** である。
増えた 471 件のうち 328 件は、symbol 名が tests/ 内に出現する。

例: `awscli.help:TopicListerCommand` (`tests/unit/test_help.py` で import)、
`awscli.paramfile:ResourceLoadingError`、`awscli.customizations.ecs.deploy:MAX_WAIT_MIN`。

最小再現:

```
app/sub/exceptions.py   class UsedOnlyInTests(Exception): ...
tests/test_x.py         from app.sub.exceptions import UsedOnlyInTests
```

v0.4.0 は報告しないが、v0.5.0 は `CHK006 app.sub.exceptions:UsedOnlyInTests` を報告する。
`tests/__init__.py` の有無に関係なく再現する。

原因: `src/rules/symbols/analyze.rs` の `analyze_with_context` が、
`sources.layout.in_local_package` の module を `reachable_modules` から除外している。
`ReferenceIndex::build` も同じ `reachable_modules` を入力にするため、tests/ は
「CHK006 の報告対象」だけでなく「参照元」からも外れてしまう。
`__init__.py` がない tests/ でも再現するため、`file_module_name` → `path_to_module` の置き換えで
module 名が付かなくなった経路もありそう (未確認)。

修正方針: 報告対象 (registry) から local package を除外するのはそのままにする。
`ReferenceIndex` には到達可能な全 module (tests を含む) を渡す。

## 既存の偽陽性: `from pkg import module` 経由の属性参照 (両版共通)

```python
# awscli/customizations/emr/config.py
from awscli.customizations.emr import exceptions
raise exceptions.InvalidBooleanConfigError(...)
```

`exceptions.InvalidBooleanConfigError` の参照を追跡できず、CHK006 になる (両版とも)。
#74 で対応したのは `import module; module.name` 形式だけで、`from pkg import module; module.name` 形式は未対応。
aws-cli ではこの import 形式が多用されている。

## 残る主なノイズ (aws-cli 固有・設定で対処可能)

- CHK003 117 件の大半 (`botocore` 86、`s3transfer` 8 など) は、`setup.py` の `install_requires` を読めないことに起因する。
  static な `[project]` がないプロジェクトの制約であり、chokkin 側の不具合ではない。
- CHK001 136 件の大半は vendored の `awscli/botocore/*`。aws-cli 本体は top-level の `botocore` を import しており、
  vendored コピーは import されていない。`[tool.chokkin] ignore` で除外する対象。
