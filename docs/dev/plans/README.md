# yokei 実装プラン索引

`docs/dev/spec.ja.md` §6 パイプラインと §17 ロードマップに対応する設計ドキュメント一覧。
各プランは **update-plan 合格**（90点以上）を付記してから実装に進む。

**横断検証:** [`VALIDATION.md`](./VALIDATION.md)（Steps 9–13 + Phase 1 CLI、2026-06-13 合格）

## パイプライン Steps 1–13

| Step | ドキュメント | 状態 | 実装 |
| ---: | --- | --- | --- |
| 1 | [step-01-root-discovery.md](./step-01-root-discovery.md) | 確定 | ✅ |
| 2 | [step-02-config-load.md](./step-02-config-load.md) | 確定 | ✅ |
| 3 | [step-03-manifest-extraction.md](./step-03-manifest-extraction.md) | 確定 | ✅ |
| 4 | [step-04-source-file-discovery.md](./step-04-source-file-discovery.md) | 確定 | ✅ |
| 5 | [step-05-config-plugin-extraction.md](./step-05-config-plugin-extraction.md) | 確定 | ⬜ |
| 6 | [step-06-python-parse.md](./step-06-python-parse.md) | 確定 | 🟡 spike のみ |
| 7 | [step-07-import-resolution.md](./step-07-import-resolution.md) | 確定 | ⬜ |
| 8 | [step-08-entry-root-construction.md](./step-08-entry-root-construction.md) | 確定 | ⬜ |
| 9 | [step-09-reachability-analysis.md](./step-09-reachability-analysis.md) | 確定 | ⬜ |
| 10 | [step-10-dependency-reconciliation.md](./step-10-dependency-reconciliation.md) | 確定 | ⬜ |
| 11 | [step-11-symbol-usage-analysis.md](./step-11-symbol-usage-analysis.md) | 確定 | ⬜ |
| 12 | [step-12-issue-emission.md](./step-12-issue-emission.md) | 確定 | ⬜ |
| 13 | [step-13-optional-fix.md](./step-13-optional-fix.md) | 確定 | ⬜ |

## Phase 0 / Phase 1 横断

| 項目 | ドキュメント | 状態 | 実装 |
| --- | --- | --- | --- |
| Parser spike + graph core | [phase-0-parser-spike-graph-core.md](./phase-0-parser-spike-graph-core.md) | 確定 | 🟡 骨格あり |
| CLI 縦スライス（probe） | [phase-0-cli-vertical-slice.md](./phase-0-cli-vertical-slice.md) | 確定 | ⬜ |
| bundled maps | [step-07](./step-07-import-resolution.md) §3.2–3.3 | 確定 | ⬜ |
| wheel + PyPI release | spec §15, `release.yml` | CI のみ | ⬜ 未タグ |
| **フル CLI + reporter** | [phase-1-cli-reporter.md](./phase-1-cli-reporter.md) | 確定 | ⬜ |

## 推奨実装順（クリティカルパス）

```mermaid
flowchart TB
  subgraph done [完了]
    S1[Step 1]
    S2[Step 2]
    S3[Step 3]
    S4[Step 4]
  end

  subgraph phase0 [Phase 0 残り — 並行可]
    CLI[CLI probe]
    P0G[graph/parser spike ✅]
    MAPS[bundled maps]
  end

  subgraph core [v0.1 クリティカルパス]
    S5[Step 5 plugins]
    S6[Step 6 parse]
    S7[Step 7 resolver]
    S8[Step 8 entry]
    S9[Step 9 reachability]
    S10[Step 10 deps]
    S11[Step 11 symbols]
    S12[Step 12 issues]
    S13[Step 13 fix]
    P1[Phase 1 CLI]
  end

  S4 --> CLI
  S4 --> S5
  S4 --> S6
  P0G --> S6
  S6 --> S7
  MAPS --> S7
  S5 --> S8
  S6 --> S8
  S7 --> S9
  S8 --> S9
  S9 --> S10
  S6 --> S11
  S9 --> S11
  S10 --> S12
  S11 --> S12
  S12 --> S13
  S12 --> P1
  S13 --> P1
```

## 設計完了 — 実装フェーズ

**パイプライン Steps 1–13 + Phase 0/1 CLI の設計はすべて確定済み。**

残作業は実装と Phase 1 exit criteria（§17）:

- OSS 20 プロジェクト dogfooding
- YOK002/YOK003 誤検知率 5% 未満
- medium project cold 2s 以内

## ADR

| ADR | 内容 |
| --- | --- |
| [0001-parser-selection.md](../adr/0001-parser-selection.md) | `rustpython-parser` 採用 |
