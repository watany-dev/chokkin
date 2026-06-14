# Step 13: Optional Fix 設計

解析パイプライン §6 の **処理ステップ 13 (optional fix)** の実装設計。
`--fix` フラグ時に **manifest の安全な自動編集**を行う。コード・ファイル削除は明示フラグ必須。

> **関連プラン**
>
> - [`step-12-issue-emission.md`](./step-12-issue-emission.md) — fix 対象 issue
> - [`step-10-dependency-reconciliation.md`](./step-10-dependency-reconciliation.md) — CHK002/005/009

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | 明確な unused / duplicate / misplaced dependency を **manifest から削除・移動**する |
| 成果物 | `apply_fixes(...) -> FixReport` |
| Phase 0 / 1 との関係 | v0.1 — dependency 削除のみ（§16） |
| 後続 | v1.0 で safe autofix contract 凍結 |

## 2. スコープ

### In scope（v0.1 `--fix`）

| 対象 issue | 操作 |
| --- | --- |
| CHK002（confidence = **Certain** のみ） | `[project.dependencies]` 等から削除 |
| CHK009 | 重複宣言の一方を削除（dev 側優先保持は設定なし — 辞書順で低優先 context を削除） |
| CHK005（明確な case のみ） | dev group → runtime への **移動**（逆は手動） |

**編集対象ファイル:**

- `pyproject.toml` — `toml_edit`
- `requirements*.txt` — line-based（完全一致行削除）
- `setup.cfg` — 限定セクション

### Out of scope（§13）

| 操作 | フラグ / 時期 |
| --- | --- |
| 未使用ファイル削除 | `--allow-remove-files` — v0.1 非実装でも可（設計のみ） |
| missing dep 追加 | `--add-missing` — 一意解決時のみ将来 |
| 関数・class 削除 | `--fix --unsafe` — v1 まで禁止 |
| lockfile 編集 | 禁止 — fix 後に `uv lock` 促進メッセージ |
| hash-pinned requirements | 自動編集禁止 |

## 3. 仕様との対応

### 3.1 安全契約

```text
1. fix 前に IssueReport のコピーを入力とする（再解析は fix 後にユーザーが実行）
2. 各編集は単一 issue に 1:1 対応（バッチで複数可）
3. 曖昧な CHK002（Likely のみ）は --fix 対象外（Certain のみ）
4. 編集失敗は当該 issue を skip し FixDiagnostic を記録
```

### 3.2 `toml_edit` 方針

- comment / 空行を可能な限り保持
- 配列末尾削除 — trailing comma 正規化
- `dependency-groups` のネストテーブル対応

### 3.3 requirements 編集

```text
パッケージ名行のみ削除（PEP 508 正規化後にマッチ）
-r / -c 行は触らない
ハッシュオプション行はスキップ（FixDiagnostic::SkippedPinned）
```

### 3.4 `FixReport`

```rust
pub struct FixReport {
    pub applied: Vec<AppliedFix>,
    pub skipped: Vec<SkippedFix>,
    pub reminders: Vec<String>,   // "Run `uv lock` to refresh uv.lock"
}

pub struct AppliedFix {
    pub rule: RuleId,
    pub subject: IssueSubject,
    pub file: String,
    pub description: String,
}
```

## 4. モジュール構成

```
src/
  fix/
    mod.rs
    types.rs
    error.rs
    apply.rs          # apply_fixes
    pyproject.rs      # toml_edit
    requirements.rs
    setup_cfg.rs
    plan.rs           # issue → 編集操作の計画
```

## 5. API

```rust
pub struct FixOptions {
    pub allow_remove_files: bool,  // v0.1: 常に false 拒否
    pub add_missing: bool,         // v0.1: 未実装
}

pub fn apply_fixes(
    issues: &IssueReport,
    root: &ProjectRoot,
    manifest: &LoadedManifest,
    options: &FixOptions,
) -> Result<FixReport, FixError>;
```

**dry-run:** `--fix --dry-run` は Phase 1 CLI。Step 13 は `FixReport` に `dry_run: bool` を渡し、disk 書き込みをスキップ。

## 6. 依存

| Crate | 用途 |
| --- | --- |
| `toml_edit` | pyproject 編集 |

`cargo deny` 通過後に追加。

## 7. Exit criteria

- [ ] CHK002 Certain が pyproject から削除される
- [ ] requirements line 削除
- [ ] lockfile は変更しない + reminder 出力
- [ ] `--allow-remove-files` なしでファイル削除しない
- [ ] `make check` 通過

## 8. 未決事項

| 項目 | 理由 | 再検討 |
| --- | --- | --- |
| CHK005 移動の自動化 | 曖昧 case 多い | v0.1 は明確 case のみ |
| `setup.py` fix | 静的限界 | v0.2 |

## 9. update-plan 検証サマリ（確定）

### Phase 1: コンテキスト収集

| 成果物 | 確認結果 |
| --- | --- |
| `step-13-optional-fix.md` | 本プラン |
| `docs/dev/spec.ja.md` §13, §16 | safe fix 境界と一致 |
| `step-12` | `IssueReport` 入力 |
| `Cargo.toml` | `toml_edit` 未追加 — 本 PR で deny 確認後 |

### Phase 2: 品質評価（100点満点）

| カテゴリ | 配点 | 得点 | 所見 |
| --- | ---: | ---: | --- |
| モジュール / struct 設計 | 20 | 19 | `fix/` 単独。manifest 編集を分離 |
| 静的解析制約 | 20 | 20 | 対象 project コードは編集しない |
| ルール / ポリシー | 20 | 19 | Certain のみ自動削除。lock 非編集 |
| エラー処理 | 20 | 19 | `Result<FixReport, FixError>`。skip は diagnostic |
| テスト容易性 | 20 | 18 | tempdir + golden manifest |
| **合計** | **100** | **95** | **合格**（90 以上） |

### Phase 3: 整合性チェック

| チェック項目 | 結果 |
| --- | --- |
| §13 禁止事項 | OK |
| CHK002 scope vs 安全契約 | OK — P0 矛盾を修正（Certain のみ） |

### Phase 4: 改善反映（課題分類）

| 優先度 | 課題 | 対応 |
| --- | --- | --- |
| **P0** | §2 と §3.1 で CHK002 fix 条件が矛盾 | Certain のみに統一済み |

### 確定判定

**合格 — 実装着手可。** Step 12 + Phase 1 CLI `--fix` フラグ後。
