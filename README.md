# bq-lite

A pure-Rust, embedded **GoogleSQL (BigQuery) engine** and **BigQuery REST API v2 + gRPC Storage Read/Write server** for fast local development, CI, dbt pipelines, and AI coding agents.

Zero C/C++, zero SQLite, zero ZetaSQL/Bazel, zero Docker dependency.

## Highlights

- **Pure-Rust GoogleSQL Engine (`bq-lite-core`):** Hand-rolled lexer, recursive-descent parser, CoW `Engine::fork_isolated()` snapshots, 165+ scalar functions (incl. exact `FARM_FINGERPRINT`, JSON, regex, timezones, `RANGE`, `NET.*`, `ST_*` geography, `SEARCH`), 20 aggregates, 11 window functions, `QUALIFY`, `PIVOT`/`UNPIVOT`, pipe syntax (`|>`), `MERGE`, scripting (`DECLARE`/`IF`/`LOOP`/`BEGIN..EXCEPTION`/`EXECUTE IMMEDIATE`), Row Access Policies, column masking, and live `INFORMATION_SCHEMA` (incl. `JOBS`, `VIEWS`, `PARTITIONS`, `ROW_ACCESS_POLICIES`).
- **BigQuery REST v2 + gRPC Storage API (`bq-lite-server`):** Axum HTTP REST v2 (`/bigquery/v2/...`, multipart & resumable CSV/NDJSON/Parquet uploads, `insertAll` dedup, `dryRun`, typed query parameters, `STORAGE_EMULATOR_HOST` `gs://` bridge) plus `tonic` gRPC `BigQueryRead` (`ARROW` IPC + `AVRO`, multi-stream & `SplitReadStream`, `row_restriction`) and `BigQueryWrite` (`_default`, `COMMITTED`, `PENDING`, `BUFFERED`, `proto_rows`, `arrow_rows`).
- **Optional `js-udf` Feature (`boa_engine`):** Sandboxed pure-Rust JavaScript engine supporting `CREATE FUNCTION ... LANGUAGE js` with ES features (`RegExp`, `Date`, `JSON`, loops, arrow functions, `Uint8Array`), loop/recursion limits, and BigQuery type mapping (`STRUCT`, `ARRAY`, `TIMESTAMP`, `BYTES`, `NUMERIC`).
- **Incremental `redb` Persistence:** Optional (`default = ["persistence"]`) multi-key durable storage across server restarts.

## Workspace Crates

| Crate | Path | Purpose |
|---|---|---|
| `bq-lite-core` | `crates/bq_lite_core` | Embedded GoogleSQL engine, CoW catalog/storage, Arrow/Parquet/CSV/NDJSON I/O, optional `redb` persistence & `boa_engine` JS UDFs |
| `bq-lite-server` | `crates/bq_lite_server` | Standalone BigQuery REST v2 + gRPC Storage API emulator, interactive `repl`, and one-shot `exec` CLI |
| `bq-lite-test-support` | `crates/bq_lite_test_support` | Shared test assertion helpers |

## Installation & Build

```bash
# Default build (REST v2 + gRPC Storage + redb persistence)
cargo build --release -p bq-lite-server

# With sandboxed JavaScript UDFs (boa_engine)
cargo build --release -p bq-lite-server --features js-udf
```

## Embedded Rust Usage (`bq-lite-core`)

```rust
use bq_lite_core::Engine;

let mut engine = Engine::new();
engine.exec_script("
  CREATE SCHEMA analytics;
  CREATE TABLE analytics.orders (id INT64, amount NUMERIC);
  INSERT INTO analytics.orders VALUES (1, NUMERIC '19.99');
")?;

let res = engine.query("SELECT id, amount FROM analytics.orders;")?;
assert_eq!(res.rows.len(), 1);

// O(1) copy-on-write fork for parallel isolated tests:
let test_fork = engine.fork_isolated();
```

## CLI Reference (`bq-lite-server`)

### Global Flags & Environment Variables

