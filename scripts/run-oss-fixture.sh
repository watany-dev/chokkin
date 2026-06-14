#!/usr/bin/env bash
# Run chokkin against OSS / regression fixture paths (Phase 1 dogfooding skeleton).
#
# Usage:
#   scripts/run-oss-fixture.sh [OPTIONS]
#
# Options:
#   -m, --manifest PATH   Fixture list (default: scripts/oss-fixtures.manifest)
#   -o, --output DIR      Report directory (default: target/oss-fixtures)
#   -b, --bin PATH        chokkin binary (default: target/release/chokkin)
#   --build               cargo build --release before running
#   -h, --help            Show help
#
# Each fixture produces:
#   <output>/<slug>.json   — chokkin --reporter json --no-exit-code
#   <output>/<slug>.meta   — exit code, duration, issue count
#   summary.tsv            — aggregate row per fixture
#
# Exit 0 when all fixtures complete without chokkin internal errors (exit 3).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${OSS_FIXTURES_MANIFEST:-$ROOT/scripts/oss-fixtures.manifest}"
REPORT_DIR="${OSS_FIXTURES_REPORT_DIR:-$ROOT/target/oss-fixtures}"
CHOKKIN_BIN="${CHOKKIN_BIN:-$ROOT/target/release/chokkin}"
DO_BUILD=0

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    -o | --output)
      REPORT_DIR="$2"
      shift 2
      ;;
    -b | --bin)
      CHOKKIN_BIN="$2"
      shift 2
      ;;
    --build)
      DO_BUILD=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$DO_BUILD" -eq 1 ]]; then
  (cd "$ROOT" && cargo build --release --locked --bin chokkin)
fi

if [[ ! -x "$CHOKKIN_BIN" ]]; then
  echo "chokkin binary not found or not executable: $CHOKKIN_BIN" >&2
  echo "Run with --build or set CHOKKIN_BIN." >&2
  exit 2
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "manifest not found: $MANIFEST" >&2
  exit 2
fi

mkdir -p "$REPORT_DIR"

SUMMARY="$REPORT_DIR/summary.tsv"
printf 'fixture\texit_code\tduration_ms\tissues\tchokkin_version\n' >"$SUMMARY"

failures=0
ran=0

slugify() {
  local path="$1"
  path="${path//\//-}"
  path="${path#-}"
  echo "${path:-root}"
}

issue_count_from_json() {
  local file="$1"
  if command -v jq >/dev/null 2>&1; then
    jq -r '.summary.total // 0' "$file" 2>/dev/null || echo 0
  else
    # Fallback: count "code" fields (rough; sufficient for skeleton reports).
    grep -c '"code"' "$file" 2>/dev/null || echo 0
  fi
}

while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line%%#*}"
  line="$(echo "$line" | xargs)"
  [[ -z "$line" ]] && continue

  fixture_path="$line"
  if [[ "$fixture_path" != /* ]]; then
    fixture_path="$ROOT/$fixture_path"
  fi

  if [[ ! -d "$fixture_path" ]]; then
    echo "skip (missing): $line" >&2
    continue
  fi

  slug="$(slugify "$line")"
  json_out="$REPORT_DIR/$slug.json"
  meta_out="$REPORT_DIR/$slug.meta"

  echo "==> $line"
  start_ms="$(date +%s%3N)"
  set +e
  "$CHOKKIN_BIN" --reporter json --no-exit-code "$fixture_path" >"$json_out" 2>"$REPORT_DIR/$slug.stderr"
  exit_code=$?
  set -e
  end_ms="$(date +%s%3N)"
  duration_ms=$((end_ms - start_ms))

  version="$("$CHOKKIN_BIN" --version 2>/dev/null | awk '{print $2}')"
  issues="$(issue_count_from_json "$json_out")"

  {
    echo "fixture=$line"
    echo "exit_code=$exit_code"
    echo "duration_ms=$duration_ms"
    echo "issues=$issues"
    echo "chokkin_version=$version"
  } >"$meta_out"

  printf '%s\t%s\t%s\t%s\t%s\n' "$line" "$exit_code" "$duration_ms" "$issues" "$version" >>"$SUMMARY"

  ran=$((ran + 1))
  if [[ "$exit_code" -eq 3 ]]; then
    echo "internal error on $line (see $REPORT_DIR/$slug.stderr)" >&2
    failures=$((failures + 1))
  fi
done <"$MANIFEST"

echo ""
echo "Ran $ran fixture(s). Summary: $SUMMARY"

if [[ "$ran" -eq 0 ]]; then
  echo "no fixtures ran — add paths to $MANIFEST" >&2
  exit 2
fi

if [[ "$failures" -gt 0 ]]; then
  exit 1
fi

exit 0
