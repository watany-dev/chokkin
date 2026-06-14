# Phase 2: v0.2 導入支援 中期計画

v0.1.0 リリース後の中期計画。Phase 2 は新しい検出種類を増やすより、
既存プロジェクトが CI に入れやすくなることを優先する。判断基準は
`docs/dev/spec.ja.md` §16–§20 と、v0.1.0 の OSS 検証結果
(`docs/dev/oss-validation-report.md`)。

## 1. 目的

| 項目 | 内容 |
| --- | --- |
| 解決する問題 | v0.1 は単発実行には使えるが、大規模既存 project では既存 issue の扱い、CI 表示、monorepo、warm performance が導入障壁になる |
| 成果物 | baseline、SARIF/GitHub Actions reporter、workspace 解決、cache、plugin 拡充 |
| 期間目安 | 6–8 週。v0.1.x の運用修正と並行する |
| exit criteria | 10k files 級 monorepo で warm 2s 以内、baseline 運用で CI 導入事例 1 件以上、OSS gate 継続合格 |

## 2. リリース列

| リリース | 目的 | 主な内容 | リリース条件 |
| --- | --- | --- | --- |
| v0.1.x | リリース後安定化 | crash/FP 修正、bundled maps 更新、docs/CI 微修正 | `make check`、`make oss-metrics ARGS=--gate` |
| v0.2.0-alpha.1 | CI 導入の最小体験 | baseline + GitHub Actions markdown/SARIF draft | dogfood repo で新規 issue のみ fail できる |
| v0.2.0-alpha.2 | monorepo 体験 | uv workspace + member 境界 + cache draft | synthetic 10k files で warm 2s 以内の見込み |
| v0.2.0 | 導入支援版 | plugin 拡充、schema 文書化、migration notes (`docs/dev/schema-migration-notes.md`) | OSS gate 合格、CI 導入事例、破壊的変更なし |

v0.2.0 まで JSON schema は draft 扱いを維持する。stable schema は Phase 3 で凍結する。v0.2 時点の JSON reporter / baseline draft と migration 方針は `docs/dev/schema-migration-notes.md` に置く。

## 3. 優先順位

### P0: Baseline

既存 issue を凍結し、新規 issue だけで CI を落とす。大規模既存 project への導入では最初に必要。

CLI:

```text
chokkin --baseline chokkin-baseline.json --update-baseline
chokkin --baseline chokkin-baseline.json
chokkin --baseline chokkin-baseline.json --reporter json
```

設計:

```text
baseline key = rule_id + stable target id + normalized path + distribution/symbol
baseline stores:
  - chokkin_version
  - generated_at
  - project_root fingerprint inputs
  - issues[]
```

PR 分割:

1. `rules` に stable issue fingerprint を追加する — 初期実装済み (`src/baseline/` で `rule_id + stable target`)
2. baseline read/filter を Step 12 issue emission 後に挿入する — 初期実装済み
3. `--update-baseline` で atomic write する — 初期実装済み (`--baseline PATH` 必須、nested baseline parent 作成対応)
4. reporter に baseline summary を出す — 初期実装済み (`default`/`compact`/`json`/`markdown`/`github`)

検証:

- path separator の差分で fingerprint が変わらない
- line number だけの変動で既存 dependency/file issue が新規扱いにならない
- symbol issue は `path:symbol` を主 key にする
- baseline file は root containment を守る

### P0: GitHub Actions / SARIF Reporter

CI で読みやすい結果を出す。baseline と同じ adoption path の一部として扱う。

CLI:

```text
chokkin --reporter github
chokkin --reporter sarif
```

方針:

- GitHub Actions reporter は job summary と annotation を主対象にする
- SARIF は GitHub code scanning に読み込める最小 subset から始める
- SARIF rule metadata は CHK001–CHK010 の severity/confidence を明示する
- `--no-exit-code` と組み合わせても reporter は完全な issue list を出す

PR 分割:

1. reporter enum と CLI parse を拡張する — 初期実装済み (`github`, `sarif`)
2. GitHub markdown/job summary reporter を追加する — annotation reporter として初期実装済み
3. SARIF v2.1.0 の最小 schema serializer を追加する — 初期実装済み (`partialFingerprints["chokkin/v0"]` で baseline と同型の stable identity を出力)
4. fixtures で snapshot test を追加する — reporter render regression は初期実装済み (`tests/reporters_render.rs`; fileless GitHub annotation / path normalization / SARIF fingerprint を含む)

### P1: Workspace / Monorepo

uv workspace と明示 `[tool.chokkin.workspaces]` を、first-party 判定だけでなく依存境界まで使う。

対象:

```text
[tool.uv.workspace].members
[tool.chokkin.workspaces.<id>]
member pyproject.toml
root uv.lock
editable/local path dependencies
```

方針:

- workspace member ごとに manifest/source/entry/reachability を分ける
- root dependency と member dependency を区別する
- default は緩く、`--strict` で member ごとの直接依存宣言を要求する
- cross-member import は first-party edge として扱い、missing dependency にはしない

PR 分割:

1. workspace member discovery を Step 1/2 の後に追加する — 初期実装済み (`LoadedConfig.workspace_members`, `--probe` 表示)
2. member ごとの `LoadedManifest` / `SourceInventory` を保持する型を追加する — 初期実装済み (`ProbeReport.workspace_inputs`)
3. resolver に member boundary を渡す — 初期実装済み (`resolve_imports(..., workspace_members)`, `ResolvedImport.workspace_member`)
4. CHK003/CHK004/CHK005 の workspace policy を実装する — CHK003/CHK005 strict member-local policy は初期実装済み (`WorkspaceDependencyBoundary`)
5. reporters に member id を表示する — 初期実装済み (`Issue.workspace_member`, human/GitHub subject labels, JSON `workspace_member`, SARIF `properties.workspaceMember`)

