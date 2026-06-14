#!/usr/bin/env bash
# Measure the Phase 1 §17 exit criteria over the OSS validation set.
#
# Exit criteria (docs/dev/spec.ja.md §17):
#   1. unused-dependency (YOK002) false-positive rate < 5%
#   2. crashes (yokei internal error, exit 3) == 0
#   3. cold run on a `medium` project <= 2000 ms
#
# Usage:
#   scripts/oss-metrics.sh [OPTIONS]
#
# Options:
#   -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
#   -l, --labels PATH     Ground-truth labels (default: scripts/oss-fixtures.labels.tsv)
#   -c, --clones DIR      Clone root (default: target/oss-clones)
#   -o, --output DIR      Report directory (default: target/oss-metrics)
#   -b, --bin PATH        yokei binary (default: target/release/yokei)
#   -r, --runs N          Timed repetitions per project, median reported (default: 3)
#   --build               cargo build --release before running
#   --clone               Run clone-oss-fixtures.sh first
#   --gate                Exit non-zero if any §17 criterion fails
#   -h, --help            Show help
#
# Outputs (under --output):
#   <slug>.json     raw yokei JSON report
#   findings.tsv    every YOK002/YOK003 finding with its ground-truth verdict
#   summary.tsv     per-project: size, exit, median_ms, totals, by-code counts
#   report.md       human-readable §17 scorecard
#
# False-positive accounting: each reported YOK002/YOK003 finding is matched
# against the labels file on (slug, code, distribution). Verdict `fp` counts as
# a false positive; `tp` as a true positive; anything unlabeled is `unknown`.
# The FP-rate gate cannot pass while unknown findings remain — every finding
# must be classified.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${OSS_CLONES_MANIFEST:-$ROOT/scripts/oss-clones.manifest}"
LABELS="${OSS_LABELS:-$ROOT/scripts/oss-fixtures.labels.tsv}"
CLONES="${OSS_CLONES_DIR:-$ROOT/target/oss-clones}"
OUTPUT="${OSS_METRICS_DIR:-$ROOT/target/oss-metrics}"
YOKEI_BIN="${YOKEI_BIN:-$ROOT/target/release/yokei}"
RUNS=3
DO_BUILD=0
DO_CLONE=0
DO_GATE=0
MEDIUM_GATE_MS=2000
FP_GATE_PCT=5

usage() { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --manifest) MANIFEST="$2"; shift 2 ;;
    -l | --labels) LABELS="$2"; shift 2 ;;
    -c | --clones) CLONES="$2"; shift 2 ;;
    -o | --output) OUTPUT="$2"; shift 2 ;;
    -b | --bin) YOKEI_BIN="$2"; shift 2 ;;
    -r | --runs) RUNS="$2"; shift 2 ;;
    --build) DO_BUILD=1; shift ;;
    --clone) DO_CLONE=1; shift ;;
    --gate) DO_GATE=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

if [[ "$DO_BUILD" -eq 1 ]]; then
  (cd "$ROOT" && cargo build --release --locked --bin yokei) || exit 2
fi
if [[ "$DO_CLONE" -eq 1 ]]; then
  "$ROOT/scripts/clone-oss-fixtures.sh" -m "$MANIFEST" -o "$CLONES" || \
    echo "warning: some clones failed; continuing with what is present" >&2
fi

[[ -x "$YOKEI_BIN" ]] || { echo "yokei binary not found: $YOKEI_BIN (use --build)" >&2; exit 2; }
[[ -f "$MANIFEST" ]] || { echo "manifest not found: $MANIFEST" >&2; exit 2; }

mkdir -p "$OUTPUT"
SUMMARY="$OUTPUT/summary.tsv"
FINDINGS="$OUTPUT/findings.tsv"
REPORT="$OUTPUT/report.md"
printf 'slug\tcategory\tsize\texit\tmedian_ms\ttotal\tYOK002\tYOK003\n' >"$SUMMARY"
printf 'slug\tcode\tdistribution\tverdict\tconfidence\tmessage\n' >"$FINDINGS"

VERSION="$("$YOKEI_BIN" --version 2>/dev/null | awk '{print $2}')"

