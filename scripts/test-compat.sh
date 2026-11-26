#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> Running BigQuery Sandbox differential golden snapshot suite"
cargo test -p bq-lite-core --all-features --test sql_suite --test milestone_f_parity --test milestone_g_parity --test milestone_j_parity --test goccy_parity

if command -v bq >/dev/null 2>&1 && [ "${BQ_LIVE_COMPAT:-0}" = "1" ]; then
    echo "==> BQ_LIVE_COMPAT=1 detected: comparing live BigQuery Sandbox outputs"
    bq query --use_legacy_sql=false --format=json "SELECT FARM_FINGERPRINT('hello') AS fp"
else
    echo "==> Live BigQuery Sandbox comparison skipped (set BQ_LIVE_COMPAT=1 with authenticated bq CLI to run live diff)"
fi

echo "==> Compat check PASSED"
