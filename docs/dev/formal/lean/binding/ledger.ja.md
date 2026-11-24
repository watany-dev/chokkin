# 台帳: Python 名前束縛オラクル (Lean 4、v0.7.14)

対象は CHK006 / CHK007 (`src/rules/symbols/*`、`src/parser/exports.rs`) と、名前の解決で決まる CHK001 の一部。
仕様は Python Language Reference §4.2 / §5 / §7.11 から書き (`Binding/Spec.lean`)、chokkin の実装は読まずに
release バイナリ (v0.7.14、`7166805`) を `--reporter json --no-cache` で外から叩いて突き合わせた。

## 実行した入力

| 入力 | 件数 | 不一致 |
|---|---|---|
| 生成: module の取得 15 通り × 値の運び方 35 通り × 読み方 12 通り | 6,300 | FP 6,072 |
| 生成: star / 再export / 相対 import / module をまたぐ流れ | 25 | FP 11 |
| 生成: import と読み取りの置き場所 49 通り × import の形 5 通り | 241 | 0 |
| 生成: package と `__init__.py` の再export (CHK007) | 24 | FP 7 |
| 生成: 負例 (Language Reference の意味では読んでいない) | 19 | FN 14 |
| OSS: click、flask、requests、httpx、rich、typer の実コード | 6 repo | 5 (typer) |

FP の件数は組合せの数で、根本原因の数ではない。下の 13 件が「別の修正が要る」単位。置き場所 (関数内 import、
class 本体、`try` / `if` / `with`、内包表記、PEP 695 など) の 241 件は全部一致した。

## 見つけた根本原因

| ID | issue | 方向 | 内容 |
|---|---|---|---|
| B1 | #1413 | FP | `from m import *` で束縛した名前を importer が読んでも参照にならない (CHK006、`__all__` があると certain。CHK007 も) |
| B2 | #1414 | FP | `import pkg` の後の `pkg.m.name` を、そのファイルが `pkg.m` を import していないと参照にしない (typer の `typer.colors.GREEN`、`_click.formatting.join_options`) |
| B3 | #1415 | FP | `f = pkg.m` などの名前への代入による module の別名を追わない (動的 import の別名だけ追う) |
| B4 | #1416 | FP | `f = importlib.import_module(...)` の別名を同じ scope でしか見ず、関数・class 本体の `f.name` が CHK006 |
| B5 | #1417 | FP | 他 module が import で束縛した module の別名 (`from pkg.user import mod`、`pkg.mod`) を辿らない |
| B6 | #1418 | FP | `__import__("pkg.m", fromlist=...)` / `__import__("pkg.m").m` の戻り値の属性を参照にしない |
| B7 | #1420 | FP | `sys.modules["pkg.m"].name` を参照にしない |
| B8 | #1421 | FP | package の `__all__` に submodule 名があるときの `from pkg import *` が束縛する submodule を辿らない |
| B9 | #1423 | FP (CHK001) | `__init__.py` の `import_module(".m", __name__)` を解決しない (`__package__` なら解決する) |
| B10 | #1425 | FP (CHK001 / CHK006) | 文字列定数を名前経由で `import_module` / `getattr` に渡す形を解決しない |
| B11 | #1427 | FP | 関数の戻り値・引数、container、class / instance 属性、`for` / `match` を通った module object を辿らない |
| B12 | #1429 | FN | 局所変数・引数・内包表記などが import 名を隠しても `pkg.m.name` を参照に数える |
| B13 | #1431 | FN | `pkg.m.name = ...` / `del pkg.m.name` を読み取りに数える |

分け方: B3 (名前への代入) と B11 (値の流れ) は直す場所が違うので分けた。B2 (属性名が submodule 名そのもの) と
B5 (別 module が付けた別名を辿る) も分けた。B12 の各形 (引数、内包表記、`except as` など) は scope の解決 1 つで
直るので 1 件にした。

## 既存 issue との関係

- #576 は package の `__init__.py` が star import する側。B1 は star import した module 自身が名前を使う側。
- #1144 は `import_module("pkg.m").name` (直接)。B3 / B4 / B6 はその隣。
- #1143 は `getattr(module, "literal")`。B10 は literal が名前に入っている形。
- #1274 は `sys.modules[__name__]` の gap。B7 は literal の key。
- v0.7.12 台帳の D5 (`version_names` が `global` と star import を見ない) は既知なので数えていない。

## 数えなかったもの

- package 内の module の `import m` を兄弟の `pkg/m.py` と読む (#977 の script fallback)。Python 3 では
  絶対 import だが、file が script として直接実行される場合と区別できないので設計どおりとみなした。
- 文字列注釈 `"pkg.m.Target"` と `typing.cast("pkg.m.T", x)`。仕様が文字列を式として読まないので、
  仕様側が参照を主張しない (chokkin は参照に数える、#545 / #1144)。

## 仕様側の既知の近似

- フロー非依存の may 解析。module の top-level で同じ名前を後から別の値に束縛し直す形は両方の値を持つ。
  生成器はこの形を作らない。負例で関数の外に置くと誤って参照を主張した 2 件 (`except as` と `case` の捕捉) は、
  関数の中に移して直した。
- 辞書、instance を返す任意の呼び出し、文字列の連結・f-string は値を持たない (参照を主張しない側に倒れる)。
- `__all__` は literal の list / tuple と `+=` / `extend` / `append` だけ読み、それ以外は全名を渡すとみなす。
