# O5 lockfile 閉包オラクル 台帳

Lean 4 で lockfile の閉包を形式の規格から書き、chokkin の実バイナリ (`--reporter json`) と差分実行した結果。1 件 = 別の修正が要る根本原因 1 つ。

## 仕組み

- `Lockfile/Spec.lean`: uv.lock / poetry.lock / pdm.lock / pylock.toml (PEP 751) の書き方から、node `(package, extra?)` の上の到達可能性 `Reach` (最小不動点) と、その実行版 `closure` を定義する。未宣言 import の判定 `classify` (none / CHK004 certain / CHK004 likely / CHK003) は chokkin spec §10 の優先順をそのまま書いた。marker は評価しない (§10・#1276 と同じ過大近似)。
- `Lockfile/Proofs.lean`: `closure_sound` (返すものはすべて到達可能)、`complete_of_closed` と `closure_exact` (閉じていれば `Reach` と一致する。oracle は実行時に `isClosed` を確かめ、閉じていなければ答えない)、`reach_mono_seeds` / `reach_mono_lock` (宣言や lock の entry を増やしても閉包は縮まない)、`reach_through_edge_extra` (`q[x]` への edge は `q` の extra `x` の先まで届く)。
- `Main.lean`: JSON Lines で scenario を受け取り、import ごとの判定を返す `oracle`。
- `harness/gen.py`: 小さな lock graph を生成し、各ツールが書く形の lockfile に描画して両者を比べる。uv は高速 scanner の経路と TOML parser の経路の 2 通り (`uv-toml`) で描画する。dev group の宣言、extra だけを使う宣言 (CHK002)、`ci.sh` の binary (CHK008) も混ぜる。
- `harness/real.py`: OSS clone の lockfile を `tomllib` で読んで model にし、宣言の組を無作為に選んで比べる。
- `harness/uv_layout.py`: 実 uv.lock を TOML として同じ意味の書き方 (先頭 comment、CRLF、末尾の空行) に変えても判定が変わらないことを確かめる metamorphic check。
- `harness/explain.py`: 不一致ごとに、どの lock の機能 (宣言の extra、edge の extra) が要ったかで分ける。

## 再現

```sh
cd docs/dev/formal/lean/lockfile
lake build                                   # 証明の検査と oracle のビルド
python3 -I harness/gen.py --chokkin ../../../../../target/release/chokkin \
    --oracle .lake/build/bin/oracle --cases 300 --seed 3 --out /tmp/mm.jsonl
python3 -I harness/explain.py /tmp/mm.jsonl
python3 -I harness/real.py --chokkin ... --oracle ... --clones <clones> --samples 4
```

## 実行規模 (v0.7.14 HEAD 7166805)

- 生成入力: 5 形式 × 計 660 scenario (seed 1〜3、約 3,300 project)。
- 実 lockfile: 25 リポジトリ (uv 22、poetry 1、pdm 1、pylock 1)、各 4 通りの宣言で約 11,000 import。
- uv scanner と TOML parser の metamorphic check: 実 uv.lock 22 件 × 3 通りの書き換えで、不一致 0。

## 見つかった根本原因

| # | issue | 方向 | 内容 | 根拠 |
|---|---|---|---|---|
| 1 | #1374 | 取りこぼし | 宣言 `p[e]` の extra が入れる配布物の先を閉包で辿らず、CHK004 が likely に下がる | 生成 + rich / mcp-python-sdk |
| 2 | #1375 | 取りこぼし | uv.lock の edge の `extra = [...]` を読まず、`q[x]` 経由の配布物が transitive edge にならない | 生成 + airflow / typer / mcp-python-sdk |
| 3 | #1376 | 誤検知 + 取りこぼし | poetry.lock / pdm.lock の extras を読まず、明示的に要求した配布物が CHK004 certain error になる | 生成 + python-poetry/poetry |
| 4 | #1377 | 誤検知 (メッセージ) | dev group / extra の宣言からの edge で runtime import を CHK004 certain にする (`--production` でも同じ) | 生成 |
| 5 | #1378 | 誤検知 | CHK008 の provider が lock の 1 hop しか見ず、宣言の extra や 2 段先の binary を未宣言と言う (`fastapi[standard]` → `uvicorn`) | 生成 + 手で確認 |
| 6 | #1379 | 誤検知 | extra が入れる配布物だけを使う宣言 `p[e]` が CHK002 certain になる | 生成 |
| 7 | #1380 | 頑健性 | TOML として読めない lockfile で次の候補へ進まず、解析全体が exit 2 で止まる | 手で確認 (選択規則を書き出す過程) |

計 **7 件**。

## 数えなかった不一致

- poetry.lock の `optional = true` の依存と、pdm.lock の extras entry を base に合流させることによる過大近似 (CHK004 likely であるべきところが certain になる)。§10 と #1276 で「lock の edge は入っている証拠」として意図した設計なので、件数に入れない。#3 を直すと一部は解消する。
- `scripts/tool.py.lock` (uv の script 用 lockfile) を読まず、script の推移依存が CHK003 になる。§10 は script を block の宣言だけと照合すると定めていて、メッセージも事実と合っている。機能の不足として扱い、件数に入れない。
- 当たらなかった観点: pdm の要件文字列の書き方 (`name @ url`、`-e ...#egg=`、`name (>=1)`、前置空白、`[a,b]`)、各形式の名前の表記揺れ (大文字、`_`、`.`)、extra 名の正規化、workspace member の閉包の起点 (#649)、uv の高速 scanner (#592)。どれも仕様と一致した。
