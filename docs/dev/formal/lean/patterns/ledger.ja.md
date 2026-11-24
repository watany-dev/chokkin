# Lean 4 pattern 方言オラクル (O4) 台帳

設定ファイルの pattern を chokkin が「その設定を読むツールの方言」で読んでいるかを、
Lean 4 で書いた方言ごとの仕様と実バイナリ (v0.7.14、`7166805`) の差分で調べた記録。
1 件 = 別の修正が要る根本原因 1 つ。

## 仕様 (`Patterns/`)

外部の規格・文書から書き、Rust の実装は見ていない。

| module | 方言 | 使う設定 | 根拠 |
| --- | --- | --- | --- |
| `Fnmatch` | Python `fnmatch.fnmatchcase` | setuptools `packages.find`、pytest `python_files` | `fnmatch` の文書と `translate` |
| `Gitignore` | gitignore(5) + git wildmatch | `.gitignore`、hatch `include` / `exclude` | gitignore(5) PATTERN FORMAT |
| `PyGlob` | Python `glob.glob(recursive=True)` | pytest `testpaths` | `glob` の文書 |
| `Tox` | tox 4 の factor 条件 | `tox.ini` の factor 付きの行 | tox の文書 (数値の範囲 `{38-40}` は対象外) |
| `Setuptools` | `find_packages` / `find_namespace_packages` | `[tool.setuptools.packages.find]` | setuptools の Package Discovery |
| `Pytest` | `fnmatch_ex` | `python_files` | pytest reference |

証明した性質 (全入力):

- `Fnmatch.star_all`: `*` は `/` と `.` を含むすべての文字列に一致する。
- `Fnmatch.lit_prefix_star`: `pre*` は `pre` に続く任意の文字列に一致する (`*` は `/` を跨ぐ)。
- `Fnmatch.plain_is_literal`: `*` `?` `[` を含まない pattern は同じ文字列だけに一致する (`{a,b}` も `\` も意味を持たない)。
- `Gitignore.star_no_slash`: wildmatch の `*` は `/` に一致しない。
- `Gitignore.dstarSlash_empty`: `**/` は 0 段の directory にも一致する。
- `Gitignore.parent_excluded`: 親 directory が除外されれば、`!` があっても file は除外される。

個別の例は `native_decide` で確かめている (`lake build` で検査される)。

## 仕様の裏取り (`harness/validate_spec.py`)

仕様を chokkin の判定に使う前に、各ツールの実装と乱数入力で突き合わせた:
CPython 3.13 の `fnmatch` / `glob`、git 2.43 の `ls-files --others --exclude-standard`、
tox 4.67 の `expand_factors`、setuptools 84 の `PackageFinder` / `PEP420PackageFinder`、
pytest 9.2 の `fnmatch_ex`。仕様側の誤りが 2 つ見つかり、どちらも実装に合わせて直した。

- pytest: `/` で始まる pattern には `*/` を前置しない。
- git: `/` を含む pattern は最初の glob 文字の前までを文字として比べ、残りを pattern の
  先頭として照合する (`dir.c` の `match_pathname`)。gitignore(5) の「その他の連続する
  `*` は通常の `*`」に反して `a**/b` が `ab` や `ax/y/b` に一致する。chokkin も git と
  違う動きをしており P11 として起票した。

VALIDATION_RESULT

## 差分 (`harness/diff_chokkin.py`)

| family | 観測 | 件数 | 不一致 |
| --- | --- | --- | --- |
| gitignore | app mode の CHK001 (走査された file) | 2,760 | 24 |
| pyfiles | `python_files` で test になる file (CHK001 の有無) | 340 | 45 |
| testpaths | `testpaths` + `python_files` で test になる file | 400 | 18 |
| find | library mode の CHK001 (wheel に入らない file) | 190 | 22 |
| tox | factor 付き `deps` 行の requirement (CHK008) | 340 | 116 |

不一致は `minimize_gitignore.py` などで最小化し、根本原因ごとにまとめた。手で足した確認
(hatch の `include` / `exclude`) は `pathspec.GitIgnoreSpec` で期待値を取った。

## 見つけた不具合

| # | issue | 方向 | 内容 |
| --- | --- | --- | --- |
| P1 | #1419 | 誤検知 + 見逃し | pytest `python_files` を globset で読む (`*` が directory 名に届く、`{a,b}`、`[^x]`) |
| P2 | #1422 | 誤検知 (CHK008) | tox の factor 条件の文法 (`py{38,39}:`、`py38, py39:`、`:` の後の空白) |
| P3 | #1424 | 誤検知 (CHK001) | setuptools `packages.find` の `include` / `exclude` を globset で読む (`[^c]`、`{a,b}`) |
| P4 | #1426 | 見逃し | setuptools `packages.find` が飛ばす `.` 入りの directory を package とみなす |
| P5 | #1428 | 誤検知 (CHK001) | pytest `testpaths` を Python glob でなく globset で読む (`t{1,2}`、`[^a]`) |
| P6 | #1430 | 誤検知 + 見逃し | file を指す `testpaths` を directory とみなす |
| P7 | #1432 | 誤検知 (CHK001) | `.gitignore` の `!` だけの行がすべての除外を打ち消す |
| P8 | #1433 | 見逃し寄り | `.gitignore` 先頭の `***/` を `**/` として読まない |
| P9 | #1434 | 誤検知 (CHK001) | hatch `include` を gitignore 形式でなく path / globset として読む |
| P10 | #1435 | 見逃し | hatch `exclude` を読まない |
| P11 | #1437 | 誤検知 (CHK001) | `.gitignore` の `a**/b.py` (文字の直後の `**/`) を git のように読まない |
| P12 | #1447 | 見逃し | `.gitignore` の否定クラス `[!x]` / `[^x]` が `/` に一致する |

計 12 件。

## 当たらなかったもの・対象外

- gitignore: brace (#1149 で修正済み)、`\ ` の末尾空白、`\#` `\!`、1 文字の名前への `[!a]` / `[^a]`、`**/`、`/**`、
  入れ子の `.gitignore` の優先順位は git と一致した。
- setuptools: `include = ["acme"]` が `acme.sub` を含まないこと、`namespaces = false` の
  `__init__.py` の連鎖は一致した。
- pytest `testpaths` が何にも一致しないときに rootdir 全体へ戻ることは一致した。
- tox の数値の範囲 (`{38-40}`) と、`norecursedirs` を読まないこと (方言ではなく未対応) は数えていない。
- Django split-settings の `include()` も globset で component を読む (`{a,b}`、`[^x]`) が、
  そういう pattern は split-settings 側で import が失敗するため実害が小さく、起票していない。

## 再現

```sh
# Lean 4.15 (lean-toolchain) と lake
lake build                                   # 仕様・証明・例の検査と oracle のビルド
VENV/bin/python -I harness/validate_spec.py 400   # tox / setuptools / pytest を入れた venv
cargo build --release                        # repo root で
python3 harness/diff_chokkin.py gitignore 1500 7  # family と件数と seed
```
