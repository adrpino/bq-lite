#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CACHE_DIR="${BQ_LITE_SDK_CACHE:-/tmp/bq-lite-sdk-cache}"
mkdir -p "$CACHE_DIR"

SERVER_PID=""
if [ -n "${BQ_LITE_ENDPOINT:-}" ]; then
    ENDPOINT="$BQ_LITE_ENDPOINT"
else
    echo "==> Building bq-lite-server"
    cargo build -p bq-lite-server
    PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
    GRPC_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
    LOG_FILE="$CACHE_DIR/dbt_server.log"
    "$ROOT_DIR/target/debug/bq-lite-server" --port "$PORT" --grpc-port "$GRPC_PORT" --project test-proj --memory >"$LOG_FILE" 2>&1 &
    SERVER_PID=$!
    ENDPOINT="http://127.0.0.1:$PORT"

    cleanup() {
        if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
            kill "$SERVER_PID" >/dev/null 2>&1 || true
            wait "$SERVER_PID" >/dev/null 2>&1 || true
        fi
    }
    trap cleanup EXIT

    for _ in $(seq 1 50); do
        if curl -fsS "$ENDPOINT/healthz" >/dev/null 2>&1; then
            break
        fi
        sleep 0.1
    done
    curl -fsS "$ENDPOINT/healthz" >/dev/null
fi

DBT_VENV="$CACHE_DIR/dbt_venv"
if [ ! -x "$DBT_VENV/bin/dbt" ]; then
    echo "==> Setting up dbt-bigquery virtualenv"
    python3 -m venv "$DBT_VENV"
    "$DBT_VENV/bin/pip" install --quiet --upgrade pip certifi
    CERT_PATH="$("$DBT_VENV/bin/python" -c 'import certifi; print(certifi.where())')"
    SSL_CERT_FILE="$CERT_PATH" "$DBT_VENV/bin/pip" install --quiet dbt-bigquery
fi

DBT_PROJ="$CACHE_DIR/dbt_advanced_proj"
rm -rf "$DBT_PROJ"
mkdir -p "$DBT_PROJ/models" "$DBT_PROJ/seeds" "$DBT_PROJ/snapshots" "$DBT_PROJ/macros" "$DBT_PROJ/tests"

cat >"$DBT_PROJ/dbt_project.yml" <<'YAML'
name: 'bqlite_adv'
version: '1.0.0'
profile: 'bqlite_adv'
model-paths: ["models"]
seed-paths: ["seeds"]
snapshot-paths: ["snapshots"]
macro-paths: ["macros"]
test-paths: ["tests"]
vars:
  batch: 1
models:
  bqlite_adv:
    +schema: mart
YAML

cat >"$DBT_PROJ/profiles.yml" <<YAML
bqlite_adv:
  target: dev
  outputs:
    dev:
      type: bigquery
      method: oauth-secrets
      token: "local-token"
      project: test-proj
      dataset: dbt_adv
      threads: 4
      api_endpoint: "$ENDPOINT"
YAML

# Custom generate_schema_name macro
cat >"$DBT_PROJ/macros/generate_schema_name.sql" <<'SQL'
{% macro generate_schema_name(custom_schema_name, node) -%}
    {%- set default_schema = target.schema -%}
    {%- if custom_schema_name is none -%}
        {{ default_schema }}
    {%- else -%}
        {{ default_schema }}_{{ custom_schema_name | trim }}
    {%- endif -%}
{%- endmacro %}
SQL

# Seed (Phase 1)
cat >"$DBT_PROJ/seeds/raw_customers.csv" <<'CSV'
id,name,email,tier,updated_at
1,Alice,alice@example.com,silver,2026-01-01 00:00:00 UTC
2,Bob,bob@example.com,gold,2026-01-02 00:00:00 UTC
3,Charlie,charlie@example.com,bronze,2026-01-03 00:00:00 UTC
CSV

# Ephemeral model
cat >"$DBT_PROJ/models/eph_active_customers.sql" <<'SQL'
{{ config(materialized='ephemeral') }}
SELECT
  id,
  UPPER(name) AS name_upper,
  email,
  tier,
  CAST(updated_at AS TIMESTAMP) AS updated_at
FROM {{ ref('raw_customers') }}
WHERE tier != 'inactive'
SQL

# View model referencing ephemeral
cat >"$DBT_PROJ/models/stg_customers.sql" <<'SQL'
{{ config(materialized='view') }}
SELECT id, name_upper, email, tier, updated_at
FROM {{ ref('eph_active_customers') }}
SQL