# Look up a ground-truth verdict for (slug, code, distribution).
label_for() {
  local slug="$1" code="$2" dist="$3"
  [[ -f "$LABELS" ]] || { echo unknown; return; }
  awk -F'\t' -v s="$slug" -v c="$code" -v d="$dist" '
    /^#/ || NF < 4 { next }
    $1 == s && $2 == c && $3 == d { print $4; found=1; exit }
    END { if (!found) print "unknown" }
  ' "$LABELS"
}

median_of() {
  # Median of whitespace-separated integers.
  tr ' ' '\n' <<<"$1" | sort -n | awk '{a[NR]=$1} END {
    if (NR == 0) { print 0; exit }
    m = int((NR + 1) / 2)
    if (NR % 2) print a[m]; else printf "%d\n", (a[m] + a[m+1]) / 2
  }'
}

ran=0
crashes=0
medium_slow=()

while IFS=$'\t' read -r slug category size ref url || [[ -n "$slug" ]]; do
  slug="${slug%%#*}"
  [[ -z "${slug// /}" ]] && continue
  proj="$CLONES/$slug"
  if [[ ! -d "$proj" ]]; then
    echo "skip (not cloned): $slug" >&2
    continue
  fi

  echo "==> $slug ($category/$size)"
  json_out="$OUTPUT/$slug.json"
  times=""
  exit_code=0
  for ((i = 0; i < RUNS; i++)); do
    start_ms="$(date +%s%3N)"
    "$YOKEI_BIN" --reporter json --no-exit-code "$proj" >"$json_out" 2>"$OUTPUT/$slug.stderr"
    exit_code=$?
    end_ms="$(date +%s%3N)"
    times+=" $((end_ms - start_ms))"
  done
  median_ms="$(median_of "$times")"

  total=0; y002=0; y003=0
  if jq -e . "$json_out" >/dev/null 2>&1; then
    total="$(jq -r '.summary.total // 0' "$json_out")"
    y002="$(jq -r '[.issues[]? | select(.code=="YOK002")] | length' "$json_out")"
    y003="$(jq -r '[.issues[]? | select(.code=="YOK003")] | length' "$json_out")"

    # Emit each YOK002/YOK003 finding with its ground-truth verdict.
    while IFS=$'\t' read -r code dist conf msg; do
      [[ -z "$code" ]] && continue
      verdict="$(label_for "$slug" "$code" "$dist")"
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$slug" "$code" "$dist" "$verdict" "$conf" "$msg" >>"$FINDINGS"
    done < <(jq -r '.issues[]? | select(.code=="YOK002" or .code=="YOK003")
                    | [.code, (.distribution // "?"), (.confidence // "?"), (.message // "")] | @tsv' "$json_out")
  else
    echo "  non-JSON output (see $OUTPUT/$slug.stderr)" >&2
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$slug" "$category" "$size" "$exit_code" "$median_ms" "$total" "$y002" "$y003" >>"$SUMMARY"

  ran=$((ran + 1))
  [[ "$exit_code" -eq 3 ]] && crashes=$((crashes + 1))
  if [[ "$size" == "medium" && "$median_ms" -gt "$MEDIUM_GATE_MS" ]]; then
    medium_slow+=("$slug=${median_ms}ms")
  fi
done <"$MANIFEST"

if [[ "$ran" -eq 0 ]]; then
  echo "no projects measured — run clone-oss-fixtures.sh first" >&2
  exit 2
fi

# ── Aggregate false-positive accounting over YOK002 (gate) and YOK003 (info) ──
fp_count() { awk -F'\t' -v c="$1" -v v="$2" 'NR>1 && $2==c && $4==v {n++} END{print n+0}' "$FINDINGS"; }
y002_total="$(awk -F'\t' 'NR>1 && $2=="YOK002"{n++} END{print n+0}' "$FINDINGS")"
y002_fp="$(fp_count YOK002 fp)"
y002_tp="$(fp_count YOK002 tp)"
y002_unknown="$(fp_count YOK002 unknown)"
y003_total="$(awk -F'\t' 'NR>1 && $2=="YOK003"{n++} END{print n+0}' "$FINDINGS")"
y003_fp="$(fp_count YOK003 fp)"
y003_unknown="$(fp_count YOK003 unknown)"

fp_rate="n/a"
if [[ "$y002_total" -gt 0 ]]; then
  fp_rate="$(awk -v f="$y002_fp" -v t="$y002_total" 'BEGIN{printf "%.1f", 100*f/t}')"
fi

# ── Gate evaluation ──
pass_fp=1; pass_crash=1; pass_speed=1
[[ "$y002_unknown" -gt 0 ]] && pass_fp=0
if [[ "$y002_total" -gt 0 ]]; then
  awk -v f="$y002_fp" -v t="$y002_total" -v g="$FP_GATE_PCT" 'BEGIN{exit !(100*f/t < g)}' || pass_fp=0
fi
[[ "$crashes" -ne 0 ]] && pass_crash=0
[[ "${#medium_slow[@]}" -ne 0 ]] && pass_speed=0

verdict() { [[ "$1" -eq 1 ]] && echo "✅ PASS" || echo "❌ FAIL"; }

# ── Markdown scorecard ──
{
  echo "# OSS validation — Phase 1 §17 scorecard"
  echo ""
  echo "- yokei version: \`$VERSION\`"
  echo "- projects measured: $ran"
  echo "- generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "- timed runs per project (median): $RUNS"
  echo ""
  echo "## Exit criteria"
  echo ""
  echo "| Criterion | Target | Measured | Result |"
  echo "|---|---|---|---|"
  echo "| Unused-dep FP rate (YOK002) | < ${FP_GATE_PCT}% | ${fp_rate}% (${y002_fp} FP / ${y002_total} reported${y002_unknown:+, ${y002_unknown} unclassified}) | $(verdict "$pass_fp") |"
  echo "| Crashes (exit 3) | 0 | ${crashes} | $(verdict "$pass_crash") |"
  if [[ "${#medium_slow[@]}" -eq 0 ]]; then
    echo "| Cold run, medium project | <= ${MEDIUM_GATE_MS} ms | all within budget | $(verdict "$pass_speed") |"
  else
    echo "| Cold run, medium project | <= ${MEDIUM_GATE_MS} ms | over: ${medium_slow[*]} | $(verdict "$pass_speed") |"
  fi
  echo ""
  echo "## Per-project results"
  echo ""
  echo "| Project | Category | Size | Exit | Median ms | Issues | YOK002 | YOK003 |"
  echo "|---|---|---|---|---|---|---|---|"
  awk -F'\t' 'NR>1 {printf "| %s | %s | %s | %s | %s | %s | %s | %s |\n",$1,$2,$3,$4,$5,$6,$7,$8}' "$SUMMARY"
  echo ""
  echo "## YOK002 / YOK003 findings"
  echo ""
  if [[ "$y002_total" -eq 0 && "$y003_total" -eq 0 ]]; then
    echo "_No unused- or missing-dependency findings across the set._"
  else
    echo "| Project | Code | Distribution | Verdict | Confidence | Message |"
    echo "|---|---|---|---|---|---|"
    awk -F'\t' 'NR>1 {printf "| %s | %s | %s | %s | %s | %s |\n",$1,$2,$3,$4,$5,$6}' "$FINDINGS"
  fi
  echo ""
  echo "## Notes"
  echo ""
  echo "- FP rate denominator is reported YOK002 findings (user-facing precision: when yokei says \"remove this\", how often is it wrong)."
  echo "- YOK003 (missing dependency): ${y003_total} reported (${y003_fp} FP, ${y003_unknown} unclassified) — informational, not a §17 gate."
  echo "- Large-size projects are reported but excluded from the medium cold-run gate."
} >"$REPORT"

echo ""
echo "Summary : $SUMMARY"
echo "Findings: $FINDINGS"
echo "Report  : $REPORT"
echo ""
sed -n '/## Exit criteria/,/## Per-project/p' "$REPORT" | sed '$d'

if [[ "$DO_GATE" -eq 1 ]]; then
  if [[ "$pass_fp" -eq 1 && "$pass_crash" -eq 1 && "$pass_speed" -eq 1 ]]; then
    exit 0
  fi
  echo "§17 gate FAILED" >&2
  exit 1
fi
exit 0
