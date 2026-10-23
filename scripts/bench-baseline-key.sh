#!/usr/bin/env bash
# Print the cache key of a commit's criterion baseline (bench gate, #836).
#
# Usage:
#   scripts/bench-baseline-key.sh REV
#
# The key hashes the trees that can move a benchmark and the active rustc, so
# bench-baseline.yml (saving main's baseline) and the bench-gate job in
# verification.yml (looking up its base's) agree on it. A docs-only commit
# keeps the key of the commit before it.
set -euo pipefail

rev=${1:?usage: bench-baseline-key.sh REV}
hash=$(
  {
    git rev-parse "$rev:src" "$rev:benches" "$rev:Cargo.toml" "$rev:Cargo.lock" "$rev:Makefile"
    rustc -V
  } | sha256sum | cut -c1-16
)
echo "bench-base-$hash"