# Partitioned + clustered table model
cat >"$DBT_PROJ/models/dim_customers_table.sql" <<'SQL'
{{ config(
    materialized='table',
    partition_by={'field': 'updated_date', 'data_type': 'date'},
    cluster_by=['tier']
) }}
SELECT
  id,
  name_upper,
  email,
  tier,
  DATE(updated_at) AS updated_date
FROM {{ ref('stg_customers') }}
SQL

# Incremental model: merge + on_schema_change='append_new_columns'
cat >"$DBT_PROJ/models/inc_events_merge.sql" <<'SQL'
{{ config(
    materialized='incremental',
    incremental_strategy='merge',
    unique_key='event_id',
    on_schema_change='append_new_columns'
) }}
{% if var('batch') == 1 %}
SELECT 1 AS event_id, 1 AS customer_id, 100 AS amount, DATE '2026-01-01' AS event_date
UNION ALL
SELECT 2 AS event_id, 2 AS customer_id, 200 AS amount, DATE '2026-01-02' AS event_date
{% else %}
SELECT 2 AS event_id, 2 AS customer_id, 250 AS amount, DATE '2026-01-02' AS event_date, 'updated' AS extra_tag
UNION ALL
SELECT 3 AS event_id, 3 AS customer_id, 300 AS amount, DATE '2026-01-03' AS event_date, 'new' AS extra_tag
{% endif %}
SQL

# Incremental model: insert_overwrite with date partition_by
cat >"$DBT_PROJ/models/inc_daily_overwrite.sql" <<'SQL'
{{ config(
    materialized='incremental',
    incremental_strategy='insert_overwrite',
    partition_by={'field': 'event_date', 'data_type': 'date'}
) }}
{% if var('batch') == 1 %}
SELECT DATE '2026-01-01' AS event_date, 10 AS total_amount
UNION ALL
SELECT DATE '2026-01-02' AS event_date, 20 AS total_amount
{% else %}
SELECT DATE '2026-01-02' AS event_date, 99 AS total_amount
UNION ALL
SELECT DATE '2026-01-03' AS event_date, 30 AS total_amount
{% endif %}
SQL

# Incremental model: on_schema_change='sync_all_columns'
cat >"$DBT_PROJ/models/inc_schema_sync.sql" <<'SQL'
{{ config(
    materialized='incremental',
    incremental_strategy='merge',
    unique_key='id',
    on_schema_change='sync_all_columns'
) }}
{% if var('batch') == 1 %}
SELECT 1 AS id, 'v1' AS keep_col, 'legacy' AS drop_me
{% else %}
SELECT 1 AS id, 'v2' AS keep_col, 42 AS added_num
UNION ALL
SELECT 2 AS id, 'v2_new' AS keep_col, 84 AS added_num
{% endif %}
SQL

# Generic schema tests (unique, not_null, accepted_values, relationships)
cat >"$DBT_PROJ/models/schema.yml" <<'YAML'
version: 2
models:
  - name: dim_customers_table
    columns:
      - name: id
        data_tests: [unique, not_null]
      - name: tier
        data_tests:
          - accepted_values:
              arguments:
                values: ['bronze', 'silver', 'gold', 'platinum']
  - name: inc_events_merge
    columns:
      - name: event_id
        data_tests: [unique, not_null]
      - name: customer_id
        data_tests:
          - relationships:
              arguments:
                to: ref('dim_customers_table')
                field: id
YAML

# Singular test
cat >"$DBT_PROJ/tests/assert_positive_amounts.sql" <<'SQL'
SELECT * FROM {{ ref('inc_events_merge') }} WHERE amount <= 0
SQL

# Snapshot 1: timestamp strategy
cat >"$DBT_PROJ/snapshots/snap_customers_ts.sql" <<'SQL'
{% snapshot snap_customers_ts %}
{{
    config(
      target_schema='dbt_adv_snapshots',
      unique_key='id',
      strategy='timestamp',
      updated_at='updated_at',
    )
}}
SELECT * FROM {{ ref('stg_customers') }}
{% endsnapshot %}
SQL

# Snapshot 2: check strategy
cat >"$DBT_PROJ/snapshots/snap_customers_check.sql" <<'SQL'
{% snapshot snap_customers_check %}
{{
    config(
      target_schema='dbt_adv_snapshots',
      unique_key='id',
      strategy='check',
      check_cols=['tier', 'email'],
    )
}}
SELECT * FROM {{ ref('stg_customers') }}
{% endsnapshot %}
SQL

