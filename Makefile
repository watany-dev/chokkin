# Tool versions — keep in sync with .github/workflows/ci.yml
CARGO_DENY_VERSION          ?= 0.19.2
CARGO_LLVM_COV_VERSION      ?= 0.9.1
CARGO_MUTANTS_VERSION       ?= 27.1.0

.PHONY: check build test lint fmt fmt-check doc deny machete coverage wheel sdist tools bench bench-save bench-cmp oss-fixtures oss-clones oss-metrics oss-gate oss-envs oss-oracle oss-diff oss-mutation bench-gate check-generated formal mutants mutants-diff kani help

## ─── Pre-commit gate ──────────────────────────────────────────────────────────
check: fmt-check lint test deny machete

## ─── Core ─────────────────────────────────────────────────────────────────────
build:
	cargo build --release --locked

test:
	cargo test --locked

lint: doc
	cargo clippy --all-targets --locked -- -D warnings

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

# bench-gate: fail on a >10% mean slowdown vs BASELINE (default main) whose 95%
#             CI excludes 0 (#342). Save the baseline on the base commit first.
bench-gate:
	scripts/bench-gate.py --baseline $(or $(BASELINE),main) $(ARGS)

## ─── OSS dogfooding (Phase 1 §17) ─────────────────────────────────────────────
# oss-fixtures: in-repo regression skeleton (always available, no network).
# oss-clones:   clone the 20-project §17 validation set into target/oss-clones/.
# oss-metrics:  measure FP rate / crashes / cold-run speed vs §17 exit criteria.
#               Pass ARGS=--gate to fail when any criterion misses.
oss-fixtures:
	scripts/run-oss-fixture.sh --build

oss-clones:
	scripts/clone-oss-fixtures.sh

oss-metrics:
	scripts/oss-metrics.py --build $(ARGS)

# oss-gate:   determinism (cold/warm/--no-cache byte-identical), JSON schema
#             and crash gates over the corpus + recall sentinels (#342).
oss-gate:
	scripts/oss-gate.py --build $(ARGS)

# oss-envs:   per-project test venvs in target/oss-envs/ (#338). BUILDS
#             UNTRUSTED PROJECTS and needs PyPI — throwaway runner only.
oss-envs:
	scripts/oss-provision-envs.py $(ARGS)

# oss-oracle: remove-and-test oracle, CHK001 by default, ARGS="--rule CHK006"
#             for symbols (#338/#339). RUNS UNTRUSTED PROJECT TESTS in
#             disposable copies of target/oss-clones/ with the oss-envs venvs,
#             network-isolated — opt-in, local or isolated runner only, never
#             release/default CI. See docs/dev/chk001-remove-and-test.md.
oss-oracle:
	scripts/oss-remove-and-test.py --build --envs target/oss-envs --offline --execute $(ARGS)

# oss-diff:   differential oracle vs vulture/deadcode/deptry/fawltydeps/ruff/
#             pyflakes (#340); the tools run via uvx and only read the code.
oss-diff:
	scripts/oss-differential.py --build $(ARGS)

# oss-mutation: inject known dead code / dependency issues into copies of the
#             clones and measure recall per rule (#341). Executes nothing.
oss-mutation:
	scripts/oss-mutation-recall.py --build $(ARGS)

## ─── Security & supply chain ──────────────────────────────────────────────────
deny:
	cargo deny check advisories licenses bans sources

machete:
	cargo machete

## ─── Generated artifacts ───────────────────────────────────────────────────────
check-generated:
	python3 tests/test_harvest_package_map.py
	python3 scripts/generate-package-map.py
	python3 scripts/generate-stdlib-modules.py
	git diff --exit-code src/resolver/bundled/ src/resolver/stdlib/

## ─── Formal models (docs/dev/formal) ──────────────────────────────────────────
# Requires python3 with z3-solver (`pip install z3-solver`). Runs every model and
# fails if any property has a counterexample. TLA+ (TLC) is run separately; see
# docs/dev/formal/README.md.
formal:
	@status=0; for m in relative_import_model deps_rules_z3 exit_status_z3 ignore_model; do \
		python3 docs/dev/formal/$$m.py || status=1; \
	done; exit $$status

## ─── Test effectiveness (#418) ────────────────────────────────────────────────
# mutants:      cargo-mutants over src/rules/ and src/resolver/ (~70 min at -j 2
#               on 4 vCPU; most of it is builds). Narrow with ARGS, e.g.
#               ARGS="-f src/rules/emit.rs". Results: mutants.out/.
# mutants-diff: only the mutants on lines changed since BASE (default origin/main).
# kani:         #[cfg(kani)] proof harnesses; needs `cargo install --locked
#               kani-verifier@0.68.0 && cargo kani setup`.
#               Not part of `make check`. See docs/dev/formal/README.md.
MUTANTS_ENV = CARGO_PROFILE_DEV_DEBUG=0
BASE       ?= origin/main

mutants:
	$(MUTANTS_ENV) cargo mutants -j 2 -f 'src/rules/**' -f 'src/resolver/**' $(ARGS)

mutants-diff:
	mkdir -p target
	git diff $(BASE)... > target/mutants.diff
	$(MUTANTS_ENV) cargo mutants -j 2 --in-diff target/mutants.diff $(ARGS)

kani:
	cargo kani

## ─── Code coverage ────────────────────────────────────────────────────────────
# NOTE: --fail-under-lines is intentionally omitted until the analyzer is implemented.
# Re-enable at 95% once Phase 1 (v0.1 MVP) coverage is established.
# See docs/dev/ci-porting-notes.md.
coverage:
	cargo llvm-cov --locked --html

## ─── Python / maturin distribution ───────────────────────────────────────────
wheel:
	uvx maturin build --release

sdist:
	uvx maturin sdist

## ─── Tool installation ────────────────────────────────────────────────────────
tools:
ifndef SKIP_TOOL_INSTALL
	cargo install cargo-deny@$(CARGO_DENY_VERSION) --locked
	cargo install cargo-llvm-cov@$(CARGO_LLVM_COV_VERSION) --locked
	cargo install cargo-mutants@$(CARGO_MUTANTS_VERSION) --locked
endif

help:
	@grep -E '^## ' Makefile | sed 's/^## //'
	@echo ""
	@grep -E '^[a-zA-Z_-]+:' Makefile | grep -v '^help:' | awk -F: '{print "  " $$1}'
