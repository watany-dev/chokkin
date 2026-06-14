# update-plan 横断検証サマリ

Steps 9–13 + Phase 1 CLI の `/update-plan` 実行結果（2026-06-13）。
個別プランの詳細は各 `step-*.md` / `phase-1-cli-reporter.md` §update-plan を参照。

## 総合判定

| プラン | 得点 | 判定 | ブロッカー |
| --- | ---: | --- | --- |
| step-09-reachability | 97 | ✅ 合格 | なし |
| step-10-dependency-reconciliation | 97 | ✅ 合格 | なし |
| step-11-symbol-usage | 96 | ✅ 合格 | なし |
| step-12-issue-emission | 97 | ✅ 合格 | なし |
| step-13-optional-fix | 95 | ✅ 合格 | なし |
| phase-1-cli-reporter | 96 | ✅ 合格 | なし |

**横断合格 — 実装フェーズへ移行可。**

## 横断で修正した課題（Phase 4）

| 優先度 | 課題 | 対応プラン |
| --- | --- | --- |
| **P0** | `IssueConfidence` と `config::Confidence` の重複 | step-09, 10, 12 — `Confidence` に統一 |
| **P0** | Step 13 の YOK002 fix 条件が §2 と §3.1 で矛盾 | step-13 — **Certain のみ**に統一 |
| **P1** | try-import (`optional_missing`) が Step 10 に未記載 | step-10 §3.4 追加 |
| **P1** | `RuntimeOverrides` に `no_exit_code` 等が未定義 | phase-1 §6.1, step-12 参照 |
| **P1** | `SymbolId` が graph 未存在と衝突 | step-11 — rules ローカル ID |
| **P1** | `UsedDependencyIndex` の定義場所が不明瞭 | step-09 — `used_modules` 経由 |
| **P2** | Step 8 / 11 / 13 の検証サマリが簡略 | 各プランで Phase 1–4 形式に拡充 |

## 型の横断契約（実装時 SoT）

```text
config::Confidence     — issue confidence 軸（表示フィルタ / exit 判定）
rules::types::Severity — error | warning | info
rules::types::RuleId   — YOK001–YOK010
rules::types::IssueCandidate — Step 10–11 出力
rules::types::Issue    — Step 12 最終形
config::RuntimeOverrides — Phase 1 で CLI フラグを集約
```

## 実装順序（再確認）

```text
Phase 0 CLI probe  ∥  Step 5  ∥  Step 6
        ↓                ↓         ↓
     Step 7 (resolver + maps)
        ↓
     Step 8 (entry)
        ↓
     Step 9 (reachability)
        ↓
     Step 10 ∥ Step 11
        ↓
     Step 12 → Phase 1 CLI → Step 13
```

## Phase 1.5（v0.1 リリースブロッカー）

| プラン | 得点 | 判定 | ブロッカー |
| --- | ---: | --- | --- |
| phase-1.5-fp-remediation | 96 | ✅ 合格 | なし |

OSS 検証で YOK002 FP 100% が判明したが、4 workstream 実装後に
`make oss-metrics ARGS=--gate` が合格（0/0）。詳細は
[`phase-1.5-fp-remediation.md`](./phase-1.5-fp-remediation.md) と
[`oss-validation-report.md`](../oss-validation-report.md)。

## 未設計（スコープ外）

- v0.2: baseline, SARIF, cache, workspace member 境界
- v1.0: plugin API, stable JSON schema, LSP
