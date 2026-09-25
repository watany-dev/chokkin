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
#   findings.tsv    every CHK001–CHK010 finding with ground-truth verdict
#   summary.tsv     per-project: size, exit, median_ms, totals, by-code counts
#   report.md       human-readable §17 scorecard + per-rule label coverage
#
# False-positive accounting: each reported CHK002 finding is matched against the
# labels file on (slug, code, target). Verdict `fp` counts as a false positive;
# `tp` as a true positive; `deferred` and unlabeled findings are unclassified.
# The FP-rate gate cannot pass while CHK002 unclassified findings remain.
#
# Recall accounting: the FP rate alone is satisfied by reporting nothing, so a
# separate recall gate measures in-repo sentinel fixtures (--recall manifest)
# whose deliberately-unused dependencies are labelled `tp`. Every `tp` label
# must appear in the run's findings or the recall gate fails.

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

ALL_RULES=(CHK001 CHK002 CHK003 CHK004 CHK005 CHK006 CHK007 CHK008 CHK009 CHK010)

usage() { sed -n '2,42p' "$0" | sed 's/^# \{0,1\}//'; }

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
printf 'slug\tcategory\tsize\texit\tmedian_ms\ttotal\tCHK002\tCHK003\n' >"$SUMMARY"
printf 'slug\tcode\ttarget\tverdict\tbucket\tconfidence\tmessage\n' >"$FINDINGS"

VERSION="$("$CHOKKIN_BIN" --version 2>/dev/null | awk '{print $2}')"

# Look up ground-truth verdict and bucket for (slug, code, target).
label_lookup() {
  local slug="$1" code="$2" target="$3"
  [[ -f "$LABELS" ]] || { echo -e "unknown\t-"; return; }
  awk -F'\t' -v s="$slug" -v c="$code" -v t="$target" '
    /^#/ || NF < 5 { next }
    $1 == s && $2 == c && $3 == t {
      bucket = ($5 == "" ? "-" : $5)
      print $4 "\t" bucket
      found = 1
      exit
    }
    END { if (!found) print "unknown\t-" }
  ' "$LABELS"
}

median_of() {
  tr ' ' '\n' <<<"$1" | sort -n | awk '{a[NR]=$1} END {
    if (NR == 0) { print 0; exit }
    m = int((NR + 1) / 2)
    if (NR % 2) print a[m]; else printf "%d\n", (a[m] + a[m+1]) / 2
  }'
}

