#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> Building release perf_bench"
cargo build --release -p bq-lite-core --example perf_bench

BIN="target/release/examples/perf_bench"

# Format: workload max_peak_mib max_elapsed_ms
BUDGETS=(
  "scan 95 150"
  "window 25 60"
  "groupby 45 80"
  "cte_corr 40 60"
  "join_ineq 12 30"
)

FAILED=0
printf "\n%-12s %12s %12s %12s %12s %s\n" "WORKLOAD" "ELAPSED_MS" "MAX_MS" "PEAK_MIB" "MAX_MIB" "STATUS"
printf "%-12s %12s %12s %12s %12s %s\n" "--------" "----------" "------" "--------" "-------" "------"

for entry in "${BUDGETS[@]}"; do
  read -r workload max_mib max_ms <<< "$entry"
  if [[ "$(uname -s)" == "Darwin" ]]; then
    out="$(/usr/bin/time -l "$BIN" "$workload" 2>&1)"
    peak_bytes="$(printf '%s\n' "$out" | awk '/peak memory footprint/{print $1; exit}')"
    if [[ -z "$peak_bytes" ]]; then
      peak_bytes="$(printf '%s\n' "$out" | awk '/maximum resident set size/{print $1; exit}')"
    fi
    peak_mib="$(awk -v b="$peak_bytes" 'BEGIN{printf "%.1f", b / 1048576.0}')"
  else
    out="$(/usr/bin/time -v "$BIN" "$workload" 2>&1)"
    peak_kb="$(printf '%s\n' "$out" | awk -F: '/Maximum resident set size/{gsub(/ /,"",$2); print $2; exit}')"
    peak_mib="$(awk -v kb="$peak_kb" 'BEGIN{printf "%.1f", kb / 1024.0}')"
  fi

  elapsed_ms="$(printf '%s\n' "$out" | sed -n 's/.*elapsed_ms=\([0-9.]*\).*/\1/p' | head -n 1)"
  ok="$(awk -v em="$elapsed_ms" -v mm="$max_ms" -v pm="$peak_mib" -v mb="$max_mib" 'BEGIN{print (em+0 <= mm+0 && pm+0 <= mb+0) ? "PASS" : "FAIL"}')"
  printf "%-12s %12s %12s %12s %12s %s\n" "$workload" "$elapsed_ms" "$max_ms" "$peak_mib" "$max_mib" "$ok"
  if [[ "$ok" != "PASS" ]]; then
    FAILED=1
  fi
done

if [[ "$FAILED" -ne 0 ]]; then
  echo ""
  echo "ERROR: Performance or memory budget exceeded!"
  exit 1
fi
echo ""
echo "==> Performance & memory regression check PASSED"
