# CI porting notes

This document tracks items ported from `watany-dev/ptuf` that are intentionally
deferred or require manual setup before they become fully effective.

## SHA pinning for third-party actions

The following actions in `release.yml` are currently referenced by semver tag,
not commit SHA. Pin them to a SHA before the first production release and remove
the corresponding `zizmor.yml` suppressions.

| Action | Current ref | Why deferred |
|--------|-------------|--------------|
| `PyO3/maturin-action` | `@v1` | SHA to be confirmed on first release run |
| `pypa/gh-action-pypi-publish` | `@release/v1` | SHA to be confirmed on first release run |

## PyPI Trusted Publishing setup

Before the first `v*` tag push, register a Trusted Publisher on pypi.org:

1. Go to https://pypi.org/manage/account/publishing/ (or the project page once
   reserved).
2. Add a publisher with:
   - **Repository owner:** `watany-dev`
   - **Repository name:** `chokkin`
   - **Workflow filename:** `release.yml`
   - **Environment name:** `pypi`
3. Create the `pypi` environment in the GitHub repository settings
   (Settings → Environments → New environment → name it `pypi`).

The workflow uses `id-token: write` + `pypa/gh-action-pypi-publish` with no
API token — authentication is handled entirely by OIDC.

## PyPI package name reservation

The package name `chokkin` on PyPI has not been reserved. Consider publishing an
empty placeholder wheel early to prevent name squatting.

## Coverage threshold

`cargo tarpaulin --fail-under 95` is intentionally omitted from both the
Makefile and `ci.yml` until the Phase 1 (v0.1 MVP) analyzer implementation
provides meaningful coverage. Re-enable when Phase 1 is merged:

```yaml
# ci.yml coverage job — uncomment when Phase 1 lands:
# --fail-under 95
```

```makefile
# Makefile coverage target — uncomment when Phase 1 lands:
# --fail-under 95
```

## Windows test temp directory

`cargo nextest` on `windows-latest` can hit `PermissionDenied` when many
parallel tests create directories under the default short-path user temp
(`RUNNER~1\AppData\Local\Temp`). The `test` job redirects `TMP` / `TEMP` /
`TMPDIR` to `$RUNNER_TEMP/chokkin-test-tmp` before running nextest.

## Deferred CI jobs (require code to exist)

These jobs from ptuf's `nightly.yml` are omitted until the corresponding code
is implemented:

| Job | Reason |
|-----|--------|
| `fuzz` | No parser/engine code to fuzz yet |
| `mutants` | No security-critical decision core yet |
| `e2e` | No end-to-end behavior to test yet |
| `pbt-deep` | No property-based tests yet |

Add `nightly.yml` with these jobs during Phase 1 implementation.

## cargo-mutants scope

When mutation testing is added, scope it to the analysis engine (analogous to
ptuf's decision core). Suggested initial `examine_globs` for `.cargo/mutants.toml`:

```toml
examine_globs = [
    "src/graph/**/*.rs",
    "src/rules/**/*.rs",
    "src/resolver/**/*.rs",
]

additional_cargo_args = []
```

## crates.io

chokkin is distributed via PyPI only (maturin `bin` bindings). Publishing to
crates.io is not planned. The `publish-crates.yml` workflow from ptuf is omitted.

## stop-wrapup hook

ptuf's `.claude/hooks/stop-wrapup.sh` depends on ptuf's own wrapup skill
(`/simplify`, `/update-docs`). It is not ported here because those skills are
ptuf-specific. Add a project-specific stop hook when a similar wrapup workflow
is established for chokkin.
