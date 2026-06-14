# chokkin 実装プラン索引

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
| 5 | [step-05-config-plugin-extraction.md](./step-05-config-plugin-extraction.md) | 確定 | ✅ |
| 6 | [step-06-python-parse.md](./step-06-python-parse.md) | 確定 | ✅ |
| 7 | [step-07-import-resolution.md](./step-07-import-resolution.md) | 確定 | ✅ |
| 8 | [step-08-entry-root-construction.md](./step-08-entry-root-construction.md) | 確定 | ✅ |
| 9 | [step-09-reachability-analysis.md](./step-09-reachability-analysis.md) | 確定 | ✅ |
| 10 | [step-10-dependency-reconciliation.md](./step-10-dependency-reconciliation.md) | 確定 | ✅ |
| 11 | [step-11-symbol-usage-analysis.md](./step-11-symbol-usage-analysis.md) | 確定 | ✅ |
| 12 | [step-12-issue-emission.md](./step-12-issue-emission.md) | 確定 | ✅ |
| 13 | [step-13-optional-fix.md](./step-13-optional-fix.md) | 確定 | ✅ |

## Phase 0 / Phase 1 / Phase 1.5 横断

| 項目 | ドキュメント | 状態 | 実装 |
| --- | --- | --- | --- |
| Parser spike + graph core | [phase-0-parser-spike-graph-core.md](./phase-0-parser-spike-graph-core.md) | 確定 | ✅ |
| CLI 縦スライス（probe） | [phase-0-cli-vertical-slice.md](./phase-0-cli-vertical-slice.md) | 確定 | ✅ (`--probe`) |
| bundled maps | [step-07](./step-07-import-resolution.md) §3.2–3.3 | 確定 | 🟡 seed あり |
| wheel + PyPI release | spec §15, `release.yml` | CI のみ | ⬜ 未タグ |
| **フル CLI + reporter** | [phase-1-cli-reporter.md](./phase-1-cli-reporter.md) | 確定 | ✅ |
| OSS dogfooding + §17 gate | `scripts/oss-metrics.sh` | 確定 | ✅ 計測済み |
| **v0.1 誤検知是正** | [phase-1.5-fp-remediation.md](./phase-1.5-fp-remediation.md) | 確定 | ✅ |

## 推奨実装順（クリティカルパス）

```mermaid
flowchart TB
  subgraph done [完了]
    S1[Step 1]
    S2[Step 2]
    S3[Step 3]
    S4[Step 4]
    S5[Step 5]
    S6[Step 6]
    S7[Step 7]
    S8[Step 8]
    S9[Step 9]
    S10[Step 10]
    S11[Step 11]
    S12[Step 12]
    S13[Step 13]
    P1[Phase 1 CLI]
    P15[Phase 1.5 FP remediation]
  end

  subgraph remaining [v0.1 リリース前]
    GATE[oss-metrics --gate ✅]
    TAG[PyPI v0.1 tag]
  end

  done --> GATE
  GATE --> TAG
```

## v0.1 リリース前の残作業（§17 exit criteria）

**計測済み (2026-06-14, Phase 1.5 完了後):** crash 0 ✅、cold medium ≤ 2s ✅、CHK002 FP **0%** ✅。
詳細は [`oss-validation-report.md`](../oss-validation-report.md)。

残作業:

1. PyPI `v0.1.0` タグ（Trusted Publishing 設定後）

Phase 1.5（[phase-1.5-fp-remediation.md](./phase-1.5-fp-remediation.md)）は完了済み:

1. **4.D** package-module-map 拡充 + 自己参照 extra guard（~11 件）
2. **4.A** binary + config usage detection（~110 件）
3. **4.B** dev context policy + PDM/Hatch 読取
4. **4.C** optional / conditional import tracing
5. `make oss-metrics ARGS=--gate` 再計測 → CHK002 FP < 5% ✅
6. PyPI `v0.1.0` タグ（Trusted Publishing 設定後）⬜

## ADR

| ADR | 内容 |
| --- | --- |
| [0001-parser-selection.md](../adr/0001-parser-selection.md) | `rustpython-parser` 採用 |
