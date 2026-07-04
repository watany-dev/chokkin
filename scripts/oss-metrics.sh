#!/usr/bin/env bash
# Measure the Phase 1 §17 exit criteria over the OSS validation set.
#
# Exit criteria (docs/dev/spec.ja.md §17):
#   1. unused-dependency (CHK002) false-positive rate < 5%
#   2. crashes (chokkin internal error, exit 3) == 0
#   3. cold run on a `medium` project <= 2000 ms
#
# Usage:
#   scripts/oss-metrics.sh [OPTIONS]
#
# Options:
#   -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
#   -l, --labels PATH     Ground-truth labels (default: scripts/oss-fixtures.labels.tsv)
#   -R, --recall PATH     Recall sentinels (default: scripts/oss-recall.manifest)
#   -c, --clones DIR      Clone root (default: target/oss-clones)
#   -o, --output DIR      Report directory (default: target/oss-metrics)
#   -b, --bin PATH        chokkin binary (default: target/release/chokkin)
#   -r, --runs N          Timed repetitions per project, median reported (default: 3)
#   --build               cargo build --release before running
#   --clone               Run clone-oss-fixtures.sh first
#   --gate                Exit non-zero if any §17 criterion fails
#   -h, --help            Show help
#
# Outputs (under --output):
#   <slug>.json     raw chokkin JSON report
#   findings.tsv    every finding (all CHK rules) with its ground-truth verdict
#   summary.tsv     per-project: size, exit, median_ms, totals, by-code counts
#   coverage.md     per-rule label-coverage table (tp/fp/unknown)
#   report.md       human-readable §17 scorecard
#
# False-positive accounting: every reported finding is matched against the
# labels file on (slug, code, key). The key is the finding's stable subject:
# distribution name for CHK002/CHK005/CHK009, `file:module` (import site) for
# CHK003/CHK004/CHK010, file path for CHK001, `path:symbol` for CHK006/CHK007,
# and binary name for CHK008. Verdict `fp` counts as a false positive; `tp` as a
# true positive; anything unlabeled is `unknown`. The §17 FP-rate gate stays
# CHK002-only: it cannot pass while unknown CHK002 findings remain. Other
# rules' unknowns are surfaced as label-coverage gaps but never gate.
#
# Recall accounting: the FP rate alone is satisfied by reporting nothing, so a
# separate recall gate measures in-repo sentinel fixtures (--recall manifest)
# whose deliberately-unused dependencies are labelled `tp`. Every CHK002 `tp`
# label must appear in the run's findings or the recall gate fails — this is
# what stops the FP remediation from silently collapsing into "report nothing".

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${OSS_CLONES_MANIFEST:-$ROOT/scripts/oss-clones.manifest}"
LABELS="${OSS_LABELS:-$ROOT/scripts/oss-fixtures.labels.tsv}"
RECALL_MANIFEST="${OSS_RECALL_MANIFEST:-$ROOT/scripts/oss-recall.manifest}"
CLONES="${OSS_CLONES_DIR:-$ROOT/target/oss-clones}"
OUTPUT="${OSS_METRICS_DIR:-$ROOT/target/oss-metrics}"
CHOKKIN_BIN="${CHOKKIN_BIN:-$ROOT/target/release/chokkin}"
RUNS=3
DO_BUILD=0
DO_CLONE=0
DO_GATE=0
MEDIUM_GATE_MS=2000
FP_GATE_PCT=5

usage() { sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m | --manifest) MANIFEST="$2"; shift 2 ;;
    -l | --labels) LABELS="$2"; shift 2 ;;
    -R | --recall) RECALL_MANIFEST="$2"; shift 2 ;;
    -c | --clones) CLONES="$2"; shift 2 ;;
    -o | --output) OUTPUT="$2"; shift 2 ;;
    -b | --bin) CHOKKIN_BIN="$2"; shift 2 ;;
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
  (cd "$ROOT" && cargo build --release --locked --bin chokkin) || exit 2
fi
if [[ "$DO_CLONE" -eq 1 ]]; then
  "$ROOT/scripts/clone-oss-fixtures.sh" -m "$MANIFEST" -o "$CLONES" || \
    echo "warning: some clones failed; continuing with what is present" >&2
fi

[[ -x "$CHOKKIN_BIN" ]] || { echo "chokkin binary not found: $CHOKKIN_BIN (use --build)" >&2; exit 2; }
[[ -f "$MANIFEST" ]] || { echo "manifest not found: $MANIFEST" >&2; exit 2; }

