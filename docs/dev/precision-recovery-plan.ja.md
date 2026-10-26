# v0.7.x 精度改善の振り返りと v0.8 に向けた再設計プラン

対象: v0.7.0 (2026-10-06) 〜 HEAD `ab369c2` (2026-10-26)。
入力: git 履歴、CHANGELOG、`docs/dev/oss-validation-report.md`、
`docs/dev/precision-gap-analysis.ja.md` (v0.7.2 時点)、`docs/dev/chk001-remove-and-test.md`、
`scripts/oss-fixtures.labels.tsv`、open issue 36 件。

結論を先に書く。v0.7.x の精度が上がり切らないのは、個々の fix の質ではなく、
**「静的 import edge だけを参照とみなし、それ以外は gap として confidence を下げる」**
という参照モデルの限界に達しているからである。FP バケットは 3 週間ずっと同じ
(文字列・動的 import・framework 規約・library public API) で、個別 plugin で 1 慣習ずつ
潰す方法ではバケットは縮まない。計測側も floor を実測に追随させる運用になっており、
gate が「現状維持」を保証するだけで「改善」を要求していない。
次の minor では新しい慣習対応を止め、(1) 参照モデルの一般化、(2) confidence を
証拠集合から導く形への置き換え、(3) 改善を要求する計測、の 3 点に投資する。

---

## 1. v0.7.x で何が起きたか

### 1.1 コミットの遷移