ran=0
crashes=0
medium_slow=()

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

  local total=0 y002=0 y003=0
  if jq -e . "$json_out" >/dev/null 2>&1; then
    total="$(jq -r '.summary.total // 0' "$json_out")"
    y002="$(jq -r '[.issues[]? | select(.code=="CHK002")] | length' "$json_out")"
    y003="$(jq -r '[.issues[]? | select(.code=="CHK003")] | length' "$json_out")"

    local code target conf msg lookup verdict bucket
    while IFS=$'\t' read -r code target conf msg; do
      [[ -z "$code" ]] && continue
      lookup="$(label_lookup "$slug" "$code" "$target")"
      verdict="${lookup%%$'\t'*}"
      bucket="${lookup#*$'\t'}"
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$slug" "$code" "$target" "$verdict" "$bucket" "$conf" "$msg" >>"$FINDINGS"
    done < <(jq -r '.issues[]?
                    | [.code, (.target // "?"), (.confidence // "?"), (.message // "")] | @tsv' "$json_out")
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

verdict_count() {
  awk -F'\t' -v c="$1" -v v="$2" 'NR>1 && $2==c && $4==v {n++} END{print n+0}' "$FINDINGS"
}

rule_reported() {
  awk -F'\t' -v c="$1" 'NR>1 && $2==c {n++} END{print n+0}' "$FINDINGS"
}

y002_total="$(rule_reported CHK002)"
y002_fp="$(verdict_count CHK002 fp)"
y002_unclassified="$(($(verdict_count CHK002 unknown) + $(verdict_count CHK002 deferred)))"

y003_total="$(rule_reported CHK003)"
y003_fp="$(verdict_count CHK003 fp)"
y003_tp="$(verdict_count CHK003 tp)"
y003_deferred="$(verdict_count CHK003 deferred)"
y003_unknown="$(verdict_count CHK003 unknown)"

fp_rate="n/a"
if [[ "$y002_total" -gt 0 ]]; then
  fp_rate="$(awk -v f="$y002_fp" -v t="$y002_total" 'BEGIN{printf "%.1f", 100*f/t}')"
fi

tp_total=0; tp_missed=0; missed=()
if [[ -f "$LABELS" ]]; then
  while IFS=$'\t' read -r lslug lcode ltarget; do
    [[ -z "$lslug" ]] && continue
    tp_total=$((tp_total + 1))
    awk -F'\t' -v s="$lslug" -v c="$lcode" -v t="$ltarget" \
      'NR>1 && $1==s && $2==c && $3==t {found=1} END{exit !found}' "$FINDINGS" ||
      { tp_missed=$((tp_missed + 1)); missed+=("$lslug/$lcode/$ltarget"); }
  done < <(awk -F'\t' '/^#/ || NF<5 {next} $4=="tp" {print $1"\t"$2"\t"$3}' "$LABELS")
fi
tp_detected=$((tp_total - tp_missed))

pass_fp=1; pass_crash=1; pass_speed=1; pass_recall=1
[[ "$y002_unclassified" -gt 0 ]] && pass_fp=0
if [[ "$y002_total" -gt 0 ]]; then
  awk -v f="$y002_fp" -v t="$y002_total" -v g="$FP_GATE_PCT" 'BEGIN{exit !(100*f/t < g)}' || pass_fp=0
fi
[[ "$crashes" -ne 0 ]] && pass_crash=0
[[ "${#medium_slow[@]}" -ne 0 ]] && pass_speed=0
[[ "$tp_missed" -ne 0 ]] && pass_recall=0

verdict() { [[ "$1" -eq 1 ]] && echo "✅ PASS" || echo "❌ FAIL"; }

coverage_pct() {
  local code="$1"
  awk -F'\t' -v c="$code" '
    NR > 1 && $2 == c { reported++ }
    NR > 1 && $2 == c && ($4 == "tp" || $4 == "fp") { classified++ }
    END {
      if (reported == 0) print "n/a"
      else printf "%.1f", 100 * classified / reported
    }
  ' "$FINDINGS"
}

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
  echo "| Unused-dep FP rate (CHK002) | < ${FP_GATE_PCT}% | ${fp_rate}% (${y002_fp} FP / ${y002_total} reported${y002_unclassified:+, ${y002_unclassified} unclassified}) | $(verdict "$pass_fp") |"
  if [[ "$tp_missed" -eq 0 ]]; then
    echo "| Recall (\`tp\` labels) | all detected | ${tp_detected}/${tp_total} detected | $(verdict "$pass_recall") |"
  else
    echo "| Recall (\`tp\` labels) | all detected | ${tp_detected}/${tp_total} detected (missed: ${missed[*]}) | $(verdict "$pass_recall") |"
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
  echo "Coverage % = (tp + fp) / reported. Deferred triage is not ground truth."
  echo "Reported=0 means the rule"
  echo "emitted nothing on this corpus — precision and recall are both unverified."
  echo ""
  echo "| Rule | Reported | tp | fp | deferred | unknown | Coverage % |"
  echo "|---|---:|---:|---:|---:|---:|---:|"
  for code in "${ALL_RULES[@]}"; do
    rep="$(rule_reported "$code")"
    tp="$(verdict_count "$code" tp)"
    fp="$(verdict_count "$code" fp)"
    def="$(verdict_count "$code" deferred)"
    unk="$(verdict_count "$code" unknown)"
    cov="$(coverage_pct "$code")"
    echo "| $code | $rep | $tp | $fp | $def | $unk | $cov |"
  done
  echo ""
  echo "## CHK003 root-cause buckets"
  echo ""
  bucket_rows="$(awk -F'\t' '
    NR > 1 && $2 == "CHK003" && $5 != "-" && $5 != "" {
      bucket[$5]++
      if (examples[$5] == "" || length(examples[$5]) < 120) {
        examples[$5] = examples[$5] " " $1 "/" $3
      }
    }
    END {
      for (b in bucket) print b "\t" bucket[b] "\t" examples[b]
    }
  ' "$FINDINGS" | sort -t$'\t' -k2,2nr)"
  if [[ -z "$bucket_rows" ]]; then
    echo "_No CHK003 findings with a classified bucket on this run._"
  else
    echo "| Bucket | Count | Examples (slug/target) |"
    echo "|---|---:|---|"
    while IFS=$'\t' read -r bucket count examples; do
      echo "| $bucket | $count | ${examples# } |"
    done <<<"$bucket_rows"
  fi
  echo ""
  echo "## Per-project results"
  echo ""
  echo "| Project | Category | Size | Exit | Median ms | Issues | CHK002 | CHK003 |"
  echo "|---|---|---|---|---|---|---|---|"
  awk -F'\t' 'NR>1 {printf "| %s | %s | %s | %s | %s | %s | %s | %s |\n",$1,$2,$3,$4,$5,$6,$7,$8}' "$SUMMARY"
  echo ""
  echo "## Findings (CHK001–CHK010)"
  echo ""
  finding_rows="$(awk 'END {print NR - 1}' "$FINDINGS")"
  if [[ "$finding_rows" -eq 0 ]]; then
    echo "_No findings across the set._"
  else
    echo "| Project | Code | Target | Verdict | Bucket | Confidence | Message |"
    echo "|---|---|---|---|---|---|---|"
    awk -F'\t' 'NR>1 {printf "| %s | %s | %s | %s | %s | %s | %s |\n",$1,$2,$3,$4,$5,$6,$7}' "$FINDINGS"
  fi
  echo ""
  echo "## Notes"
  echo ""
  echo "- FP rate denominator is reported CHK002 findings (user-facing precision)."
  echo "- CHK002 unclassified = unknown + deferred; both block the §17 FP gate."
  echo "- Recall gate counts every \`tp\` label (all rules, incl. sentinels)."
  echo "- CHK003 (missing dependency): ${y003_total} reported (${y003_fp} FP, ${y003_tp} tp, ${y003_deferred} deferred, ${y003_unknown} unknown) — informational, not a §17 gate."
  echo "- Large-size projects are reported but excluded from the medium cold-run gate."
} >"$REPORT"

echo ""
echo "Summary : $SUMMARY"
echo "Findings: $FINDINGS"
echo "Report  : $REPORT"
echo ""
sed -n '/## Exit criteria/,/## Per-project/p' "$REPORT" | sed '$d'

if [[ "$DO_GATE" -eq 1 ]]; then
  if [[ "$pass_fp" -eq 1 && "$pass_crash" -eq 1 && "$pass_speed" -eq 1 && "$pass_recall" -eq 1 ]]; then
    exit 0
  fi
  echo "§17 gate FAILED" >&2
  exit 1
fi
exit 0
