# Contributing to yokei

## Prerequisites

- Rust 1.93+ (MSRV — see `rust-version` in `Cargo.toml`; CI also tests latest stable)
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
3. `cargo test --locked` — unit and integration tests
4. `cargo deny check` — license / advisory / supply-chain policy
5. `cargo machete` — unused dependency detection

### Individual targets

```bash
make build     # release build
make test      # tests only
make lint      # clippy + doc
make fmt       # auto-format
make coverage  # HTML coverage report (no threshold yet — see below)
make semver    # public API compatibility vs. origin/main
make wheel     # build a local maturin wheel
make sdist     # build source distribution
make oss-fixtures  # run yokei on OSS/regression fixture manifest (Phase 1 §17)
make deny      # cargo-deny (license / advisory / supply-chain policy)
make machete   # unused dependency detection
make audit     # cargo-audit (run locally if you updated deps)
```

### Coverage threshold

`--fail-under` is intentionally absent from the coverage target until the
analyzer implementation reaches meaningful coverage. It will be set to 95%
at the Phase 1 (v0.1 MVP) milestone. See `docs/dev/ci-porting-notes.md`.

## Code conventions

- All new logic goes in `src/lib.rs` (and submodules). `src/main.rs` is
  argument dispatch and process exit code only.
- No `unsafe` code. No `unwrap`/`expect`/`panic` outside `#[cfg(test)]`.
- Use `std::path` APIs — yokei ships cross-platform wheels.
- Never execute the analyzed project's Python code (static analysis only).

See [AGENTS.md](./AGENTS.md) for the full architecture overview.

## Pull requests

- One logical change per PR.
- Commit messages use imperative mood with a type prefix:
  `feat:`, `fix:`, `refactor:`, `chore:`, `docs:`, `ci:`.
- Include a test plan in the PR body.
- PRs are squash-merged.

## Release flow

Releases are driven by git tags. The release workflow builds prebuilt wheels
for all platforms and publishes to PyPI via Trusted Publishing:

1. Update `version` in `Cargo.toml` and `pyproject.toml`.
2. Run `cargo generate-lockfile` to update `Cargo.lock`.
3. Commit: `chore: release vX.Y.Z`.
4. Tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
5. GitHub Actions builds wheels, creates a GitHub Release, and publishes to PyPI.

Before publishing the first release, a PyPI Trusted Publisher must be
registered at pypi.org (repo: `watany-dev/yokei`, workflow: `release.yml`,
environment: `pypi`). See `docs/dev/ci-porting-notes.md` for the full setup
checklist.