検証:

- root 実行と member directory 実行で同じ root を選ぶ
- member A の dependency を member B が直接 import した場合、default と strict の差が出る
- editable local path dependency が third-party missing にならない

### P1: Cache / Performance

§19 の warm 目標を実現する。v0.2 では correctness を優先し、無効化可能な保守的 cache にする。

CLI:

```text
chokkin --no-cache
```

cache unit:

```text
source file parse result
requirements/pyproject extraction result
plugin config scan result
module index
```

cache key:

```text
chokkin version
config hash
manifest hash
source path + mtime + size + content hash fallback
target_version
plugin version
```

PR 分割:

1. cache directory policy と `--no-cache` — policy型とCLI plumbingは初期実装済み (`CacheOptions`, `.chokkin/cache`; custom directory も project root 配下に containment)
2. parse result cache — 初期実装済み (`ParseCacheStore`, `parse_project_sources_with_cache`, `.chokkin/cache/parse/*.json`; persisted entry は atomic replace)
3. manifest/config scan cache — input fingerprint/key/record envelope、JSON payload slot、disk backend、generic config scan payload wiring、full manifest extraction payload、module index payload は初期実装済み (`ScanInputFingerprints`, `ScanCacheKey`, `ScanCacheRecord`, `read_scan_payload`, `write_scan_payload`, `extract_manifest_with_cache`, `ModuleIndex::build_with_cache`; persisted entry は atomic replace)
4. warm benchmark fixture と `make bench` comparison — parse cache warm benchmark は初期実装済み (`benches/cache.rs`)

exit:

- small project warm < 300ms
- medium project warm < 1s
- synthetic 10k files warm < 2s
- stale cache による false negative を regression test で防ぐ
- recursive requirements include の変更で manifest payload cache が stale hit しない

### P2: Plugin 拡充

Phase 1.5 の generic config scanner を、ユーザーに説明しやすい plugin として整理する。

優先順:

1. tox / nox / pre-commit / github-actions — binary usage plugin 化は初期実装済み (`src/plugins/devtools.rs`); GitHub Actions は single-line `run:` と block scalar `run: |` / `run: >` command に対応
2. flask / celery — static app command/reference extraction is initial implementation (`FLASK_APP`, `flask --app`, `celery -A/--app`); Flask route decorator and Celery task decorator module refs are initial implementation
3. Sphinx / MkDocs / Alembic — conventional config/entry extraction is initial implementation (`docs/conf.py`, `mkdocs.yml`, `alembic/env.py`); Sphinx literal `extensions` module refs and MkDocs known theme/plugin used-distribution scan are initial implementation
4. notebook parsing — `.ipynb` discovery and Python code-cell extraction are initial implementation

方針:

- plugin は entry roots、module refs、symbol refs、binary usages の 4 種だけを出す
- Python code は実行しない
- YAML/TOML/INI と Python literal parse の範囲に限定する
- plugin ごとに false-positive regression fixture を追加する

## 4. v0.1.x 運用ループ

v0.2 実装中も、v0.1.x は信頼を落とさないために小さく刻む。

毎リリース前:

```text
make check
make oss-metrics ARGS=--gate
scripts/run-oss-fixture.sh --build
```

受け付ける変更:

- crash fix
- CHK002/CHK003 の明確な FP 修正
- bundled package-module-map / binary map 追加
- docs のリリース状態更新
- reporter の backward-compatible bug fix

避ける変更:

- issue JSON の破壊的変更
- default confidence/severity の大幅変更
- `--fix` の対象拡大
- Python code 実行を伴う解析

## 5. Phase 3 への引き渡し

v0.2 完了時点で、次を Phase 3 の契約安定化に渡す。

| 項目 | Phase 2 の状態 | Phase 3 の作業 |
| --- | --- | --- |
| JSON schema | draft | 破壊的変更なしで 2 minor 維持できる形に凍結 |
| SARIF | GitHub で読める subset | rule metadata と help URI を安定化 |
| baseline | 実運用可能、migration 方針は `docs/dev/schema-migration-notes.md` に文書化済み | schema version 明示と reader compatibility を安定化 |
| plugin | built-in only | plugin API RFC と外部 plugin 試作 |
| severity | rule 固定 | config override 設計 |

## 6. リスク

| リスク | 対応 |
| --- | --- |
| baseline が false positive を隠しすぎる | `--update-baseline` と通常実行を分け、summary に suppressed 件数を出す |
| cache が stale result を返す | 初期は保守的 key、`--no-cache`、content hash fallback |
| workspace が CHK003 を悪化させる | default は緩く、strict だけ境界違反を error にする |
| SARIF schema 実装が重い | serde 直列化の最小 subset から始める |
| plugin 拡充で scope creep する | P0/P1 完了まで P2 plugin は regression fixture 中心にする |

## 7. update-plan 自己評価

| カテゴリ | 点 | 評価 |
| --- | ---: | --- |
| 目的とスコープ | 19/20 | v0.2 の導入支援に絞れている |
| 既存設計との整合 | 18/20 | §16–§20 と一致。詳細型は実装前に各 PR で再確認する |
| 実装分割 | 18/20 | P0/P1/P2 と PR 境界を定義済み |
| 検証可能性 | 18/20 | baseline/workspace/cache の主要 regression を列挙済み |
| リスク管理 | 18/20 | adoption 機能特有の失敗モードを明記済み |

総合: **91/100 — 実装計画として採用可。**
