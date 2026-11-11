# v0.7.10 の形式検証台帳

v0.7.10 の変更を `.claude/skills/formal-spec-extraction` の手順で形式化した結果。「実装の主張」はコードのコメント・テスト・CHANGELOG が述べる仕様、「検査」はモデルの結果、「確認事項」は反例のうち意図か不具合かを決める必要があるもの。確認事項はモデルに `Q` 番号で登録してあり、決着したら意図なら `D` に移し、不具合なら実装を直してモデルから外す (外さないと stale として CI が落ちる)。

## 実装の主張と検査結果

| 変更 | 実装の主張 | 検査 | 結果 |
|---|---|---|---|
| #997 `sys.version_info` ガード | 読むのは `sys.version_info` / `[:2]` とリテラルタプルの比較 (と `[0]` / `.major`) だけで、他は opaque。`requires-python` の全 minor で除外される分岐の import は CHK003/004/005 にならない | `never_runs_model.py` V1–V5、`stdlib.rs` `range_follows_the_minors_requires_python_allows` に `excludes_minors` の健全性 (PEP 440 で評価した許可 minor と交わらない) を追加 | 成立。壊した移植 3 種 (`full` の長さを無視、opaque を free 扱い、`max` を含む比較) はすべて反例になる |
| #989 setup.cfg の 1 行の値を `;` で分割 | setuptools の `_parse_list_semicolon` と同じく、1 行の値は `;` で、複数行の値は行で分ける | `setup_cfg_requirements_model.py` P1/P2 (25,732 通り) | 分割規則は成立 (P2)。分割の前段の INI 読みで Q1・Q2 の乖離 (#1311・#1308 で修正)。#989 以前の読み (常に行で分割) は反例になる |
| #998 testpaths の glob 展開 | pytest の `glob.iglob(entry, recursive=True)` と同じ path を指す。何も指さない entry は warning | `testpaths_glob_model.py` T1/T2 (4,368 entry) | `*` `?` `[...]` と隠し名の規則は成立。`**` と globset の構文差で D3・D4・D6 の乖離 (Q5 は #1309 で修正) |

## 乖離 (確認事項の決着)

| # | 反例 | chokkin | setuptools / pytest | 影響 |
|---|---|---|---|---|
| D3 | globset が拒む component (閉じない `[`、逆順の範囲 `[z-a]`) | その component は何にも一致せず、entry は字義どおり残る (D2) | `t[1` は字義どおりの名前に一致し、`[z-a]` は何にも一致しない | `t[1/*` のような entry が「何も指さない」warning になり test context が付かない |
| D4 | `[^a]` | 否定の class | `^` と `a` の class | 一致する名前が逆になる |
| D6 | 最後の component の `**` | directory だけを返す | file も返す | root 直下でだけ差が出る (`testpaths = **` で root の file が test にならない) |

Q1 (`[options]` 直後のインデントした key を読み飛ばす) は #1311 で、Q2 (`]` の後に文字がある section header を読まない) は #1308 で、Q5 (file に一致した component の後の `**` が file を残す) は #1309 で直した。D3・D4・D6 は `testpaths` にまず書かれない形で、Python 流に読み替える層を足すほどの価値が無いため、意図した差として受け入れた (2026-10-28 決定)。