run_dbt() {
    (cd "$DBT_PROJ" && GOOGLE_OAUTH_ACCESS_TOKEN="local-token" "$DBT_VENV/bin/dbt" "$@" --profiles-dir "$DBT_PROJ")
}

echo "==> Phase 1: Initial dbt build (seeds + models + snapshots + tests, threads=4)"
run_dbt build

echo "==> Phase 2: Mutate seed + incremental batch 2 + schema evolution + SCD2 snapshots"
cat >"$DBT_PROJ/seeds/raw_customers.csv" <<'CSV'
id,name,email,tier,updated_at
1,Alice,alice.new@example.com,platinum,2026-01-10 00:00:00 UTC
2,Bob,bob@example.com,gold,2026-01-02 00:00:00 UTC
3,Charlie,charlie@example.com,bronze,2026-01-03 00:00:00 UTC
4,Diana,diana@example.com,silver,2026-01-10 00:00:00 UTC
CSV

run_dbt seed --full-refresh
run_dbt run --vars '{"batch": 2}'
run_dbt snapshot
run_dbt test

echo "==> Phase 3: Verify exact BigQuery state via REST API"
BQ_ENDPOINT="$ENDPOINT" python3 - <<'PY'
import json
import os
import urllib.request

ep = os.environ["BQ_ENDPOINT"]

def bq_query(sql):
    req = urllib.request.Request(
        f"{ep}/bigquery/v2/projects/test-proj/queries",
        data=json.dumps({"query": sql, "useLegacySql": False}).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req) as resp:
            data = json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"Query failed ({e.code}): {sql}\nBody: {e.read().decode()}") from e
    assert data.get("jobComplete"), data
    cols = [f["name"] for f in data["schema"]["fields"]]
    rows = [[cell["v"] for cell in r["f"]] for r in data.get("rows", [])]
    return cols, rows

# 1. Incremental merge + append_new_columns
cols, rows = bq_query("SELECT event_id, amount, extra_tag FROM `test-proj`.dbt_adv_mart.inc_events_merge ORDER BY event_id")
assert rows == [["1", "100", None], ["2", "250", "updated"], ["3", "300", "new"]], f"inc_events_merge mismatch: {cols} {rows}"

# 2. Incremental insert_overwrite (2026-01-01 preserved, 2026-01-02 overwritten 20->99, 2026-01-03 added)
_, rows = bq_query("SELECT CAST(event_date AS STRING), total_amount FROM `test-proj`.dbt_adv_mart.inc_daily_overwrite ORDER BY event_date")
assert rows == [["2026-01-01", "10"], ["2026-01-02", "99"], ["2026-01-03", "30"]], f"inc_daily_overwrite mismatch: {rows}"

# 3. Incremental sync_all_columns (drop_me removed, added_num added)
cols, rows = bq_query("SELECT * FROM `test-proj`.dbt_adv_mart.inc_schema_sync ORDER BY id")
assert "drop_me" not in [c.lower() for c in cols], f"drop_me should be dropped: {cols}"
assert "added_num" in [c.lower() for c in cols], f"added_num missing: {cols}"
assert len(rows) == 2, f"inc_schema_sync row count: {rows}"

# 4. Snapshot timestamp SCD2 (id=1 has 2 rows: 1 closed, 1 open; id=2,3,4 each have 1 open row => 5 total)
_, rows = bq_query("""
    SELECT id, COUNT(*) AS total, COUNTIF(dbt_valid_to IS NULL) AS open_cnt
    FROM `test-proj`.dbt_adv_snapshots.snap_customers_ts
    GROUP BY id ORDER BY id
""")
assert rows == [["1", "2", "1"], ["2", "1", "1"], ["3", "1", "1"], ["4", "1", "1"]], f"snap_customers_ts mismatch: {rows}"

# 5. Snapshot check SCD2 (same SCD2 shape)
_, rows = bq_query("""
    SELECT id, COUNT(*) AS total, COUNTIF(dbt_valid_to IS NULL) AS open_cnt
    FROM `test-proj`.dbt_adv_snapshots.snap_customers_check
    GROUP BY id ORDER BY id
""")
assert rows == [["1", "2", "1"], ["2", "1", "1"], ["3", "1", "1"], ["4", "1", "1"]], f"snap_customers_check mismatch: {rows}"

print("All REST state assertions PASSED!")
PY

echo "==> Phase 4: Full refresh & idempotent dbt build"
run_dbt run --full-refresh --vars '{"batch": 2}'
run_dbt build --vars '{"batch": 2}'

echo "==> Advanced dbt-bigquery E2E suite PASSED"
