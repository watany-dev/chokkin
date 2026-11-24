# O3 字句・行構造オラクル 台帳 (v0.7.14 `7166805`)

Python の字句構造 (物理行・comment と文字列の境界・PEP 263・PEP 723・IPython の行) を外部の規格から Lean 4 で書き、性質を証明してから、`chokkin --reporter json` を black box として突き合わせた結果。1 件 = 別の修正が要る根本原因 1 つ。

## 見つけたもの (issue 化済み 5 件)

| ID | issue | 方向 | 内容 | どの比較で出たか |
|----|-------|------|------|------------------|
| L1 | #1438 | 見逃し | 文字列 literal (f-string・bytes・raw・docstring) の中の `# chokkin: ignore[…]` が抑制する | `gen_ignores.py --seed 1`: 970 件中 149 件 |
| L2 | #1439 | 見逃し / warning 欠落 | `IGNORE_RE` の `\s` が改行と U+3000・NBSP に一致する。次の行にまたがる directive が抑制し、NBSP の崩れた形は warning も出ない | `gen_ignores.py --seed 1`: 改行またぎの抑制 100 件、U+3000 / NBSP の抑制 103 件、warning の欠落 113 件 |
| L3 | #1440 | 誤検出 | CR だけで改行する file の PEP 723 block を見つけず、script の依存が CHK003 | `gen_pep723.py --seed 3`: 1,313 件中 49 件、全件 CR |
| L4 | #1441 | 読み違い | UTF-8 として読める byte 列では PEP 263 の宣言を無視し、宣言した codec と違う名前で import を読む (対応外 codec や未知の名前の宣言でも読む) | `gen_encoding.py`: 7,824 件中 190 件 + 対応外 codec 242 件 + CPython が拒否する宣言 |
| L5 | #1442 | warning 欠落 | 文字列の中の `%load_ext` を拡張の読み込みとみなし、未知の magic の cell を読めた扱いにする | 手動 (行構造の観点で `ipython.rs` を読んで) |

## 見たが issue にしなかったもの

- **先頭の `\` だけの行**: `\` + 改行のあとに `# chokkin: file-ignore[…]` を書くと、CPython ではその comment は先頭 block にあるが、chokkin は `\` の行を最初の文とみなす (`first_statement_offset`)。実在する書き方ではないので起票しない。
- **複数行の import 文の行番号**: `import os, \` の次の行の `yaml` も、CHK003 は文の先頭行に立つ (CPython の `alias.lineno` は 2 行目)。CHK003 は module ごとの finding で、文の行に立てるのは設計どおりと読んだ。
- **docstring・文字列の中の `# chokkin:` の warning**: spec §18 が明記している。
- **CPython が拒否する file を読む**: BOM と `utf8` 宣言 (CPython は `encoding problem: utf8 with BOM`)、gb18030 の 2000 / 2005 の差 (CPython 側は私用領域の文字で識別子にならない)、cp932 の `0xA0` 等 (WHATWG に無い)、big5 の ETEN 行。どれも誤検出につながらないか、spec §6 に書かれた gap。
- **対応外の codec (koi8-r、cp1251 など) の skip**: spec §6 の gap どおり warning と Likely への降格がある。

## 規格と照合した範囲 (問題が出なかったもの)

- 物理行: OSS 2,454 file の末尾に足した import の行は全件一致。`CHK003` / `CHK010` の import 行 (CPython AST と比較) と `CHK006` / `CHK007` の symbol 行 3,637 件も全件一致。
- comment と文字列の境界 (仕様の検証): Lean の COMMENT token は cpython `Lib`・django・sympy の 2,496 file すべてで CPython 3.13 の `tokenize` と一致。生成入力でも不一致 0 (PEP 701 の入れ子 f-string・replacement field 内の comment・`\N{…}` を含む)。
- PEP 263 の宣言位置 (1 行目・2 行目・3 行目・emacs / vim 形式・BOM) と CPython の codec 別名表 (`encodings.aliases` 全件と表記揺れ): 対応 codec の別名の取りこぼしは無い。
- PEP 723 の block の形 (末尾空白・`#///`・内容の途中の `# ///`・複数 block): CR 以外の差は無い。
- notebook の cell をまたぐ行番号: 末尾改行の有無・magic のある cell で、finding と warning の行がそろう。

## 再現

```
cargo build --release          # repo root で。target/release/chokkin
lake build                     # .lake/build/bin/oracle
python3 -I harness/gen_ignores.py --cases 1000 --seed 1
python3 -I harness/gen_ignores.py --cases 800 --seed 2 --exclude string,whitespace
python3 -I harness/gen_encoding.py
python3 -I harness/gen_pep723.py --cases 1500 --seed 3
python3 -I harness/oss.py <cpython>/Lib <django> <sympy> --limit 2500
python3 -I harness/oss_symbols.py <django>/django <sympy>/sympy <cpython>/Lib --limit 500
```

`--exclude string,whitespace` は L1・L2 の引き金を生成から外し、残りに別の原因が無いかを見るためのもの (出たのは spec §18 が明記する文字列の中の warning と、先頭の `\` 行だけ)。