| Flag | Env Var | Default | Description |
|---|---|---|---|
| `--host <IP>` | `BQ_LITE_HOST` | `0.0.0.0` | Bind host for HTTP REST and gRPC listeners |
| `--port <PORT>` | `BQ_LITE_PORT` | `9050` | Port for the HTTP REST v2 API |
| `--grpc-port <PORT>` | `BQ_LITE_GRPC_PORT` | `9060` | Port for the gRPC Storage Read/Write API |
| `--project <PROJECT>` | `BQ_LITE_PROJECT` | `local` | Default GCP project ID |
| `--data-dir <PATH>` | `BQ_LITE_DATA_DIR` | *(none)* | `redb` database path/directory for durable state across restarts |
| `--memory` | — | `false` | Force ephemeral in-memory mode (ignores `--data-dir` / `BQ_LITE_DATA_DIR`) |
| `--seed <SQL_FILE>` | — | — | Execute a `.sql` script on startup before listening |
| `-q, --quiet` | — | `false` | Suppress startup tracing logs (useful in CI/agent scripts) |
| `-v, --version` | — | — | Print version and exit |

### Subcommands

```bash
# 1. Start HTTP REST v2 + gRPC Storage server (ephemeral in-memory)
bq-lite-server --memory --port 9050 --grpc-port 9060

# 2. Start durable server seeded with schema/fixtures
bq-lite-server --data-dir ./bq_state.redb --seed ./init.sql

# 3. Interactive GoogleSQL REPL (.tables, .schema <table>, .help, .quit)
bq-lite-server --memory repl

# 4. One-shot SQL execution (ideal for CI & AI agents; supports table|json|csv)
bq-lite-server --memory exec "SELECT 1 + 1 AS sum" --format json
bq-lite-server --data-dir ./bq_state.redb exec --file ./query.sql --format csv
```

## Connecting Official SDKs, `dbt-bigquery` & `bq` CLI

### Python (`google-cloud-bigquery` + `google-cloud-bigquery-storage`)

```python
import grpc
from google.auth.credentials import AnonymousCredentials
from google.cloud import bigquery, bigquery_storage
from google.cloud.bigquery_storage_v1.services.big_query_read import transports

client = bigquery.Client(
    project="local",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://127.0.0.1:9050"},
)

channel = grpc.insecure_channel("127.0.0.1:9060")
bqstorage = bigquery_storage.BigQueryReadClient(
    transport=transports.BigQueryReadGrpcTransport(channel=channel)
)
df = client.query("SELECT 1 AS x").to_dataframe(bqstorage_client=bqstorage)
```

### Node.js (`@google-cloud/bigquery`)

```js
const { BigQuery } = require("@google-cloud/bigquery");
const bq = new BigQuery({ projectId: "local", apiEndpoint: "http://127.0.0.1:9050" });
```

### Go (`cloud.google.com/go/bigquery`)

```go
client, err := bigquery.NewClient(ctx, "local",
    option.WithEndpoint("http://127.0.0.1:9050/bigquery/v2/"),
    option.WithoutAuthentication(),
)
```

### Rust (`gcp-bigquery-client`)

```rust
let client = ClientBuilder::new()
    .with_v2_base_url("http://127.0.0.1:9050/bigquery/v2".to_owned())
    .build_from_authenticator(Arc::new(LocalAuth))
    .await?;
```

### `dbt-bigquery` (`profiles.yml`)

```yaml
my_profile:
  target: dev
  outputs:
    dev:
      type: bigquery
      method: oauth-secrets
      token: "local-token"
      project: local
      dataset: analytics
      threads: 1
      api_endpoint: "http://127.0.0.1:9050"
```

### `bq` CLI

```bash
bq --api http://127.0.0.1:9050 --project_id=local --use_gce_service_account=false query --use_legacy_sql=false "SELECT 1"
```

## Verification & Quality Gates

```bash
# Formatting, file-size/layering DAG checks, Clippy (-D warnings), cargo-deny, full unit/integration/proptest/SLT suites
./scripts/quality-gate.sh

# Compatibility & feature-matrix verification
./scripts/test-compat.sh

# Live official SDKs & dbt-bigquery E2E suite
./scripts/test-sdks.sh
```

## License

Licensed under the [MIT License](LICENSE).
