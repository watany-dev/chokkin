# Tool versions — keep in sync with .github/workflows/ci.yml
CARGO_DENY_VERSION          ?= 0.19.2
CARGO_TARPAULIN_VERSION     ?= 0.35.1
CARGO_SEMVER_CHECKS_VERSION ?= 0.47.0

.PHONY: check build test lint fmt fmt-check doc deny audit machete coverage semver wheel sdist tools bench bench-save bench-cmp oss-fixtures help

## ─── Pre-commit gate ──────────────────────────────────────────────────────────
check: fmt-check lint test deny machete

## ─── Core ─────────────────────────────────────────────────────────────────────
build:
	cargo build --release --locked

test:
	cargo test --locked

lint:
	cargo clippy --all-targets --locked -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked

fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked

## ─── Benchmarks ───────────────────────────────────────────────────────────────
# bench-save/bench-cmp: pass BASELINE=<name> (e.g. make bench-save BASELINE=main)
bench:
	cargo bench --benches --locked

bench-save:
	cargo bench --benches --locked -- --save-baseline $(BASELINE)

bench-cmp:
	cargo bench --benches --locked -- --baseline $(BASELINE)

## ─── OSS dogfooding (Phase 1 §17) ─────────────────────────────────────────────
oss-fixtures:
	scripts/run-oss-fixture.sh --build

## ─── Security & supply chain ──────────────────────────────────────────────────
deny:
	cargo deny check advisories licenses bans sources

audit:
	cargo audit --deny warnings

machete:
	cargo machete

## ─── Code coverage ────────────────────────────────────────────────────────────
# NOTE: --fail-under is intentionally omitted until the analyzer is implemented.
# Re-enable at 95% once Phase 1 (v0.1 MVP) coverage is established.
# See docs/dev/ci-porting-notes.md.
coverage:
	cargo tarpaulin --out html --skip-clean --timeout 300 -- --test-threads=1

## ─── Semver ───────────────────────────────────────────────────────────────────
semver:
	cargo semver-checks --baseline-rev origin/main

## ─── Python / maturin distribution ───────────────────────────────────────────
wheel:
	uvx maturin build --release

sdist:
	uvx maturin sdist

## ─── Tool installation ────────────────────────────────────────────────────────
tools:
ifndef SKIP_TOOL_INSTALL
	cargo install cargo-deny@$(CARGO_DENY_VERSION) --locked
	cargo install cargo-tarpaulin@$(CARGO_TARPAULIN_VERSION) --locked
	cargo install cargo-semver-checks@$(CARGO_SEMVER_CHECKS_VERSION) --locked
endif

help:
	@grep -E '^## ' Makefile | sed 's/^## //'
	@echo ""
	@grep -E '^[a-zA-Z_-]+:' Makefile | grep -v '^help:' | awk -F: '{print "  " $$1}'
