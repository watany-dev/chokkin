# O6 / O7 台帳: PEP 508 marker・PEP 440 と import 名 ↔ distribution (Lean 4)

対象 commit: main 7166805 (v0.7.14 の release build)。1 件 = 1 つの根本原因 (別の修正が要るもの)。

## 見つかったもの

| # | issue | 領域 | 方向 | 確度 | oracle | 生成入力での不一致 |
|---|---|---|---|---|---|---|
| 1 | #1391 | venv の PEP 420 namespace portion を root でまとめる | FP (CHK002) | certain | `Dist.resolve` | 40 件中 8 件。実 wheel (opentelemetry-instrumentation-httpx / -logging) でも再現 |
| 2 | #1395 | METADATA の description を header として読む (field 名の大小も区別) | FP (CHK002) | certain | `Dist.headers` | 40 件中 5 件 |
| 3 | #1398 | PEP 794 `Import-Name` の `; private` と dotted 名 | FP (CHK010 / CHK002) | likely | `Dist.importNameValue` | 40 件中 10 件 |
| 4 | #1401 | RECORD の top-level 拡張 module のファイル名 | FP (CHK010 / CHK002) | likely | `Dist.moduleOfFile` | 40 件中 9 件 |
| 5 | #1404 | RECORD を CSV として読まない | FP (CHK010 / CHK002) | likely | `Dist.csvFirst` | 固定ケース |
| 6 | #1406 | `*.egg-info` を読まない | FP (CHK010 / CHK002) | likely | `Dist` の `Kind.egg` | 40 件中 15 件 |
| 7 | #1410 | setuptools `extras_require` の `extra:marker` key | FP (CHK009) + FN (CHK002) | likely / certain | `Setuptools.splitKey` + `Marker.verdict` | 固定ケース |

合計 7 件。既知の issue (`gh api search/issues` で Import-Name / METADATA / RECORD / extras_require / namespace venv / dist-info / marker / egg-info / site-packages) との重複は無かった。

## 当たらなかったもの

- **marker が `requires-python` の下で常に成り立つか (#919 の `marker_always_holds`)**: 4,000 marker (python_version / python_full_version / os_name / sys_platform、`'3.8' < python_version` の逆向き、`==X.*`、`~=`、0 埋め、`03.09` など) で FP 0 件。FN は `>3.*` のように PEP 440 として読めず文字列比較になる比較と os_name の恒真式だけで、どちらも `python_range.rs` が「model の外は条件付き」と明記している範囲。
- **PEP 508 requirement の受理範囲**: packaging 26.3 と 300 件突き合わせて差なし (既存の `tests/pep508_packaging_golden.rs` が効いている)。
- **PEP 503 正規化**: `METADATA` の `Name` の綴り (`Zz_Alpha`、`zz.alpha`) と宣言名の違いはすべて一致。
- 上の 7 原因の生成を止めた venv 80 件で不一致 0 件。

## 仕様と証明

- `MarkerNaming/Pep440.lean`: PEP 440 の release 比較 (0 埋め)・pre-release・specifier (`<V` が V の pre-release を許さないこと、`==V.*`、`~=`、`===`)。
- `MarkerNaming/Marker.lean`: PEP 508 marker の文法と packaging の評価規則 (Specifier として読めれば `contains(prereleases=True)`、読めなければ文字列比較)。`verdict` は X.Y.Z (X∈2..4, Y<17, Z<7) と OS / platform の全組で判定する。
- `MarkerNaming/Naming.lean`: PEP 503 / 685 の正規化。
- `MarkerNaming/Dist.lean`: Core Metadata の header、PEP 794、RECORD (CSV)、egg-info、importlib の `FileFinder` (regular package > module > namespace portion) から「import が読み込むファイルの持ち主」を定義する。
- `MarkerNaming/Setuptools.lean`: `extras_require` の key。
- `MarkerNaming/Proofs.lean` で証明したもの: 正規化の冪等性 (`norm_idem`)、release 比較の反射性・0 埋め・反対称 (`cmpRelease_refl` / `_pad` / `_swap`)、`!=` が `==` の補集合 (`ne_complement`)、`~=` の定義 (`compat_def`)、`requires-python` の項を足すと許す interpreter が減るだけ (`admits_cons`)、`:` の無い key は extra (`splitKey_plain`)。PEP の本文の例 (PEP 503 の friendly-bard、PEP 440 の `~=2.2`・`==3.1.*`・`<3.1`) は `native_decide` で評価して固定している。

## 再現

```sh
# Lean 4.15.0 (elan を使わない場合は release zip を展開して PATH に足す)
cd docs/dev/formal/lean/marker_naming && lake build
cargo build --release
cd harness
python3 diff_markers.py 1 25          # O6: marker
python3 diff_venv.py 1 60             # O7: 生成した .venv
SKIP=ext,egg,portion,private,body,lc python3 diff_venv.py 7 80   # 既知の原因を除くと 0 件
python3 cases.py                      # 生成に入らない固定ケース (#1404 / #1398 / #1410 など)
python3 real_venv.py <path/to/.venv> opentelemetry.instrumentation.httpx ...   # 実 wheel
```
