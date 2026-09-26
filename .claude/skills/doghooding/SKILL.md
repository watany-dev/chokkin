---
name: doghooding
description: >
  Dogfood chokkin against a real OSS Python project. Clone the named
  repository, run the three-way comparison (chokkin vs deptry + vulture) over
  it, and turn chokkin's misses, false positives, and crashes into anonymised
  issues with minimal fixtures — the upstream project is never named in
  anything that gets committed or filed. Fire when the user invokes
  /doghooding or says "doghooding", "この OSS でベンチを取って",
  "実プロジェクトで chokkin を試して", "dogfood against <repo>".
---

# doghooding

指定された OSS の Python プロジェクトを持ってきて三者比較にかけ、chokkin 側の
穴だけを**匿名化した最小 fixture と issue** に落とす。上流の名前・本文・行番号は
成果物に一切残さない。

対象は引数で受け取る (`/doghooding owner/repo`)。指定が無ければ何を対象に
するか聞く。

`scripts/oss-clones.manifest` の固定 corpus (§17 ゲート) に入れるのとは別物。
対象を corpus に昇格させたいときはユーザーに確認してから別タスクで行う。

## 前提

```bash
cargo build --release --locked       # 採点対象のバイナリ (target/release/chokkin)
uvx deptry@0.25.1 --version          # 比較対象 1: 依存の整合性
uvx vulture@2.16 --version           # 比較対象 2: 未使用コード
```

- 比較ツールの版は上のピン留めに合わせる。版が違うと差分がツール更新由来か
  chokkin 由来か切り分けられない。上げるときはこのファイルも更新する。
- **解析対象のコードは実行しない** (chokkin の不変条件)。`pip install` /
  `uv sync` / `python setup.py` / テスト実行は、build backend や import 時副作用で
  上流のコードが走るので禁止。3 ツールとも静的解析だけで回す。deptry は環境に
  パッケージが無いとモジュール名を推測する (`Assuming the corresponding module
  name ...`) が、それで構わない — 推測由来の差分は Phase 3 で除外する。
- 取得したプロジェクトは**リポジトリの外**（スクラッチ領域）に置く。上流の
  ライセンスを持つファイルをこのリポジトリに入れない。`target/oss-clones/` は
  `make oss-clones` が管理するので使わない。

## Phase 1: 取得

```bash
work=$(mktemp -d)
git clone --quiet --depth 1 https://github.com/<owner>/<repo> "$work/src"
git -C "$work/src" rev-parse HEAD    # 手元の記録用。成果物には書かない
ls "$work/src"
```

monorepo なら Python プロジェクトのルート (`pyproject.toml` のある階層) を
決めて以降の `cd` 先にする。uv workspace ならルートでよい。

## Phase 2: 三者実行

3 本とも同じディレクトリに対して、キャッシュ・ネットワーク無しで走らせる。

```bash
ck=$PWD/target/release/chokkin     # リポジトリのルートで解決してから移動する
cd "$work/src"
"$ck" --reporter json --no-cache --confidence maybe . \
  > "$work/chokkin.json"; echo "chokkin exit=$?"
uvx deptry@0.25.1 . --no-ansi --json-output "$work/deptry.json" \
  > "$work/deptry.log" 2>&1; echo "deptry exit=$?"
uvx vulture@2.16 . --min-confidence 60 --exclude '.venv,build,dist,.chokkin' \
  > "$work/vulture.txt"; echo "vulture exit=$?"
```

終了コードは 3 本とも「指摘があった」で非ゼロになる (chokkin 1、deptry 1、
vulture 3)。実行エラーと読み違えないこと。ただし **chokkin の exit 2
(config エラー) と exit 3 (内部エラー)** は Phase 3 の B 群として必ず拾う。
`--confidence maybe` は取りこぼしを見るため。既定 (`likely`) の結果は
`summary.by_code` と `confidence` で後から絞れる。`--strict` の差分は参考扱い。

3 本の結果を突き合わせる。依存系は名前、コード系は `(ファイル, 行)` が鍵:

```bash
python3 - "$work" <<'PY'
import collections, json, pathlib, re, sys

work = pathlib.Path(sys.argv[1])
norm = lambda s: re.sub(r"[-_.]+", "-", s).lower()
rows = collections.defaultdict(lambda: collections.defaultdict(set))
for i in json.loads((work / "chokkin.json").read_text())["issues"]:
    if i["code"] == "CHK002":
        key = ("dep", norm(i["distribution"] or i["target"]))
    else:
        key = (i["file"] or i["target"], i["line"] or 0)
    rows[key]["chokkin"].add(f'{i["code"]}({i["confidence"]})')
for d in json.loads((work / "deptry.json").read_text()):
    loc = d["location"]
    key = ("dep", norm(d["module"])) if d["error"]["code"] == "DEP002" else (loc["file"], loc["line"] or 0)
    rows[key]["deptry"].add(d["error"]["code"])
pat = re.compile(r"^(.+?):(\d+): unused (\w+(?: \w+)?) '")
for line in (work / "vulture.txt").read_text().splitlines():
    if m := pat.match(line):
        rows[(m[1].removeprefix("./"), int(m[2]))]["vulture"].add(m[3])
for key, by in sorted(rows.items(), key=lambda kv: tuple(map(str, kv[0]))):
    cols = " | ".join(f"{t}={','.join(sorted(v))}" for t, v in sorted(by.items()))
    print(f"{key[0]}:{key[1]} {cols}" + ("  <- FN候補" if "chokkin" not in by else ""))
PY
```

対応関係の目安:

| deptry / vulture | chokkin |
|---|---|
| DEP001 missing | CHK003 (未知モジュールなら CHK010) |
| DEP002 unused | CHK002 |
| DEP003 transitive | CHK004 |
| DEP004 misplaced dev | CHK005 |
| DEP005 stdlib 宣言 | 無し (既知の gap、R-15) |
| vulture `unused function` / `class` / `variable` | CHK006 / CHK007 (意味が違う。下の表) |
| — | CHK001 / CHK008 / CHK009 は比較対象なし。読んで妥当性だけ見る |

`FN候補` の印は機械的な突き合わせでしかない。chokkin の CHK003 と deptry の
DEP001 は同じ import 行を指すが、名前の突き合わせは表記揺れ (`PyYAML` と
`yaml`) でずれる。**元のファイルの該当箇所を必ず自分で読み**、chokkin 側は
`--explain <CODE>:<target>` / `--trace <path>` で判断理由を確かめてから起票する。

## Phase 3: 差分の分類

| 群 | 中身 | 行き先 |
|---|---|---|
| A. FN | deptry / vulture が出して chokkin が出さない、かつ指摘が妥当 | gap として起票 |
| B. 堅牢性 | chokkin の exit 2 / exit 3・parse error・ファイルやエントリの黙った脱落 | 最優先で起票。実ワールドの書き方は意図したケースより価値が高い |
| C. FP | chokkin だけが出していて、読んだ結果その指摘が誤り | 起票 |
| D. unique-win | chokkin だけが出していて妥当 | 記録のみ。起票しない |

次の差分は意図した仕様なので A にも C にも数えない。

- deptry がパッケージ未導入でモジュール名を推測したことによる DEP001 / DEP002
  (chokkin は bundled map と `[tool.chokkin.package_module_map]` で解く)
- optional / platform-guarded / `TYPE_CHECKING` 下の import に対する DEP001 —
  chokkin は既定で CHK003 を info に落とす (`--strict` で出る)
- deptry が見ない情報源 (dev group、tox / nox / pre-commit / CI の binary
  usage、plugin 設定) で chokkin が CHK002 を抑制した分
- vulture の `unused import` (ruff F401 の領域)、`unused method` /
  `attribute` / `property` (クラスメンバーは R-16)、エントリ関数・framework の
  hook に対する指摘
- vulture は「どこからも使われない」、CHK006 は「モジュールの外から使われない」。
  モジュール内でだけ使う公開シンボルに CHK006 が出て vulture が出ないのは正常

既に `docs/dev/roadmap-gap-analysis.ja.md` §3 に R 番号がある gap の再発なら、
新規起票ではなくその issue / 節に追記する。

## Phase 4: 最小 fixture に置き換える

**上流のファイルをそのまま持ち込まない。** 現象を再現する最小のプロジェクトを
自分で書き直す。

- 置き場は検出段に合わせて `tests/fixtures/<category>/<name>/`
  (依存なら `deps/`、到達性なら `reachability/`、シンボルなら `symbols/`、
  parse なら `parse/`)。既存 fixture (`tests/fixtures/deps/unused_boto3/` など)
  と同じく `pyproject.toml` + `src/acme/` の形にする
- ファイル 2〜3 個、各 20 行前後。その現象に要らない依存・モジュール・設定は
  全部落とす
- 名前は汎用語 (`acme`, `main.py`, `boto3`, `requests`, `example.com`)
- 落としながら毎回 3 ツールを回し、**差分が消えない最小形**まで削る

削り終えたら、その fixture だけを見て「どのプロジェクト由来か分かるか」を
確かめる。分かるなら削り足りない。

fixture は起票時点ではコミットせず、issue 本文に載せる。直す PR で fixture と
テストをまとめて入れる (失敗するテストを先に入れない — `make check` が通らない)。

## Phase 5: 匿名化

コミットするもの・起票するもの・PR 本文のすべてから次を落とす。

- リポジトリ名・組織名・製品名・その略称、GitHub の URL とコミット SHA
- 上流固有のパッケージ名・モジュール名・社内 index / レジストリ・ホスト名
- 環境変数名・ブランチ名・パス・関数名/クラス名の固有部分
- 上流ファイルの引用と行番号 (「実プロジェクトで見つかった」まで)

起票の書き出しは「実運用の Python プロジェクトを三者比較したところ」でよい。
どの OSS だったかは会話の中だけに留め、成果物には残さない。

**上流の本物の問題 (脆弱な依存・秘密情報の混入など) を見つけたら公開 issue に
書かない。** chokkin 側の課題 (検出の有無) だけを起票し、上流の問題そのものは
報告せず、ユーザーにそのまま伝えて判断を仰ぐ (責任ある開示の対象)。

## Phase 6: 起票と取り込み

1. A / B / C を 1 件ずつ issue にする。表題は現象で書く (対象名は入れない)。
   本文に「再現する最小 fixture (Phase 4)」「3 ツールの出力」「chokkin の
   `--explain` / `--trace`」「期待する挙動」を入れる
2. 新しい種類の gap (A) なら `docs/dev/roadmap-gap-analysis.ja.md` §3 の
   該当優先度に次の空き番号で `R-<n>` を足し、issue 番号を添える
3. 直すのは別タスク。直す PR では fixture をテスト (`tests/deps_reconcile.rs`
   など) から参照し、CHK002 / CHK003 / CHK004 の FN を直したなら
   `scripts/oss-recall.manifest` に sentinel を、`scripts/oss-fixtures.labels.tsv`
   に `tp` ラベルを足して recall ゲートで守る
4. コミットする前に `make check` を通す

最後に `wrapup` を通す。

## 報告

ユーザーには次を返す。対象の実名を出してよいのは**この報告だけ**。

- 対象と Python ファイル数、3 ツールの指摘件数 (chokkin は `by_code`) と実行エラー
- A / B / C / D の件数と、起票した issue 番号
- 意図した差分として除外した件数の内訳
