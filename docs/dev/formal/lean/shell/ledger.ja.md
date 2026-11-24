# POSIX sh オラクル (O1) の台帳

`src/plugins/commands.rs` と task file の前処理 (Makefile・tox・Dockerfile) が「どのコマンドを実行するか」を
読む部分を、Rust を移植せずに規格から書いた Lean 4 の仕様と突き合わせた結果。

## 仕様と証明

| file | 中身 | 出典 |
|---|---|---|
| `ShellSpec/Syntax.lean` | token 認識、引用、展開 (`$( )`・バッククォート・`${ }`・`$(( ))`・`$'..'`)、here-document、文法 (list・pipeline・`if` / `while` / `for` / `case` / `{ }` / `( )` / 関数定義・`[[ ]]`・`(( ))`)。コマンド置換・here-document の本文の中も含め、実行されうる simple command をすべて返す | POSIX XCU 2.2〜2.10、bash の拡張 |
| `ShellSpec/Exec.lean` | simple command が実行するコマンド。builtin (`command` / `exec` / `eval` / `trap`)、wrapper (`env` / `nice` / `timeout` / `stdbuf` / `setsid` / `ionice` / `taskset` / `flock` / `xargs` / `find -exec` / `watch` / `sudo` / `doas`)、shell の `-c`、CPython のコマンドライン、`uv run` / `poetry run` / `pdm run` / `hatch run` / `conda run` / `coverage run` | 各 utility の man page と `python -h` |
| `ShellSpec/Proofs.lean` | 全入力・全 fuel で成り立つ定理: `-c` で option 解析が終わる、`-m MOD` は MOD を実行する、値を取らない flag は透過、`-um MOD` は MOD を実行する、`-W` / `--check-hash-based-pycs` は値を消費する、`--` で getopt が終わる。加えて構文層の回帰例 5 件 (`native_decide`) | |

`skipOpts` の「operand で止まる」「値を取る option は次の語を消費する」は Lean 4.15 が整礎再帰の展開定理を
作れず、証明を外した (挙動は差分テストで確かめている)。

## 手順

```bash
# Lean 4.15.0 を GitHub release の zip から入れる (elan のインストーラは使わない)
lake build                           # 仕様・証明・oracle をビルド。証明が崩れるとここで失敗する
python3 harness.py --chokkin ../../../../../target/release/chokkin --generate 6000 --seed 7
```

`harness.py` は文法から script を生成し、`pkg/tool.py` と `pkg/gen.py` を持つ app project の root の `ci.sh` に
置いて chokkin を走らせる。期待値は生成側の意図ではなく oracle (`.lake/build/bin/oracle`) の答えで、
「追跡する tool (`black` 等) を実行する ⇔ CHK008」「`python -m pkg.tool` を実行する ⇔ `pkg/tool.py` に
CHK001 が無い」「`python pkg/gen.py` を実行する ⇔ `pkg/gen.py` に CHK001 が無い」を比べる。
6000 件のうち 2899 件が食い違い、下の 23 の根本原因に分かれた。Makefile・tox・Dockerfile の前処理層は
個別の入力で確かめた (`ci.sh` 以外の file は harness の対象外)。

## 根本原因 (v0.7.14、`7166805`)

「別の修正が要るもの = 1 件」で数える。FP は誤って finding を出すもの (多くは実行される module の
CHK001 certain)、FN は CHK008 を出し損ねるもの。

| ID | issue | 層 | 内容 | 向き |
|---|---|---|---|---|
| R1 | #1381 | sh | コマンド置換 `$( )` / バッククォートの中を読まない | FP (CHK001) |
| R2 | #1382 | sh | `function name { }` の本体を読まない | FP |
| R3 | #1383 | sh | 先頭リダイレクト `>log cmd` のコマンドを読まない | FP |
| R4 | #1384 | sh | 複数行の引用文字列の 2 行目以降をコマンドとして読む | FP (CHK008) |
| R5 | #1385 | sh | `command -v X` を X の実行とみなす | FP (CHK008) |
| R6 | #1386 | sh | wrapper の option の値 (`sudo -u app`) をコマンド名と取り違える | FP |
| R7 | #1387 | sh | `nice` / `stdbuf` / `setsid` / `ionice` / `doas` / `xargs` を知らない | FP |
| R8 | #1388 | sh | `timeout DURATION` / `taskset MASK` / `flock FILE` を知らない | FP |
| R9 | #1389 | sh | `find -exec` のコマンドを読まない | FP |
| R10 | #1390 | sh | `eval` / `trap` / `watch` の文字列を読まない | FP |
| R11 | #1392 | sh | `/usr/bin/env` の後ろを読まない | FP |
| R12 | #1393 | sh | `hatch run ENV:CMD` | FP |
| R13 | #1394 | sh | `conda run -n ENV CMD` | FP |
| R14 | #1396 | python | `python -um MOD` が entry にならない | FP (likely) |
| R15 | #1397 | python | `--check-hash-based-pycs MODE` の値を飛ばさない | FP (likely) |
| R16 | #1399 | python | `pypy3` を Python とみなさない | FP |
| R17 | #1400 | sh | `uv run` の後の wrapper の連鎖を読まない | FP |
| R18 | #1402 | make | `target: ; recipe` を読まない | FP |
| R19 | #1403 | tox | `-cmd` の `-` を外さない | FP |
| R20 | #1405 | tox | `{[section]key}` を辿らない | FP |
| R21 | #1407 | docker | `# escape=` を読まない | FP |
| R22 | #1408 | make | `.RECIPEPREFIX` を読まない | FP |
| R23 | #1409 | tox | `{posargs:DEFAULT}` を読まない | FP |

## 数えなかったもの

- `cat <<EOF | sh` の here-document を shell に渡す形。仕様も本文をデータとして扱うので比べていない。
- `x='a 'b' c'` のように引用と非引用を 1 語につなげた語の扱いの違い。生成器の副産物で、実際の script では見ない。
- `.github/workflows/*.yml` の `run:`。spec §11 が usage に数えない (install の provider と script の path だけ) と定めている。
- `$(BLACK)` のような make 変数の展開。`$(PYTHON) -m` は読むので、tool 名を変数に置いた形の FN だけが残る。
- 生成した 6000 件のうち 4 件は仕様が parse できず (引用を何重にも入れ子にした生成器の副産物)、比べていない。
