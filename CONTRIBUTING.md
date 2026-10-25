# Contributing to chokkin

## Prerequisites

- Rust 1.96+ (MSRV — see `rust-version` in `Cargo.toml`; CI also tests latest stable)
- [uv](https://docs.astral.sh/uv/) (for `uvx maturin` wheel builds)

Install development tools:

```bash
make tools
```

## Development workflow

### Before every push

```bash
make check
```

This runs the full pre-commit gate in order:

1. `cargo fmt --check` — formatting
2. `cargo clippy --all-targets --locked -- -D warnings` + `cargo doc` — linting
3. `ruff check .` and `shellcheck scripts/*.sh` — Python helper and shell
   script linting (`make lint-scripts`; pinned versions run through `uvx`)
4. `cargo test --locked` — unit and integration tests
5. `cargo deny check` — license / advisory / supply-chain policy
6. `cargo machete` — unused dependency detection

### Individual targets

```bash
make build     # release build
make test      # tests only
make lint      # clippy + doc
make lint-scripts  # ruff + shellcheck
make fmt       # auto-format
make coverage  # HTML coverage report (no threshold yet — see below)
make wheel     # build a local maturin wheel
make sdist     # build source distribution
make oss-fixtures  # run chokkin on OSS/regression fixture manifest (Phase 1 §17)
make deny      # cargo-deny (license / advisory / supply-chain policy)
make machete   # unused dependency detection
```

### Coverage threshold

`--fail-under` is intentionally absent from the coverage target until the
analyzer implementation reaches meaningful coverage. It will be set to 95%
at the Phase 1 (v0.1 MVP) milestone. See `docs/dev/ci-porting-notes.md`.

## Code conventions

- All new logic goes in `src/lib.rs` (and submodules). `src/main.rs` is
  argument dispatch and process exit code only.
- No `unsafe` code. No `unwrap`/`expect`/`panic` outside `#[cfg(test)]`.
- Lint exemptions are `#[expect(lint, reason = "...")]`; `#[allow]` and a
  missing reason are denied. An expectation that stops firing fails clippy,
  so delete it rather than keep it around.
- Use `std::path` APIs — chokkin ships cross-platform wheels.
- Never execute the analyzed project's Python code (static analysis only).

See [docs/dev/spec.ja.md](./docs/dev/spec.ja.md) for the full architecture and [AGENTS.md](./AGENTS.md) for coding rules.

## Pull requests

- One logical change per PR.
- Commit messages use imperative mood with a type prefix:
  `feat:`, `fix:`, `refactor:`, `chore:`, `docs:`, `ci:`.
- Include a test plan in the PR body.
- PRs are squash-merged.

## Release flow

Releases are driven by git tags. The release workflow builds prebuilt wheels
for all platforms and publishes to PyPI via Trusted Publishing:

1. Update `version` in `Cargo.toml` and `pyproject.toml`, and the `version` input default in `action.yml`.
2. Run `cargo generate-lockfile` to update `Cargo.lock`.
3. Move the `[Unreleased]` entries of `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD`.
4. Commit: `chore: release vX.Y.Z`.
5. Tag the squash-merged release commit itself, not the tip of `main`:
   `git tag vX.Y.Z <release-commit> && git push origin vX.Y.Z`. Anything
   merged after it ships in the wheels without a CHANGELOG entry or release
   validation (v0.7.4 was tagged at the tip and shipped #765–#772 and #774
   this way).
6. GitHub Actions checks the tag (it must be `v` plus the `Cargo.toml` and
   `pyproject.toml` version, on the release commit itself, on `main`), builds
   wheels, tests them (each wheel installed and run on its platform, the sdist
   built from source on the MSRV toolchain, and the test suite with the `dist`
   profile on `RUST_TOOLCHAIN`), creates a GitHub Release, and publishes to
   PyPI. A failing check or test publishes nothing; the tests also run on
   every pull request, and `make wheel-smoke` / `make sdist-smoke` run the
   wheel and sdist tests locally. The GitHub Release and the PyPI upload each wait for
   approval in the `pypi-chokkin` environment, so approve twice.
7. Once the tag is pushed, move `RELEASE_REV` in
   `.github/workflows/oss-metrics.yml` to `vX.Y.Z` in a `ci:` PR, so the
   run-time criteria compare with the new release (#1088). A slowdown that a
   PR makes on purpose is written in `CHANGELOG.md` and moves `RELEASE_REV`
   to that PR's merge commit instead.

Wheels are built with the toolchain pinned by `RUST_TOOLCHAIN` in
`.github/workflows/release.yml`, not the latest stable (maturin, syft and the
Linux build containers are pinned there too, and bumped by hand); the CI lint job
(rustfmt, clippy, rustdoc) pins the same version in `.github/workflows/ci.yml`
so a new clippy lint does not break open PRs the day it ships. To move it, bump
both in their own `ci:` PR (the release workflow builds and tests every wheel
on pull requests) before tagging, not in the release commit.

Before publishing the first release, a PyPI Trusted Publisher must be
registered at pypi.org (repo: `watany-dev/chokkin`, workflow: `release.yml`,
environment: `pypi-chokkin`). See `docs/dev/ci-porting-notes.md` for the full setup
checklist, and its "Repository settings checklist" for the branch ruleset,
environment reviewers, immutable releases, and PyPI account settings.
