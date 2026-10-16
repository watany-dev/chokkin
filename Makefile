# Tool versions — keep in sync with .github/workflows/ci.yml
CARGO_DENY_VERSION          ?= 0.19.2
CARGO_LLVM_COV_VERSION      ?= 0.9.1
CARGO_MUTANTS_VERSION       ?= 27.1.0
CARGO_NEXTEST_VERSION       ?= 0.9.133

.PHONY: check build test lint fmt fmt-check doc deny machete coverage wheel wheel-smoke sdist sdist-smoke tools bench bench-save bench-cmp oss-fixtures oss-clones oss-metrics oss-gate oss-envs oss-oracle oss-diff oss-mutation bench-gate check-generated test-scripts pbt-deep formal tlc mutants mutants-diff kani help

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

## ─── Property-based tests (#861) ─────────────────────────────────────────────
# pbt-deep: every proptest block at PBT_CASES / 256 times its usual cases
#           (16x by default; blocks tuned with with_cases scale through
#           internals::proptest_cases). CI runs it nightly. A failure saves its
#           seed under proptest-regressions/ or tests/*.proptest-regressions:
#           commit the seed with the fix, and review it like a test.
PBT_CASES ?= 4096

pbt-deep:
	PROPTEST_CASES=$(PBT_CASES) cargo test --locked

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
#             ARGS=--in-repo runs the in-repo fixtures instead (no clones; CI, #835).
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
	python3 scripts/generate-package-map.py
	python3 scripts/generate-stdlib-modules.py
	git diff --exit-code src/resolver/bundled/ src/resolver/stdlib/

# Unit tests of scripts/ (tests/test_*.py), stdlib unittest only.
test-scripts:
	python3 -m unittest discover -s tests -p 'test_*.py'

## ─── Formal models (docs/dev/formal) ──────────────────────────────────────────
# formal: requires python3 with z3-solver (`pip install z3-solver`). Runs every
#         Python model and fails if any property has a counterexample.
# tlc:    model-checks Reachability.tla; pass TLA2TOOLS=<path to tla2tools.jar>
#         (a few seconds). CI runs both (.github/workflows/formal.yml); see
#         docs/dev/formal/README.md.
TLA2TOOLS ?= tla2tools.jar

formal:
	@status=0; for m in relative_import_model deps_rules_z3 exit_status_z3 ignore_model; do \
		python3 docs/dev/formal/$$m.py || status=1; \
	done; exit $$status

tlc:
	java -XX:+UseParallelGC -cp "$(TLA2TOOLS)" tlc2.TLC -deadlock -workers auto \
		-metadir target/tlc docs/dev/formal/MCReachability

## ─── Test effectiveness (#418) ────────────────────────────────────────────────
# mutants:      cargo-mutants over src/rules/, src/resolver/, src/fix/ and
#               src/graph/ (most of the time is builds). Tests run under
#               nextest (.cargo/mutants.toml), so `make tools` is needed.
#               To narrow to one file run cargo mutants directly with the same
#               env, e.g. `$(MUTANTS_ENV) cargo mutants -f src/rules/emit.rs`
#               (a -f in ARGS widens the set: file globs are OR'ed).
#               Results: mutants.out/. CI fails on any missed mutant (#837).
# mutants-diff: only the mutants on lines changed since BASE (default origin/main);
#               what the PR check runs.
# kani:         #[cfg(kani)] proof harnesses; needs `cargo install --locked
#               kani-verifier@0.68.0 && cargo kani setup`.
#               Not part of `make check`. See docs/dev/formal/README.md.
#
# cargo-mutants builds every mutant in a copy of the tree, so each build is a
# small edit on top of the baseline: incremental compilation turns the rebuild
# from a full one (~35 s) into a few seconds. Set explicitly because
# Swatinem/rust-cache exports CARGO_INCREMENTAL=0 into a job's environment.
# Debug info is dropped to keep each build and link short.
MUTANTS_ENV = CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1
BASE       ?= origin/main

mutants:
	$(MUTANTS_ENV) cargo mutants -j 2 -f 'src/rules/**' -f 'src/resolver/**' -f 'src/fix/**' -f 'src/graph/**' $(ARGS)

mutants-diff:
	mkdir -p target
	git diff $(BASE)... > target/mutants.diff
	$(MUTANTS_ENV) cargo mutants -j 2 --in-diff target/mutants.diff $(ARGS)

kani:
	cargo kani

## ─── Code coverage ────────────────────────────────────────────────────────────
# coverage: same 95% line gate as the CI coverage job (docs/dev/ci-porting-notes.md).
coverage:
	cargo llvm-cov --locked --html --fail-under-lines 95

## ─── Python / maturin distribution ───────────────────────────────────────────
wheel:
	uvx maturin build --release

# wheel-smoke: build the wheel the way release.yml does (dist profile) and run
#              scripts/wheel-smoke.py on it, as the release's test-wheels job does.
wheel-smoke:
	rm -rf target/wheel-smoke
	uvx maturin build --profile dist --locked --out target/wheel-smoke
	python3 scripts/wheel-smoke.py target/wheel-smoke/*.whl

# sdist-smoke: build the sdist and run scripts/wheel-smoke.py on it, as the
#              release's test-sdist job does (with the default toolchain, not MSRV).
sdist-smoke:
	rm -rf target/sdist-smoke
	uvx maturin sdist --out target/sdist-smoke
	python3 scripts/wheel-smoke.py target/sdist-smoke/*.tar.gz

sdist:
	uvx maturin sdist

## ─── Tool installation ────────────────────────────────────────────────────────
tools:
ifndef SKIP_TOOL_INSTALL
	cargo install cargo-deny@$(CARGO_DENY_VERSION) --locked
	cargo install cargo-llvm-cov@$(CARGO_LLVM_COV_VERSION) --locked
	cargo install cargo-mutants@$(CARGO_MUTANTS_VERSION) --locked
	cargo install cargo-nextest@$(CARGO_NEXTEST_VERSION) --locked
endif

help:
	@grep -E '^## ' Makefile | sed 's/^## //'
	@echo ""
	@grep -E '^[a-zA-Z_-]+:' Makefile | grep -v '^help:' | awk -F: '{print "  " $$1}'
