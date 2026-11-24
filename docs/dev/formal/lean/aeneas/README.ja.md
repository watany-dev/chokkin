# O8: Aeneas で実装そのものを Lean 4 に落とす

`docs/dev/formal/README.md` が認める「移植と実装の乖離は検出できない」を閉じるための oracle。
既存のモデル (`version_guard_model.py` など) は Rust の判定を人が Python に移植したもので、
移植が実装と食い違っても気づけない。ここでは実装を **Charon + Aeneas で機械的に Lean に翻訳** し、
翻訳を外部の規格から書いた仕様と照合する。

## 構成

| path | 中身 |
|---|---|
| `extract/` | chokkin の純粋関数の逐語コピー (Cargo crate、chokkin の build には入らない) |
| `check_sync.py` | コピーが `src/` と token 単位で一致することを確かめる (22 関数) |
| `Chokkin/Types.lean`, `Chokkin/Funs.lean` | Aeneas が `extract/` から生成した Lean (手で編集しない) |
| `Chokkin/FunsExternal.lean`, `Chokkin/TypesExternal.lean` | Aeneas に model の無い std 関数の Lean model (Rust の doc から書いた) |
| `Spec/` | 外部の規格から書いた仕様と、仕様自体の性質の証明 |
| `Proofs/` | 翻訳と仕様の関係の定理 |
| `Check/` | 有限領域での全数照合 (`lake exe check`) |
| `ledger.ja.md` | 見つけた不一致と issue の台帳 |

`lean-toolchain` は他の oracle の v4.15.0 ではなく **v4.36.0**。Aeneas の Lean library
(`backends/lean`) がこの版と対応する Mathlib を固定しているため。

## 対象と扱い

| 関数 | 元 | 方法 | 仕様の出典 |
|---|---|---|---|
| `VersionTest::holds` | `parser/version_guard.rs` | Aeneas 翻訳 + 全入力で証明 (`Proofs/Holds.lean`) | Python Language Reference §6.10.1、`sys.version_info` |
| `PythonRange::{from_intervals, union, complement, intersect, is_subset_of}`、`successor` | `manifest/python_range.rs` | Aeneas 翻訳 + 有限格子で全数照合 (`Check/Intervals.lean`、143573 件) | 区間の集合としての意味 (型の doc comment) |
| `specifier_range`、`minor_blocks` の意味 | 同上 | `packaging` 26.3 との 6 万件の差分照合 (下記) | PEP 440 / PEP 508 (`Spec/Pep440.lean`) |
| `normalize_distribution_name`、`uv_lock::normalized` | `manifest/pep508_util.rs`、`manifest/uv_lock.rs` | 全数照合 (`Check/Strings.lean`) | PEP 503 (`Spec/Pep503.lean`) |
| `target_part` | `rules/types.rs` | 全数照合 | RFC 3986 §2.1 (`Spec/TargetPart.lean`) |
| root 相対 path の正規化 5 種 | `pipeline/trace.rs`、`config/types.rs`、`resolver/pytest_path.rs`、`manifest/wheel.rs`、`config/workspace.rs` | 全数照合 | POSIX XBD 3.271 / 4.16 (`Spec/Path.lean`) |

### Aeneas が翻訳できなかったもの

- **`str` の API 全般** (`replace`、`strip_prefix`、`trim_matches`、`chars` など)。Aeneas の
  Lean library は `Str` を byte 列としてしか持たず、`Pattern` trait を取る関数の signature で
  `Unimplemented` になる。文字列関数は `extract` の `dump` binary で小さな alphabet 上の全文字列
  (path は `a . / \` の長さ 6 以下、名前は `A a - _ .` の長さ 6 以下、`target_part` は
  `a % : 3 A 2 5` の長さ 5 以下) を流し、Lean の仕様と照合する。
- **iterator の adapter** (`iter().chain(..).copied().collect()`)。`union` の 1 式だけ局所の
  stand-in `chain_copied_collect` に置き換え、Lean 側は `a ++ b` で model した。`check_sync.py`
  がこの置換を明示的に適用してから比較するので、元の式が変われば `STALE` で落ちる。
- `specifier_range` の `text` の数字だけ判定も stand-in (`digits_only`) にした。
- `pad` (`iter_mut().zip(..)`) は Aeneas の Lean library に `Iterator::zip` の model が無く、
  生成した Lean が型検査を通らない。`pad` を使う `prefix_range` / `specifier_range` と、
  `Iterator::{skip, any, map, filter}` の model を書く必要がある `exact` / `minor_blocks` /
  `minor_runs` は翻訳の対象から外した。`specifier_range` と `minor_blocks` の意味は `packaging`
  との差分照合で代える (`minor_runs` は既存の `version_guard_model.py` の範囲)。

## 実行

```sh
# 1. コピーが src/ と一致するか
python3 docs/dev/formal/lean/aeneas/check_sync.py

# 2. Lean への翻訳 (Charon / Aeneas は AeneasVerif/aeneas fb44cb4 の nix flake で build)
cd docs/dev/formal/lean/aeneas/extract
P=chokkin_extract::python_range
charon cargo --preset=aeneas \
  --start-from "$P::PythonRange::all" --start-from "$P::PythonRange::union" \
  --start-from "$P::PythonRange::complement" --start-from "$P::PythonRange::intersect" \
  --start-from "$P::PythonRange::is_subset_of" --start-from "$P::successor" \
  --start-from chokkin_extract::version_guard::VersionTest::holds \
  --opaque "$P::chain_copied_collect" --dest-file chokkin_extract.llbc -- --lib
aeneas -backend lean -split-files -all-computable -subdir Chokkin -dest .. chokkin_extract.llbc
#    生成される *_Template.lean は捨て、FunsExternal.lean / TypesExternal.lean を使う

# 3. 文字列関数の出力
cargo run -q --release --bin dump > dump.tsv

# 4. 証明と全数照合
cd .. && lake build && lake exe check extract/dump.tsv
```

`lake exe check` は不一致があると 1 で終わる。台帳の未修正の issue が残っている間は落ちる。

## 結果の要約

`ledger.ja.md` を参照。

- `holds` は `read` が作る全 test・全 minor・全 micro で Python のタプル比較と一致する (証明)。
- 区間代数は有限格子上で集合演算と一致し、`from_intervals` の出力は常に正規形。生成した
  `Funs.lean` の merge 条件 `<=` を `<` に変えると照合が落ちることも確かめた (検査が空振りしていない)。
- `requires-python` × marker の 6 万件で、chokkin が「常に成り立つ」と言い `packaging` が
  偽とする例は 0 件。
- PEP 503 の 2 関数と `target_part` は全数で仕様どおり。
- path 正規化は 4 関数が POSIX の pathname 解決と食い違い、うち 2 件が利用者に見える (#1443、#1444)。