| 区間 | 日付 | commits | fix | feat | 主な内容 |
| --- | --- | --- | --- | --- | --- |
| 0.7.0 → 0.7.1 | 10-06 → 10-08 | 36 | 22 | 3 | CHK005 / CHK009 / CHK010 の個別文脈 (optional・TYPE_CHECKING・suppress)、test data の CHK001 除外 (#593) |
| 0.7.1 → 0.7.3 | 10-08 → 10-09 | 18 | 5 | 2 | app mode 判定の調整 (#664)、examples/ docs/ script を entry (#666)、CHK006 の規約除外 (#655)、`precision-gap-analysis.ja.md` 作成 |
| 0.7.3 → 0.7.4 | 10-09 → 10-10 | 46 | 30 | 3 | examples/ を dev context、CHK010 の info 降格 (134 件) |
| 0.7.4 → 0.7.5 | 10-10 → 10-12 | 17 | 10 | 2 | workspace root 自動判定 (#776)、deprecation shim (#778) |
| 0.7.5 → 0.7.6 | 10-12 → 10-16 | 52 | 19 | 11 | entry 推定の拡張、package map 追加 |
| 0.7.6 → 0.7.8 | 10-16 → 10-20 | 37 | 16 | 5 | main-block / command entry root (#1026, #912)、WSGI/ASGI、Airflow/Dagster/Prefect/Scrapy plugin (#401) |
| 0.7.8 → 0.7.9 | 10-20 → 10-24 | 83 | 50 | 5 | module_loaders (#493)、Django 慣習 module (#399)、poetry/setup.cfg entry (#916)、NoEntryRoot gap (#1138) |
| 0.7.9 → 0.7.10 | 10-24 → 10-25 | 12 | 9 | 1 | Django URLconf include() 追従 (#1094)、INSTALLED_APPS += / MIGRATION_MODULES、全 CHK001 hit のラベルと floor 調整 (#1243) |

合計 301 commits: **fix 161 / feat 32 / chore 32 / docs 20 / test 17 / ci 17 / refactor 10 / perf 7**。
3 週間で 10 リリース、ほぼ毎日リリースしている。

### 1.2 コードの遷移

| 指標 | v0.6.0 → v0.7.0 | v0.7.0 → HEAD |
| --- | --- | --- |
| `src/` 増分 | +9,975 行 | **+58,311 / −8,354 行** (202 files) |
| `tests/` 増分 | | +24,907 行 |
| `src/plugins/` | | +8,635 行 (31 files、現在 13,247 行) |
| `docs/dev/spec.ja.md` | | 1,481 → 1,926 行 (+539) |

v0.7.x の 1 minor で src が 6 割増えた。最も変更回数の多いファイルは
`src/parser/visit.rs` (45 回、3,794 行)、`parser/parse.rs` 40、`pipeline/analyze.rs` 38、
`resolver/resolve.rs` 34、`pipeline/probe.rs` 28、`manifest/extract.rs` 28、
`rules/symbols/analyze.rs` 27、`reachability/build.rs` 26、`reachability/bfs.rs` 25、
`rules/gaps.rs` 23 (現在 2,732 行)。**個別慣習への対応が plugin 層ではなく
parser / resolver / reachability という中核に積み重なっている**。

spec の Django plugin 節 (§9) は 1 慣習ごとに「読む形 / 読まない形 / gap にする形」を
列挙する形で肥大し、Django の例だけで約 15,000 文字になっている。同じ密度で `INSTALLED_APPS` の
`.append()`・`+=`、`include()` の tuple 形、`MIGRATION_MODULES` …と足していく限り、
次の OSS で別の書き方に出会うたびに同じ作業が続く。

### 1.3 リリースごとの精度推移 (`oss-validation-report.md`、ラベル付き precision)

| 時点 | CHK001 | CHK005 | CHK006 | CHK008 | CHK010 |
| --- | --- | --- | --- | --- | --- |
| 10-08 (0.7.1) | 6% | | 50% | | 42% |
| 10-09 (0.7.3) | 7% | 33% | 43% | 14% | 38% |
| 10-10 (0.7.4) | | | 55% | | 51% |
| 10-11 (0.7.5 直前) | 10% | 47% | 80% | 88% | 87% |
| app-mode corpus 追加後 (#1065) | 28.3% | | (app project で低下) | | |
| include() 追従後 (0.7.10) | 48.1% | | | | |
| 0.7.10 (#1243) | **53.5%** | 41.5% | **74.8%** (app) | 52.4% | 84.4% |

10-10 → 10-11 の跳ね上がりは、examples/ の dev context 化と library mode の info 降格
(CHK010 134 件、CHK003 27 件を info へ) による。検出を正しくしたのではなく、
**出す場所を狭めた**ことで precision が上がった区間である (`release-validation.ja.md` §3 の
Demoted 341 件が v0.7.3 → v0.7.4 の実体)。

CHK006 は library corpus で 80〜90% に達した後、app-mode corpus を足すと 74.8% に戻った。
floor は 90 → 70 に下げられた。CHK001 の floor は 25 → 45 → 50 と実測の直後を追っている。
`PRECISION_FLOORS` は現在 `{CHK001: 50, CHK003: 90, CHK004: 95, CHK006: 70, CHK010: 90}`
で、**floor が目標ではなく実測の下限になっている**。

### 1.4 FP の中身は変わっていない

`scripts/oss-fixtures.labels.tsv` (2,323 行) の FP バケット:

| rule | precision | FP バケット (件数) |
| --- | --- | --- |
| CHK001 | 23/48 = 47.9% | dynamic-import 13、library-public 3、external-reference 3、other 3、framework-loaded 2、script-entry 1 |
| CHK005 | 17/41 = 41.5% | optional-import 37 |
| CHK006 | 101/163 = 62.0% | library-public-api 71、name-convention 16、dynamic-import 16、external-reference 14、framework-loaded 8 |
| CHK008 | 11/21 = 52.4% | not-invoked 7 |
| CHK010 | 108/128 = 84.4% | dev-context 10、stdlib 6、first-party-missed 5 |

project 別では CHK001 の FP が babybuddy 8 / airflow 7 / weblate 6、CHK006 が
airflow 15 / weblate 8 / redash 7 / mlflow 6 に集中している。

remove-and-test oracle (`chk001-remove-and-test.md`) も同じことを言っている。
v0.6.0 で CHK001 の break 22 件の内訳は「app 判定された library 7、test data 7、
path / 文字列による load 8」、CHK006 の break は「star re-export 5、文字列参照 5、
framework hook 3」。healthchecks では 30/30 が break (include() gap)。文書の結論は
「文字列 module 参照の plugin route (spec §9) が追うべき修正で、oracle をもっと回すこと
ではない」だった。これは v0.7.2 の `precision-gap-analysis.ja.md` P3
(動的 import / 文字列参照を考慮していない) と同じ指摘で、付録 B の
「confidence を証拠 lattice に」「メタモルフィック性質」「mutation を confidence へ」は
未着手のまま、P3 は include() や module_loaders などの**個別 plugin としてだけ**部分着手された。

### 1.5 open issue も同じ形

10-25 に起票された #1238〜#1242 は rule 別の FP バケットの列挙である
(CHK006 の settings 名・dotted string・登録 decorator、CHK005 の optional / test 文脈、
CHK008 の dev tool、CHK002 の settings backend、CHK001 の layout / entry 取りこぼし)。
このまま 1 件ずつ閉じると v0.7.11 以降も同じ曲線をなぞる。

一方で構造に効く issue も残っている: #1112 (CPython の実 import 結果を正解にした
到達性の差分テスト)、#1113 (gap が立たずに certain が出る入力のテスト)、
#702 (メタモルフィック性質。`tests/prop_metamorphic.rs` に 2 性質だけ着手済み)、
#406 (JSON に evidence)、#882 (TOML/INI 由来の参照に行が無く ignore できない)、
#862 (CHK005/7/8/9 の precision floor)、#398 (v0.8 差別化)。

---

## 2. 本質的な課題

### 課題 A: 参照モデルが「静的 import edge」に閉じている

`src/rules/gaps.rs` の `GapKind::StringModuleRef` のコメントが現状を端的に表す。
文字列で module を名指す箇所 (`lazy_load_command("pkg.cli.x.cmd")`、`"python:pkg.config"`)
は **参照ではなく gap** として扱われ、その file の CHK001 を info / likely に下げるだけである。
つまり「文字列・動的 import・framework 規約・decorator 登録」はすべて
「分からないから弱める」側に置かれ、「使われている証拠」として数えられていない。

結果として:

- 文字列参照を正しく扱える慣習は、plugin が 1 慣習ずつ「module reference」に昇格させた
  ものだけ (Django `include()`、`INSTALLED_APPS`、`MIDDLEWARE`、pytest_plugins、
  entry point …)。昇格していない慣習はすべて FP になるか、gap で丸ごと降格される。
- 降格は file 単位・project 単位 (「到達可能な file に動的 import が 1 つでもあれば
  全 unreachable file を likely に」§11) なので、certain を出せる project が減る一方、
  likely の中の FP は減らない。
- 一般形の「first-party module に解決できる dotted 文字列 literal」は Django settings
  にだけ実装されている (§9「表に無い設定も…module reference にする」)。これは
  settings 以外の場所でも同じ規則で成立するが、そこに広げられていない。

### 課題 B: confidence が証拠の集合から導かれていない

confidence は各 rule が個別に決め (`Confidence::` の参照は gaps.rs 80 箇所、
resolve.rs 20、missing.rs 19、build.rs 19、emit.rs 15、filter.rs 12 …)、
後段の gaps.rs で「gap の種類 × rule」の表で cap / hint に下げる。
「なぜ certain なのか」を説明する正の根拠 (entry からの到達経路、参照の種類、
public surface の判定根拠) は finding に残らないので:

- `--explain` はテキストでしか出せず、JSON には evidence が無い (#406)。FP 報告に
  根拠を添える運用 (spec §17 横断 work) ができない。
- 「gap が立たずに certain が出る」入力を系統的に見つけられない (#1113)。
  gap は負の証拠の列挙であり、正の証拠が無いことを検出する仕組みがない。
- ある fix が precision を上げた理由が「参照を正しく拾った」のか「降格した」のかを、
  リリース検証で毎回 `--strict` の差分から手作業で分ける必要がある
  (`release-validation.ja.md` §3)。

### 課題 C: 計測が「改善」を要求していない

- floor が実測に追随する (CHK006 90 → 70、CHK001 25 → 45 → 50)。gate は退行を止めるが、
  前進を求めない。
- precision を上げる最短手段が「info / maybe への降格」になっており、recall 側の計測が
  弱い (mutation recall は in-repo fixture のみ、maybe プールの FN 率 12% は 1 回計測)。
  降格は見かけの precision を上げつつ、ユーザーには「何も出ない」ツールとして映る。
- ラベルの偏り: CHK003 は deferred 777 件が残り、CHK001 / CHK006 の FP は airflow と
  Django app 群に集中している。corpus 44 project のうち app は 15 で、うち 6 (babybuddy / healthchecks / redash /
  saleor / weblate / wger) は app-mode CHK001 のために #1065 でまとめて足した Django 系
  (redash は Flask) の app である。
  **floor はこの 6 project の個別慣習にどれだけ追従したかの指標になっている**。
- remove-and-test oracle は最も信頼できる正解源だが opt-in で、v0.6.0 と healthchecks の
  2 回しか回していない。

### 課題 D: 対応がコアに積まれ、spec が慣習カタログになっている

個別慣習の判定が parser / resolver / reachability に直接入り、plugin の責務
(spec §9「entry を足す / string・module reference を足す / binary usage を足す」) が
守られていない。spec は「何を読み、何を gap にするか」の列挙が主になり、
「どの規則で読むか」が見えにくい。新しい慣習ごとに 3 層 (parser・plugin・gaps) を
触る必要があり、変更回数の最多ファイルがすべて中核になっている理由がこれである。

---

## 3. プラン

v0.8 の Phase 5 (Knip 相当の運用性) の前に、**v0.7.11〜v0.7.x 終盤を「精度の基盤整備」に
充て、新しい慣習対応の fix を原則止める**。以下 4 つの work stream を並行させる。
期間の目安は合計 4〜6 週。

### Stream 1: 参照モデルの一般化 (課題 A・D)

目標: 「first-party に解決できる名前の出現」を、import edge と同じ一級の参照
(`Reference`) として扱い、その**種類**を finding に残す。

1. **`Reference` の型を定義する**。`kind` に `Import` / `RelativeImport` /
   `DynamicImportLiteral` / `DottedStringLiteral` / `EntryPoint` / `PluginConfig(label)` /
   `Decorator(suffix)` / `Convention(label)` を持ち、`origin` (file・行・field) を必ず持つ。
   既存の plugin の module reference と `StringModuleRef` gap をこの型に寄せる。
2. **汎用 dotted string 参照を parser に 1 つ実装する**。関数本体を含む全スコープで、
   「`.` 区切りの識別子列で、先頭 segment が発見済み first-party の top-level module」
   に一致する文字列 literal を候補にし、module または `module.attr`
   (`module` が存在するもの) に解決できたものだけを `DottedStringLiteral` 参照にする。
   Django settings の「表に無い設定」規則 (§9) をそのまま一般化したもので、
   §12 が懸念する `name="deploy"` のような単語 1 つの文字列は first-party の
   top-level module 名と衝突しない限り拾わない (衝突する project は `--probe` で警告)。
   これで Airflow の `lazy_load_command`、gunicorn の `"python:pkg.config"`、
   celery の `imports = [...]`、`importlib.import_module("pkg.x")` の変数経由、
   dotted string による registry 登録 (#1238 の dotted string) を 1 機構で拾う。
   `StringModuleRef` gap は、文字列が f-string / `+` / `%` / `.format()` で組み立てられ
   解決できない場合にだけ残す。
3. **到達性に効かせ方を 2 段にする**。`DottedStringLiteral` から到達した file は
   CHK001 の対象外 (import edge と同等) にし、trace に `via string "pkg.x" at a.py:12`
   を残す。symbol 側 (CHK006) は `module.attr` に解決できた場合に `attr` を外部参照済みに
   する。plugin が `label` 付きで出す参照はこの上に乗るだけになり、Django `include()` の
   ような個別実装は「文字列をどこから読むか」の指定だけに縮む。
4. **登録 decorator を名前ではなく形で判定する** (#1238 の登録 decorator、#698)。
   現在は `REGISTRATION_DECORATOR_SUFFIXES` の suffix 一致 (§12)。
   「呼び出し形の decorator で、receiver が module level の名前で、その名前が
   third-party または first-party の class instance (`app = Flask()`、`router = APIRouter()`、
   `register = Library()`) に束縛されている」を `Decorator` 参照にし、suffix の表は
   receiver を特定できない場合の fallback に落とす。
5. **plugin の責務を spec §9 の 3 つに戻す**。parser / resolver / reachability に入った
   Django・Airflow 固有の分岐を棚卸しし (`git log --stat v0.7.0..HEAD -- src/parser
   src/resolver src/reachability` に plugin 名を含むコミットから列挙)、plugin に戻すか、
   1〜2 の汎用機構で置き換えられるものは削除する。削除は精度 gate と
   `oss-expectations.tsv` の差分で安全を確認する。

関連 issue: #1238、#1239、#698、#1240 (settings backend は 2 で拾える)、#938、#882
(origin を必ず持たせるので ignore 可能になる)、#1069、#1091、P3、R-08 の一部。

### Stream 2: confidence を証拠集合から導く (課題 B)

目標: finding ごとに `evidence: [Reference | Gap | ModeDecision]` を持ち、
confidence はそこから**1 箇所で**計算する。

1. **`Evidence` を finding に付ける**。正の証拠 (到達経路、参照の種類、public surface の
   判定根拠、mode 判定の signal) と負の証拠 (既存の `Gap`) を同じ list に入れる。
   JSON schema に `evidence` を追加し (#406)、`--explain` はこの list を整形するだけにする。
   SARIF の `relatedLocations` にも載せる。
2. **confidence の導出を 1 関数にする**。`certain` = 「対象を使いうる経路の種類すべてに
   ついて、使われていない正の確認がある、または対象経路が存在しないことが確認できる」、
   `likely` = 「未確認の経路があるが、それが対象を指す兆候はない」、`maybe` = それ以外。
   既存の gaps.rs の「gap 種類 × rule」表は、「未確認の経路」を生成する側に置き換える。
   rule 側に散在する `Confidence::` の直接代入 (resolve.rs、missing.rs、build.rs など
   約 180 箇所) を段階的にこの関数へ寄せる。付録 B の lattice 案の具体化で、
   bitset で持てば `--explain` に「certain に足りない証拠」をそのまま出せる。
3. **「certain なのに正の証拠が無い」を検査する** (#1113)。proptest で生成 project に
   対して「certain の finding は `evidence` に gap を含まず、かつ少なくとも 1 つの
   正の到達確認を含む」を不変条件にする。生成器は `tests/prop_metamorphic.rs` のものを
   拡張する。
4. **mode 判定を証拠に含める** (P1 / P2 / P8)。app / library の判定根拠
  (CLI の有無、wheel target、workspace root の signal) を `ModeDecision` として
   finding に残し、判定が揺れたときに finding がどう動くかを `--explain` で追えるようにする。

関連 issue: #406、#1113、#405 (ty の ADR はここで判断材料が揃う)、付録 B、P1 / P2 / P8。

### Stream 3: 計測を「改善を要求する」形に変える (課題 C)

1. **floor を実測追随から目標値に戻す**。`PRECISION_FLOORS` を現状値で固定し、
   引き下げは ADR 相当の文書 (理由・計測・期限) を伴わない限り禁止する。
   目標値は別途 `PRECISION_TARGETS` として置き (例: CHK001 80 / CHK006 85 /
   CHK005 70 / CHK008 70)、gate は floor で落とし、report は target との差を出す。
   CHK005 / CHK007 / CHK008 / CHK009 にも floor を置く (#862)。
2. **降格を数える**。`release-validation.ja.md` §3 の「Demoted / To info / Recontexted」を
   `oss-metrics.py --gate` の出力に常設し、**「precision の改善分のうち降格由来の割合」**を
   report に出す。降格だけで floor を超えたリリースを可視化する。
3. **CPython oracle を recall / precision の正解源にする** (#1112)。
   安全な範囲は「対象 project の `import` を実行せず、`modulefinder` 相当の静的走査を
   CPython 自身の `importlib` の解決規則で行う」こと。具体的には
   `importlib.util.find_spec` を**対象 project を import しない隔離 interpreter**
   (`-I`、`sys.path` に project root のみ、`sys.modules` を凍結) で呼び、
   chokkin の resolver と module 名 → file の対応を突き合わせる。AGENTS.md の
   「解析対象のコードを実行しない」は守れる (find_spec は親 package の `__init__` を
   import するため、`-I` で `__init__.py` を持つ package は `pkgutil` ベースの走査に
   落とす)。差分は resolver の FP / FN として直接数えられる。
4. **remove-and-test oracle を四半期ではなく minor ごとに回す**。
   `make oss-oracle` を app 群 6 project に限って release validation の必須項目にし、
   break の内訳 (文字列 load / framework hook / test data / library 判定) を
   `chk001-remove-and-test.md` の表に追記する。Stream 1 の効果はここで測る。
5. **ラベルの偏りを直す**。`sample-precision-labels.py` の `--per-project` 上限を
   project 種別 (library / cli / app / framework) ごとの quota にし、CHK003 の
   deferred 777 件は「CHK003 は certain を出し過ぎ」(P4) の判断材料として
   先に 100 件だけ層別で片付ける。app project に Django 以外 (FastAPI / Flask /
   Typer / Streamlit の app) を 4 件足して、floor が Django 慣習の追従度にならないようにする。
6. **メタモルフィック性質を増やす** (#702)。現状 2 性質に次を足す:
   (a) 文字列参照を同値の import に書き換えても finding が増えない、
   (b) plugin を無効にしたとき finding は増えるが `evidence` は減るだけで certain は増えない、
   (c) file を entry に追加したとき CHK001 は減るだけで CHK006 は増えない、
   (d) `__all__` を Partial にしたとき certain は出ない。

関連 issue: #1112、#702、#862、#1223。perf 系 (#1087 / #855 / #843 / #810 / #760) は現状維持。

### Stream 4: 個別 FP バケットを共通機構で閉じる (#1238〜#1242 の扱い)

Stream 1〜3 が入るまで、#1238〜#1242 の個別 fix は原則凍結する。例外は
「1 の汎用機構の受け入れテストになる」ものだけで、issue ごとの出口を次のように決める。

| issue | バケット | 閉じ方 |
| --- | --- | --- |
| #1238 CHK006 app-mode | settings 名 / dotted string / 登録 decorator | Stream 1-2 (dotted string)、1-4 (decorator)、settings 名は config_star_symbols の一般化 |
| #1239 CHK001 layout / entry | 取りこぼし | Stream 3-3 の CPython oracle で resolver 差分として検出してから直す |
| #1240 CHK002 settings backend / shell script | 文字列 | Stream 1-2 で dotted string が third-party module を指す場合を CHK002 の使用証拠にする。shell script は CHK008 provider と同じ reader |
| #1241 CHK008 dev tool | not-invoked | 「宣言はあるが invoke 根拠が無い」を `likely` から `maybe` にするのではなく、invoke の証拠種類 (workflow / tox / pre-commit / Makefile) を evidence にして certain / likely を決める |
| #1242 CHK005 optional / test 文脈 | optional-import 37 件 | `try/except ImportError` と `TYPE_CHECKING` と test context の 3 つを `Reference` の属性 (`guarded`) にし、CHK005 は guarded のみの import を info にする。P6 と同根 |
| #960 star import `__all__` bug | bug | 単独で先に直す (Stream 2-3 の不変条件で再発を止める) |
| #1049 star import 二乗 | perf | Stream 1 で reference を正規化するときに同時に直す |

### やめること

- **OSS 1 件の 1 慣習に対する fix を単独 PR で入れること**。入れる場合は
  「どの汎用規則の特殊ケースか」を PR 本文に書けるものに限る。
- **floor の引き下げ**。
- **info / maybe への降格で precision を上げる変更**を、Demoted の内訳を report に
  出さずに入れること。
- **毎日リリース**。v0.7.11 以降は 2 週間に 1 回にし、release validation
  (`release-validation.ja.md`) を毎回フルで回す。

---

## 4. マイルストーン

| 週 | 内容 | 終了条件 |
| --- | --- | --- |
| 1 | Stream 3-1 (floor 固定・target 導入)、3-2 (Demoted を gate 出力に)、#960 修正、Stream 1-1 (`Reference` 型と plugin reference の移行、挙動不変) | gate の出力に Demoted 列、finding の内部表現に `Reference.kind` と `origin` |
| 2〜3 | Stream 1-2 (汎用 dotted string)、1-3 (到達性と CHK006 への接続)、Stream 2-1 (JSON `evidence`、`--explain` の置き換え) | app 6 project の remove-and-test で CHK001 break の「文字列 load」分類が半減。JSON schema に `evidence` |
| 3〜4 | Stream 1-4 (decorator の形判定)、Stream 2-2 (confidence 導出の一本化、rule 2 つから)、Stream 3-3 (CPython oracle を CI の nightly に) | CHK006 app-mode の precision が降格なしで 80% 以上。resolver の oracle 差分 0 (または issue 化済み) |
| 5〜6 | Stream 1-5 (コアから plugin 固有分岐を除去)、Stream 2-3 (#1113 不変条件)、3-5 (ラベル quota、app corpus 追加)、3-6 (metamorphic 4 性質)、Stream 4 の表に沿って issue を閉じる | `src/parser` `src/resolver` `src/reachability` に framework 名が残らない。CHK001 floor を 70 に**上げて**通る |

Phase 5 (v0.8) の R-08 (framework plugin 拡充) は、この基盤の上で「文字列をどこから
読むか」の指定だけで足せる状態になってから着手する。R-12 (`--trace path:symbol`) は
Stream 2-1 の evidence をそのまま出せるので前倒しできる。

## 5. リスクと判断点

- **汎用 dotted string 参照の FN 側コスト**: 「first-party module 名に一致する文字列」を
  使用とみなすと、本当に未使用の file が文字列 1 つで隠れる。対策は (a) 参照の種類を
  evidence に残して `--explain` で見えるようにする、(b) `DottedStringLiteral` のみで
  到達した file の CHK001 を「消す」のではなく `info / likely` の「string-referenced」
  として残す選択肢を `--strict` に用意する、(c) remove-and-test oracle で FN 率を測る。
  v0.6.0 計測の maybe プール FN 率 12% を上限の目安にする。
- **confidence 一本化の移行コスト**: 約 180 箇所の直接代入を一度に置き換えると
  gate が広く動く。rule 単位 (まず CHK001、次に CHK006) で移し、各段で
  `finding-diff.tsv` の CHANGED を全件説明できることを条件にする。
- **CPython oracle の「実行しない」境界**: `find_spec` は親 package を import する。
  `-I` の隔離 interpreter でも `__init__.py` の副作用は走りうるため、package の走査は
  `pkgutil.iter_modules` と path 走査に限定し、`find_spec` は top-level と
  namespace package の判定にだけ使う。この境界は ADR として残す。
- **期間**: 4〜6 週は新機能を出さない期間になる。v0.7.x の fix 161 件に使った
  3 週間と比べれば、同じ工数で曲線の傾きを変える投資として妥当と判断する。
