# O8 (Aeneas) の台帳

対象は v0.7.14 (`7166805`)。1 件 = 別の修正が要る根本原因 1 つ。起票前に既存 issue を検索した。

## 起票したもの

| ID | issue | 対象 | 仕様 | 反例 | 利用者に見える影響 |
|---|---|---|---|---|---|
| A1 | #1443 | `config/types.rs` `normalize_entry_path` | POSIX XBD 4.16 (連続 `/` は 1 つ、`.` は同じ directory) | `app//run.py`、`app/./run.py` が正規化されない | entry が skipped になり、到達できる file が CHK001 likely、使う dependency が CHK002 likely (FP)。#1168 は先頭の `./` と `\` だけを直した |
| A2 | #1444 | `pipeline/trace.rs` `normalize_trace_path` | 同上 | `.//src/app.py` → `/src/app.py` | `--trace` が root の外と誤報する。`a//b` や `a/./b` は #1157 の `Path` 経由の再検索で救われるので、見えるのは `.//` 始まりだけ |

A1 / A2 は #1451 で 1 つの `path_util::normalize_user_path` にまとめて直した。`extract/` の copy と `dump` もその関数に替え、`lake exe check` の照合対象は `normalize_user_path` になる。

## 仕様と食い違うが起票していないもの

| ID | 対象 | 反例 (最短) | 起票しない理由 |
|---|---|---|---|
| N1 | `resolver/pytest_path.rs` `normalize_dir` | `a/.` → `a/.` (仕様 `a`) | `pythonpath = ["src/."]`、`["src//"]`、`["./src/./"]` を実バイナリで試し、結果は `["src"]` と同じだった (src layout の推定が先に解決する)。見える影響を作れていない |
| N2 | `manifest/wheel.rs` `normalize_path` | `a/.` → `a/.` | `package-dir` などに `src/.` と書く実例が考えにくく、影響の再現もしていない |

## 仕様どおりだったもの (契約として固定)

- **`VersionTest::holds`**: Aeneas の翻訳が、`read` が作る全 test (6 演算子 × `full` / `[:2]` /
  major のみ × minor あり / なし) と全 minor・全 micro で Python のタプル比較 (Language Reference
  §6.10.1) と一致することを証明した (`Proofs/Holds.lean` `holds_eq_spec`)。既存の
  `version_guard_model.py` C1 は移植の照合だったので、移植と実装の乖離もこれで閉じる。
- **区間代数**: `from_intervals` / `complement` / `union` / `intersect` / `is_subset_of` の翻訳を、
  bound が各 segment `{0, 1, 2}` と `TOP` の格子で全数照合し (`Check/Intervals.lean`)、集合演算と
  一致、`from_intervals` の出力は常に正規形 (昇順・空でない・隣と隙間あり) だった (143573 件)。
  生成した `Funs.lean` の merge 条件を `<=` から `<` に変えると落ちることも確かめた。
- **`marker_always_holds`**: `requires-python` 15 種 × 生成した marker 6 万件で `packaging` 26.3
  (Python 3.13) の評価と比べた (`packaging_diff/`)。chokkin が「常に成り立つ」とし `packaging` が
  偽とする例 (健全性違反) は 0 件。逆向き (chokkin は「常に」とせず、格子 2.0.0〜4.15.3 の
  `packaging` は真) の 77 件は、格子を 0.0.0〜5.15.7 に広げると全件 `packaging` も偽になり、
  chokkin が正しかった (1.x や patch 4 以上の版が marker を外す)。読まない marker (`NOMARK`) は `===`、左辺
  literal の `==`、`packaging` が文字列比較に落ちる値などで、ADR 0005 どおり条件付きのまま残る。
- **PEP 503**: `normalize_distribution_name` と `uv_lock::normalized` (早道つき) は ASCII の全
  19531 文字列で `re.sub(r"[-_.]+", "-", name).lower()` と一致した。
- **`target_part`**: 全 19608 文字列で出力に `:` が無く、RFC 3986 の percent-decoding で元に戻る
  (よって単射)。
- **`normalize_relative_path`** (workspace) は path の仕様と全数で一致した。他の path 正規化を直す
  ときの見本になる。

## 件数

起票 2 件 (#1443、#1444)。プランの見積もり 1〜3 件の範囲。
