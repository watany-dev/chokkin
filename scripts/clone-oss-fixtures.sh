#!/usr/bin/env bash
# Clone the §17 validation OSS set into target/oss-clones/ (Phase 1 dogfooding).
#
# Usage:
#   scripts/clone-oss-fixtures.sh [OPTIONS]
#
# Options:
#   -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
#   -o, --output DIR      Clone root (default: target/oss-clones)
#   -j, --jobs N          Parallel clones (default: 4)
#   --force               Remove and re-clone existing checkouts
#   -h, --help            Show help
#
# Clones are shallow (--depth 1) at the manifest `ref` (or default branch when
# empty). The resolved commit SHA for every checkout is recorded in
# <output>/clones.lock.tsv so a validation run is reproducible regardless of
# upstream branch movement.
#
# Network is required. Clone failures are reported but do not abort the batch;
# the exit code is non-zero if any clone failed so CI can gate on it.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${OSS_CLONES_MANIFEST:-$ROOT/scripts/oss-clones.manifest}"
OUTPUT="${OSS_CLONES_DIR:-$ROOT/target/oss-clones}"
JOBS=4
FORCE=0

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --manifest) MANIFEST="$2"; shift 2 ;;
    -o | --output) OUTPUT="$2"; shift 2 ;;
    -j | --jobs) JOBS="$2"; shift 2 ;;
    --force) FORCE=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! -f "$MANIFEST" ]]; then
  echo "manifest not found: $MANIFEST" >&2
  exit 2
fi

mkdir -p "$OUTPUT"
LOCK="$OUTPUT/clones.lock.tsv"
: >"$LOCK.tmp"
export LOCK

clone_one() {
  local slug="$1" ref="$2" url="$3"
  local dest="$OUTPUT/$slug"

  if [[ "$FORCE" -eq 1 ]]; then
    rm -rf "$dest"
  fi

  if [[ -d "$dest/.git" ]]; then
    echo "have $slug (skip; --force to refresh)"
  else
    echo "clone $slug <- $url${ref:+ @ $ref}"
    local attempt=0 ok=0
    while [[ "$attempt" -lt 4 ]]; do
      if [[ -n "$ref" ]]; then
        git clone --quiet --depth 1 --branch "$ref" "$url.git" "$dest" 2>/dev/null && ok=1 && break
      else
        git clone --quiet --depth 1 "$url.git" "$dest" 2>/dev/null && ok=1 && break
      fi
      attempt=$((attempt + 1))
      sleep $((1 << attempt))
    done
    if [[ "$ok" -ne 1 ]]; then
      echo "FAILED $slug" >&2
      printf '%s\t%s\t%s\tCLONE_FAILED\n' "$slug" "$ref" "$url" >>"$LOCK.tmp"
      return 1
    fi
  fi

  local sha
  sha="$(git -C "$dest" rev-parse HEAD 2>/dev/null || echo UNKNOWN)"
  printf '%s\t%s\t%s\t%s\n' "$slug" "$ref" "$url" "$sha" >>"$LOCK.tmp"
}

export -f clone_one
export OUTPUT FORCE

# Build the worklist (slug, ref, url) from the manifest.
worklist="$(mktemp)"
while IFS=$'\t' read -r slug category size ref url || [[ -n "$slug" ]]; do
  slug="${slug%%#*}"
  [[ -z "${slug// /}" ]] && continue
  printf '%s\t%s\t%s\n' "$slug" "${ref:-}" "$url" >>"$worklist"
done <"$MANIFEST"

total="$(wc -l <"$worklist" | tr -d ' ')"
echo "Cloning $total project(s) into $OUTPUT (jobs=$JOBS)"

failures=0
if command -v xargs >/dev/null 2>&1 && xargs --version 2>/dev/null | grep -qi gnu; then
  # GNU xargs: run clones in parallel.
  if ! xargs -P "$JOBS" -L1 bash -c 'clone_one "$@"' _ <"$worklist"; then
    failures=1
  fi
else
  # Portable fallback: sequential.
  while IFS=$'\t' read -r slug ref url; do
    clone_one "$slug" "$ref" "$url" || failures=1
  done <"$worklist"
fi
rm -f "$worklist"

# Stable, deduplicated lock file.
{
  printf 'slug\tref\turl\tsha\n'
  sort -u "$LOCK.tmp"
} >"$LOCK"
rm -f "$LOCK.tmp"

echo ""
echo "Lock file: $LOCK"
if grep -q 'CLONE_FAILED' "$LOCK"; then
  echo "Some clones failed (see CLONE_FAILED rows in $LOCK)." >&2
  exit 1
fi
[[ "$failures" -eq 0 ]] || exit 1
exit 0
