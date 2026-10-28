# v0.7.12 の形式検証台帳

v0.7.12 (2026-10-28) で入った判定ロジックから「実装が主張する仕様」と「暗黙に決めている挙動」を抜き出し、
全数探索のモデルと実バイナリ (`target/release/chokkin`) で反証を試みた結果。手順は
`.claude/skills/formal-spec-extraction` に従う。

## 対象の選び方

v0.7.12 の差分 (`ad66f83..35ee2f1`、`src/` で +5233 行) のうち、入力の全組合せで成り立つべき純粋な
判定を持つものを選んだ。

| 対象 | 変更 | 扱い |
|---|---|---|
| `sys.version_info` の読み取りと module-level の名前 (`PY2 = sys.version_info[0] == 2`) | #1272 | `version_guard_model.py` |
| first-party の install check (`check_x()` / `if has_x():` の後の import を optional にする) | #1269, #1272 | `install_check_model.py` |
| `"x" in sys.modules` の body と否定 | #1272 | 実バイナリで確認 (下記 C3) |
| 関数内で module-level の flag を読む (`has_yaml`) | #1272 | 実バイナリで確認 (下記 Q2) |
| `GapKind` の新しい 7 種が finding を弱めるだけであること | #1266, #1273〜#1275 | 既存の `src/rules/gaps.rs` `mod props` が 7 種すべてを参照表に持つことを確認。追加のモデルは作らない |
| tox / nox / workflow の `-r` 参照、`packages.find` の複数 `where` | #1271, #1273 | 入力の解析 (plumbing) が主で、形式化の効きが薄い (skill §7)。対象外 |

## 実装の主張と検証結果

### 成り立つ (契約として固定)

- **C1 version guard は CPython と一致する** (`version_guard_model.py` V1〜V3)。読む 5 形 × 6 演算子 ×
  左右 × 境界の literal と、`not` / `and` / `or`・`if` / `elif` / `else`・`PY2` / `PY3 = not PY2` の名前を
  3.0〜3.30 と最後の bit (3.127 以降) で CPython の演算子による評価と比べ、走りうる minor を落とす例 (健全性違反) も、
  opaque な operand が無いのに走らない minor を残す例も無かった。`sys.version_info > (3, 9)` が 3.9 で
  真になる (`(3, 9, 0, ...) > (3, 9)`) ことも含む。`--trace` で実バイナリにも同じ 2512 本の chain を
  `requires-python = "==3.m.*"` (m = 0〜20) ごとに流し、info / warning がモデルと全件一致した。
- **C2 install check の否定は gate しない**。`if not probe(): import x` の `x`、`try: check() except: pass`
  の後、`if probe() or a:` の body は optional にならない (実バイナリで CHK003 error / certain のまま)。
- **C3 `sys.modules` の否定は gate しない**。`"x" not in sys.modules` の body、`sys.modules.get("x") is None`
  の body、`not sys.modules.get("x")` の body、`elif` の別条件、`or` の片側は optional にならず、
  `else` 側と `"x" not in sys.modules: return` の後だけが optional になる。

### 反例 (挙動が doc comment と食い違う)

すべて finding を **弱める** 向き (import が optional になり、certain な CHK003 / CHK005 が info / likely に
落ちる) で、誤検知は増えない。見逃しが増える。

| ID | 主張 | 反例 (実バイナリで再現済み) |
|---|---|---|
| D1 | `import_checks`: 本体が「無条件に」import を試して raise する関数だけが check | `def check(name): if name != "x": return` の後に `try: import xlib / except ImportError: raise`。`check("y")` の後の `import requests` が optional になる。前の `try` の handler が `return True` でも同じ |
| D2 | handler の最後の文が `raise` なら raise する | `except ImportError: if not strict: return True` の後に `raise`。`check()` の後の `import httpx` が optional |
| D3 | 両方の種類の `try` を持つ関数は raise する | 1 つ目の `try` が `return False`、2 つ目が `raise`。文として呼ぶと、1 つ目の package が無いとき戻るのに後の import が optional |
| D4 | (暗黙) `finally` は見ない | `finally: return` が handler の `raise` を握りつぶすのに check 扱い |
| D5 | `version_names`: module の scope で他に束縛される名前は除く | 関数内の `global NEW; NEW = True`、および後の `from helpers import *` は数えないので、`if NEW:` の import は requires-python が 3.12 未満だけを許すとき「走らない」(info) になる |

D1〜D4 は `install_check_model.py` が G1 の既知の例外として witness ごと固定している (直すとモデルが
落ちるので、docstring と `KNOWN` から外す)。D5 は scope の解析なのでモデルの外に置き、
`version_guard_model.py` の docstring に書いた。

## 確認したいこと (ドメイン判断)

- **Q1 D1〜D4 を直すか**。直すなら、`import_checks` は最初の数える `try` より前に return しうる文
  (`return` を含む文、handler が return する `try`) があれば check にしない、`import_failure` は handler の
  途中に `return` があれば `None`、`finally` に `return` があれば `None`、文の呼び出しは「最初の数える
  `try` が raise する」ときだけ gate する、で足りる。いずれも certain を増やす方向なので、OSS corpus の
  precision を測ってから入れたい。
- **Q2 module-level の flag の再束縛**。`try: import yaml; HAS_YAML = True / except ImportError: HAS_YAML = False`
  の後に module-level で `HAS_YAML = True` と上書きしても、`if HAS_YAML:` の import は optional のまま
  (v0.7.12 より前からの挙動)。`version_names` は「1 回だけ束縛された名前」に限るのに、try flag は
  回数を見ない。揃えるか、flag は上書きされても「True なら import は成功した」と読む方針か。
- **Q3 D5 を直すか**。`global` での再束縛は `bound_names` が関数本体を見ないため。`*` import は名前を
  列挙できないので、`*` import のある module では名前を読まない、が保守的。どちらも稀な形。

## 再現

```bash
make formal                                     # 2 モデルを含む全モデル
python3 docs/dev/formal/version_guard_model.py --broken
python3 docs/dev/formal/install_check_model.py --broken
cargo build --release
python3 docs/dev/formal/version_guard_model.py --trace target/release/chokkin   # 20 秒ほど
```