mkdir -p "$OUTPUT"
SUMMARY="$OUTPUT/summary.tsv"
FINDINGS="$OUTPUT/findings.tsv"
REPORT="$OUTPUT/report.md"
ALL_CODES=(CHK001 CHK002 CHK003 CHK004 CHK005 CHK006 CHK007 CHK008 CHK009 CHK010)
printf 'slug\tcategory\tsize\texit\tmedian_ms\ttotal\t%s\n' "$(IFS=$'\t'; echo "${ALL_CODES[*]}")" >"$SUMMARY"
printf 'slug\tcode\tkey\tverdict\tconfidence\tmessage\n' >"$FINDINGS"

VERSION="$("$CHOKKIN_BIN" --version 2>/dev/null | awk '{print $2}')"

# Look up a ground-truth verdict for (slug, code, key).
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

# Measure one project tree: time it, record findings with verdicts, update the
# run-wide counters. Used for both OSS clones and in-repo recall sentinels.
measure_one() {
  local slug="$1" category="$2" size="$3" proj="$4"
  echo "==> $slug ($category/$size)"
  local json_out="$OUTPUT/$slug.json"
  local times="" exit_code=0 start_ms end_ms i
  for ((i = 0; i < RUNS; i++)); do
    start_ms="$(date +%s%3N)"
    "$CHOKKIN_BIN" --reporter json --no-exit-code "$proj" >"$json_out" 2>"$OUTPUT/$slug.stderr"
    exit_code=$?
    end_ms="$(date +%s%3N)"
    times+=" $((end_ms - start_ms))"
  done
  local median_ms; median_ms="$(median_of "$times")"

  local total=0
  local code_counts=$'0\t0\t0\t0\t0\t0\t0\t0\t0\t0'
  if jq -e . "$json_out" >/dev/null 2>&1; then
    total="$(jq -r '.summary.total // 0' "$json_out")"
    code_counts="$(jq -r --argjson codes "$(printf '%s\n' "${ALL_CODES[@]}" | jq -R . | jq -s .)" '
      [.issues[]? | .code] as $seen
      | $codes | map(. as $c | ($seen | map(select(. == $c)) | length)) | @tsv' "$json_out")"

    # Emit every finding with its ground-truth verdict, keyed by the stable
    # subject: distribution name when the subject is a distribution, else the
    # reporter's stable `target` (path / path:symbol / binary / file:module).
    local code key conf msg verdict
    while IFS=$'\t' read -r code key conf msg; do
      [[ -z "$code" ]] && continue
      verdict="$(label_for "$slug" "$code" "$key")"
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$slug" "$code" "$key" "$verdict" "$conf" "$msg" >>"$FINDINGS"
    done < <(jq -r '.issues[]?
                    | [.code, (.distribution // .target // "?"), (.confidence // "?"), (.message // "")] | @tsv' "$json_out")
  else
    echo "  non-JSON output (see $OUTPUT/$slug.stderr)" >&2
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$slug" "$category" "$size" "$exit_code" "$median_ms" "$total" "$code_counts" >>"$SUMMARY"

  ran=$((ran + 1))
  [[ "$exit_code" -eq 3 ]] && crashes=$((crashes + 1))
  if [[ "$size" == "medium" && "$median_ms" -gt "$MEDIUM_GATE_MS" ]]; then
    medium_slow+=("$slug=${median_ms}ms")
  fi
}

while IFS=$'\t' read -r slug category size ref url || [[ -n "$slug" ]]; do
  slug="${slug%%#*}"
  [[ -z "${slug// /}" ]] && continue
  proj="$CLONES/$slug"
  if [[ ! -d "$proj" ]]; then
    echo "skip (not cloned): $slug" >&2
    continue
  fi
  measure_one "$slug" "$category" "$size" "$proj"
done <"$MANIFEST"

# Recall sentinels: in-repo fixtures with a known-unused dependency that chokkin
# must keep flagging. Measured alongside the clones (no network) so the recall
# gate has true positives to verify even when every real project is clean.
if [[ -f "$RECALL_MANIFEST" ]]; then
  while IFS=$'\t' read -r slug path || [[ -n "$slug" ]]; do
    slug="${slug%%#*}"
    [[ -z "${slug// /}" ]] && continue
    [[ "$path" != /* ]] && path="$ROOT/$path"
    if [[ ! -d "$path" ]]; then
      echo "skip (missing recall fixture): $slug" >&2
      continue
    fi
    measure_one "$slug" recall sentinel "$path"
  done <"$RECALL_MANIFEST"
fi

if [[ "$ran" -eq 0 ]]; then
  echo "no projects measured — run clone-oss-fixtures.sh first" >&2
  exit 2
fi

# ── Aggregate false-positive accounting over CHK002 (gate) and CHK003 (info) ──
fp_count() { awk -F'\t' -v c="$1" -v v="$2" 'NR>1 && $2==c && $4==v {n++} END{print n+0}' "$FINDINGS"; }
y002_total="$(awk -F'\t' 'NR>1 && $2=="CHK002"{n++} END{print n+0}' "$FINDINGS")"
y002_fp="$(fp_count CHK002 fp)"
y002_tp="$(fp_count CHK002 tp)"
y002_unknown="$(fp_count CHK002 unknown)"
y003_total="$(awk -F'\t' 'NR>1 && $2=="CHK003"{n++} END{print n+0}' "$FINDINGS")"
y003_fp="$(fp_count CHK003 fp)"
y003_unknown="$(fp_count CHK003 unknown)"

fp_rate="n/a"
if [[ "$y002_total" -gt 0 ]]; then
  fp_rate="$(awk -v f="$y002_fp" -v t="$y002_total" 'BEGIN{printf "%.1f", 100*f/t}')"
fi

# ── Per-rule label coverage (issue #85 WS1) ──
# Bucket every finding across the set into tp/fp/unknown per rule so the label
# blind spots are visible. Informational only — never part of the §17 gate.
COVERAGE="$OUTPUT/coverage.md"
{
  echo "| Rule | Findings | tp | fp | unknown | Label coverage | FP rate (labelled) |"
  echo "|---|---|---|---|---|---|---|"
  for code in "${ALL_CODES[@]}"; do
    c_total="$(awk -F'\t' -v c="$code" 'NR>1 && $2==c {n++} END{print n+0}' "$FINDINGS")"
    c_tp="$(fp_count "$code" tp)"
    c_fp="$(fp_count "$code" fp)"
    c_unknown="$(fp_count "$code" unknown)"
    c_labelled=$((c_tp + c_fp))
    c_cov="n/a"
    [[ "$c_total" -gt 0 ]] &&
      c_cov="$(awk -v l="$c_labelled" -v t="$c_total" 'BEGIN{printf "%.0f%%", 100*l/t}')"
    c_rate="n/a"
    [[ "$c_labelled" -gt 0 ]] &&
      c_rate="$(awk -v f="$c_fp" -v l="$c_labelled" 'BEGIN{printf "%.1f%%", 100*f/l}')"
    printf '| %s | %s | %s | %s | %s | %s | %s |\n' \
      "$code" "$c_total" "$c_tp" "$c_fp" "$c_unknown" "$c_cov" "$c_rate"
  done
} >"$COVERAGE"

# ── Recall accounting: every CHK002 `tp` label must actually be reported ──
# A `tp` label that is absent from findings is a false negative — chokkin stopped
# detecting a genuinely-unused dependency. This is the over-suppression guard.
# Deliberately CHK002-only: adding tp labels for other rules (per-rule coverage
# above) must not change the §17 gate criteria.
tp_total=0; tp_missed=0; missed=()
if [[ -f "$LABELS" ]]; then
  while IFS=$'\t' read -r lslug lcode ldist; do
    [[ -z "$lslug" ]] && continue
    tp_total=$((tp_total + 1))
    awk -F'\t' -v s="$lslug" -v c="$lcode" -v d="$ldist" \
      'NR>1 && $1==s && $2==c && $3==d {found=1} END{exit !found}' "$FINDINGS" ||
      { tp_missed=$((tp_missed + 1)); missed+=("$lslug/$lcode/$ldist"); }
  done < <(awk -F'\t' '/^#/ || NF<4 {next} $2=="CHK002" && $4=="tp" {print $1"\t"$2"\t"$3}' "$LABELS")
fi
tp_detected=$((tp_total - tp_missed))

# ── Gate evaluation ──
pass_fp=1; pass_crash=1; pass_speed=1; pass_recall=1
[[ "$y002_unknown" -gt 0 ]] && pass_fp=0
if [[ "$y002_total" -gt 0 ]]; then
  awk -v f="$y002_fp" -v t="$y002_total" -v g="$FP_GATE_PCT" 'BEGIN{exit !(100*f/t < g)}' || pass_fp=0
fi
[[ "$crashes" -ne 0 ]] && pass_crash=0
[[ "${#medium_slow[@]}" -ne 0 ]] && pass_speed=0
[[ "$tp_missed" -ne 0 ]] && pass_recall=0

verdict() { [[ "$1" -eq 1 ]] && echo "✅ PASS" || echo "❌ FAIL"; }

# ── Markdown scorecard ──
{
  echo "# OSS validation — Phase 1 §17 scorecard"
  echo ""
  echo "- chokkin version: \`$VERSION\`"
  echo "- projects measured: $ran"
  echo "- generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "- timed runs per project (median): $RUNS"
  echo ""
  echo "## Exit criteria"
  echo ""
  echo "| Criterion | Target | Measured | Result |"
  echo "|---|---|---|---|"
  echo "| Unused-dep FP rate (CHK002) | < ${FP_GATE_PCT}% | ${fp_rate}% (${y002_fp} FP / ${y002_total} reported${y002_unknown:+, ${y002_unknown} unclassified}) | $(verdict "$pass_fp") |"
  if [[ "$tp_missed" -eq 0 ]]; then
    echo "| Unused-dep recall (CHK002 tp) | all detected | ${tp_detected}/${tp_total} detected | $(verdict "$pass_recall") |"
  else
    echo "| Unused-dep recall (CHK002 tp) | all detected | ${tp_detected}/${tp_total} detected (missed: ${missed[*]}) | $(verdict "$pass_recall") |"
  fi
  echo "| Crashes (exit 3) | 0 | ${crashes} | $(verdict "$pass_crash") |"
  if [[ "${#medium_slow[@]}" -eq 0 ]]; then
    echo "| Cold run, medium project | <= ${MEDIUM_GATE_MS} ms | all within budget | $(verdict "$pass_speed") |"
  else
    echo "| Cold run, medium project | <= ${MEDIUM_GATE_MS} ms | over: ${medium_slow[*]} | $(verdict "$pass_speed") |"
  fi
  echo ""
  echo "## Per-rule label coverage"
  echo ""
  cat "$COVERAGE"
  echo ""
  echo "## Per-project results"
  echo ""
  awk -F'\t' 'NR==1 { printf "|"; for (i=1;i<=NF;i++) printf " %s |", $i; printf "\n|"
                      for (i=1;i<=NF;i++) printf "---|"; printf "\n"; next }
              { printf "|"; for (i=1;i<=NF;i++) printf " %s |", $i; printf "\n" }' "$SUMMARY"
  echo ""
  echo "## Findings"
  echo ""
  findings_total="$(awk 'NR>1 {n++} END{print n+0}' "$FINDINGS")"
  if [[ "$findings_total" -eq 0 ]]; then
    echo "_No findings across the set._"
  else
    echo "| Project | Code | Key | Verdict | Confidence | Message |"
    echo "|---|---|---|---|---|---|"
    awk -F'\t' 'NR>1 {printf "| %s | %s | %s | %s | %s | %s |\n",$1,$2,$3,$4,$5,$6}' "$FINDINGS"
  fi
  echo ""
  echo "## Notes"
  echo ""
  echo "- FP rate denominator is reported CHK002 findings (user-facing precision: when chokkin says \"remove this\", how often is it wrong)."
  echo "- Recall gate counts CHK002 \`tp\` labels (incl. in-repo sentinels) that failed to appear in findings — it fails the run if the FP remediation over-suppresses and stops detecting genuinely-unused dependencies."
  echo "- CHK003 (missing dependency): ${y003_total} reported (${y003_fp} FP, ${y003_unknown} unclassified) — informational, not a §17 gate."
  echo "- Per-rule label coverage is informational (issue #85 WS1): unknowns outside CHK002 surface blind spots but never gate."
  echo "- Findings are keyed by \`distribution\` when the subject is a distribution, else by the reporter's stable \`target\`."
  echo "- Large-size projects are reported but excluded from the medium cold-run gate."
} >"$REPORT"

echo ""
echo "Summary : $SUMMARY"
echo "Findings: $FINDINGS"
echo "Coverage: $COVERAGE"
echo "Report  : $REPORT"
echo ""
sed -n '/## Exit criteria/,/## Per-rule/p' "$REPORT" | sed '$d'
echo "Per-rule label coverage:"
echo ""
cat "$COVERAGE"

if [[ "$DO_GATE" -eq 1 ]]; then
  if [[ "$pass_fp" -eq 1 && "$pass_crash" -eq 1 && "$pass_speed" -eq 1 && "$pass_recall" -eq 1 ]]; then
    exit 0
  fi
  echo "§17 gate FAILED" >&2
  exit 1
fi
exit 0
