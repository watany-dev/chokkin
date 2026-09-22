# #140: cold parse 支配を踏まえた parser 再評価 (ADR 0001 follow-up)

2026-09-22 時点の調査記録。ADR 0001 (`rustpython-parser` 0.4 採用) の再評価。

## 動機

#140 の実測では、合成 10,703 files / 約 2KiB の **cold parse が 1,248 ms**
(117 µs/file)、パイプライン cold のおよそ 85% を占める。#131 の並列化と #134 の
AST 走査統合を入れても、parser 自体が下限になる。medium project (§17, 2s 以内) は
現状クリアしているが、v1.0 の large monorepo (§16) は 10k 超が前提であり、ここが
最初に当たる。

## crates.io の状況 (2026-09-22 確認)

| 項目 | `ruff_python_parser` | `rustpython-parser` |
|---|---|---|
| crates.io 公開 | あり | あり |
| 最新版 | 0.0.14 | 0.4.0 |
| 最終 publish | 2026-09-16 | 2024-08-06 |
| License | MIT | MIT |
| crate の位置付け | "This is an internal component crate of Ruff" | 独立 parser crate |
| API 安定性 | 0.0.x。stable API の約束なし | 0.x だが 2 年間変化なし |
| 内部 crate 依存 | `ruff_python_ast` / `ruff_python_trivia` / `ruff_text_size` (いずれも 0.0.14 で lockstep) | `rustpython-ast` / `rustpython-parser-core` 0.4 |
| 主な transitive | `bitflags` `bstr` `hashbrown` `memchr` `rustc-hash 2` `stacker` `thin-vec` `unicode-*` | `lalrpop-util` `phf` `itertools` `rustc-hash 1` `unic-*` `num-bigint` |
| build 上の注意 | `stacker` → `psm` は asm を含む build script を持つ。musl / aarch64 の cross wheel で追加検証が要る | pure Rust。現行 7 target の wheel で実績あり |

ADR 0001 の「Triggers to revisit」に対する現状:

- **「`rustpython-parser` unmaintained for > 12 months」— 発火済み。** 最終 publish
  から約 25 か月。upstream RustPython 本体は動いているが、この parser crate 自体の
  release は止まっている。
- **「Astral publishes stable `ruff_python_parser` on crates.io」— 部分的に発火。**
  公開はされたが 0.0.x で、crate description が "internal component crate" と明示して
  いる。semver 上の安定 API とは言えず、ADR 0001 が求めた「stable」条件は満たさない。

## 既知の品質差

`src/parser/syntax.rs` の `SyntaxFeature::Pep695Generics` は宣言のみで
`#[allow(dead_code)]` が付いており、parse 経路から参照されていない。PEP 695 の
generic 構文は rustpython 0.4 で AST に届かず、`syntax_target_hint` が文字列一致で
target hint を返す generic な syntax error として扱われる。`type` alias 文だけは
`note_unsupported_syntax` が AST から検出できる。Ruff parser は 3.12–3.14 構文を
Ruff 本体の要求として追随しているため、この差は今後 Python 3.13 / 3.14 構文を含む
実プロジェクトで広がる方向にある。

## 本 issue で埋まらなかった点

**ruff 側の 10k 実測は取れていない。** 作業環境の agent proxy が organization policy
により `static.crates.io` へ 403 を返すため、新しい crate を download できず、
head-to-head の bench を実行できなかった。したがって受け入れ条件のうち
「比較表」と「見送り理由 + 再評価条件」は本ドキュメントで満たすが、
「ruff parser 側の 10k 実測」は **未取得の open item** として残る。

以下は crate download が可能な環境でそのまま再現できる手順:

```bash
# 1. rustpython 側の baseline (現行コード)
CHOKKIN_BENCH_LARGE=1 cargo bench --bench pipeline --locked -- --save-baseline rustpython-10k

# 2. ruff 側は feature gate 付きの second backend として追加する
#    Cargo.toml:
#      [features]
#      parser-ruff = ["dep:ruff_python_parser", "dep:ruff_python_ast"]
#    src/parser/ に backend module を並べ、parse_file の中身だけ差し替える。
#    ParsedModule / ParseDiagnostic は backend 非依存に保つ。
cargo bench --features parser-ruff --bench pipeline --locked -- --baseline rustpython-10k
```

比較で見るべき軸は #140 の提案どおり、(a) 10k cold parse の median、
(b) `tests/fixtures/parse` と `tests/fixtures/parser_spike` の全 fixture での
構文カバレッジと行番号の一致、(c) 7 target 分の wheel サイズ、(d) `cargo deny` の通過。

## 判断

**この時点では切り替えない。** 理由:

1. `ruff_python_parser` は 0.0.x の internal crate であり、ADR 0001 が切り替え条件と
   した「stable な公開 API」をまだ満たさない。lockstep な 4 crate を 0.0.x で pin する
   のは、chokkin 自身の semver contract (ADR 0004) と釣り合わない。
2. 切り替えの根拠になる 10k head-to-head がまだ無い。速度の優位は期待値であって
   実測ではない。
3. #130 / #131 / #133 / #134 / #136 の一連の改善で cold の parser 以外の部分は
   削れており、まず並列化後の 10k を測り直すのが順序として正しい。

`rustpython-parser` の release 停止は認識した上で受容する。現在 pin している 0.4.0 は
`cargo deny` を通っており、既知の advisory は無い。

## 再評価条件 (ADR 0001 の Triggers を置き換える)

- `ruff_python_parser` が 0.1 以上を publish する、または description から
  "internal component crate" の但し書きが外れる。
- #131 の並列化を入れた状態で 10k cold parse が 500 ms を切らない。
- `rustpython-parser` 0.4.0 に `cargo deny` が拾う advisory が出る。
- Python 3.13 / 3.14 構文を含む実プロジェクトで `CHK` の取りこぼしが fixture として
  再現する。

いずれかが起きた時点で、上記の bench 手順で head-to-head を取り直す。
