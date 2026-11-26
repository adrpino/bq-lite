# bq-lite — Engineering & Agent Execution Plan

> **Name availability:** Checked crates.io and GitHub — neither `bq-lite` nor `bqlite` is currently taken.

## 1. Vision & Design Principles

`bq-lite` is a **pure-Rust, embedded GoogleSQL (BigQuery) engine and wire-compatible BigQuery REST v2 + gRPC Storage API server** built for millisecond local development, CI integration testing, and embedded Rust workflows — without Docker, JVM/Python runtimes, or brittle SQLite/ZetaSQL wrappers.

### Architectural Lessons Borrowed from `tsql-lite`
1. **Own the frontend and core engine end-to-end:** Hand-roll the lexer, recursive-descent parser, AST, binder, type system, and executor in pure Rust. Never couple the AST or query planner to a third-party dialect crate (like `sqlparser-rs`) or a foreign query engine frontend that breaks on real-world dialect edge cases.
2. **Clean workspace crate separation:** Separate embedded engine (`bq-lite-core`) from wire server (`bq-lite-server`) and test harness utilities (`bq-lite-test-support`), keeping WASM optional for later.
3. **Zero-copy CoW cloning (`fork_isolated()`):** Copy-on-write catalog and table snapshots enable instant, isolated per-test databases in parallel Rust test suites and `POST /_bqlite/reset` in server mode.
4. **Dual storage modes (`--memory` + `redb`):** Pure in-memory default for CI speed, optional `redb`-backed persistence for durable local dev.
5. **Code-enforced parity discipline:**
   - Maintain a living `docs/compatibility-matrix.md` and `docs/googlesql-function-parity.md`.
   - Enforce `docs/googlesql-function-parity.md` against the live engine registries via an automated test (`googlesql_function_parity_doc.rs`), mirroring `tsql_lite_core/tests/tsql_function_parity_doc.rs`.
   - Verify end-to-end against official client SDKs (Python `google-cloud-bigquery`, Node `@google-cloud/bigquery`, Go `cloud.google.com/go/bigquery`, Rust `gcp-bigquery-client`, and `bq` CLI / REST).

### What We Intentionally Drop from `tsql-lite`
BigQuery is an analytical warehouse, not an OLTP row-locking RDBMS. **Do not** port:
- TDS binary wire protocol (replaced by HTTP/JSON REST v2 + gRPC/Arrow Storage API).
- Row-level locking hints (`UPDLOCK`, `READPAST`, `HOLDLOCK`), deadlock detection, RCSI/SNAPSHOT 3-way row merge, or `AFTER`/`INSTEAD OF` triggers.
- Collations/extended properties/filegroups/SSMS probes (replaced by BigQuery `OPTIONS(...)`, partitioning/clustering metadata, and `INFORMATION_SCHEMA`).

---

## 2. Target Workspace Layout

```text
bq-lite/
├── Cargo.toml
├── AGENTS.md                           # Agent build/test rules & workflow
├── PLAN.md                             # This roadmap
├── docs/
│   ├── compatibility-matrix.md         # High-level subsystem scoreboard
│   ├── googlesql-function-parity.md    # Code-verified function inventory
│   └── error-parity.md                 # BigQuery HTTP status / reason / message parity
├── scripts/
│   ├── test-sdks.sh                    # Live Python / Node / Go / Rust SDK E2E checks
│   └── test-compat.sh                  # Optional differential check vs BigQuery Sandbox / dry-run
└── crates/
    ├── bq_lite_core/                   # Pure Rust GoogleSQL engine
    │   └── src/
    │       ├── lib.rs                  # Public Engine / Session / QueryResult API
    │       ├── error.rs                # BqError (HTTP code, reason, location, message)
    │       ├── types/                  # BqType & BqValue (nested Array/Struct/Numeric/Json/Geo/Range)
    │       ├── ast/                    # Hand-rolled GoogleSQL AST (expressions, queries, ddl, dml, script)
    │       ├── parser/                 # Hand-rolled lexer, token stream, recursive-descent parser
    │       ├── catalog/                # CoW Project -> Dataset -> Table/View/Routine/Policy registries
    │       ├── storage/                # In-memory CoW table chunks + optional redb persistence + time-travel snapshots
    │       ├── io/                     # Load/Extract codecs (CSV, NDJSON, Parquet, Arrow IPC, Avro)
    │       └── executor/
    │           ├── engine.rs           # Engine, Session, fork_isolated(), query/exec entrypoints
    │           ├── binder.rs           # Name resolution, 3-part paths, implicit coercions, schema inference
    │           ├── query/              # SELECT, JOIN, UNNEST, CTE, QUALIFY, PIVOT/UNPIVOT, set ops, pipe syntax
    │           ├── evaluator/          # Expression eval, SAFE. prefix suppression, subqueries, ARRAY(SELECT ...)
    │           ├── scalar/             # Modular built-in function registry (string, math, date, json, array, struct, hash, net, geo)
    │           ├── aggregates.rs       # Standard + APPROX_* + ARRAY_AGG + STRING_AGG + LOGICAL_AND/OR
    │           ├── window.rs           # Analytic functions, named WINDOW, RANGE/ROWS frames, IGNORE/RESPECT NULLS
    │           ├── dml/                # INSERT, UPDATE (incl. deep STRUCT), DELETE, MERGE, TRUNCATE
    │           ├── ddl/                # CREATE/ALTER/DROP SCHEMA/TABLE/VIEW/MV/FUNCTION/PROCEDURE/SNAPSHOT/CLONE
    │           ├── script/             # DECLARE, SET, IF, CASE, LOOP/WHILE/FOR, BEGIN..EXCEPTION, EXECUTE IMMEDIATE, CALL, ASSERT
    │           ├── js_udf.rs           # Optional feature-gated JavaScript UDF runtime (boa_engine or rquickjs)
    │           └── metadata/           # INFORMATION_SCHEMA.* virtual table providers
    ├── bq_lite_server/                 # Standalone HTTP REST v2 + gRPC Storage API server
    │   └── src/
    │       ├── main.rs                 # CLI (--port 9050, --grpc-port 9060, --project, --memory/--data-dir)
    │       ├── rest/                   # axum handlers: projects, datasets, tables, tabledata, jobs, queries, routines, models, oauth stub
    │       ├── wire_json.rs            # Exact BigQuery REST f/v JSON encoder & schema mapper
    │       ├── jobs.rs                 # Sync/async job manager, pagination (pageToken), dry-run, cancellation
    │       ├── gcs.rs                  # Local gs:// directory / STORAGE_EMULATOR_HOST bridge for Load/Extract
    │       └── grpc/                   # tonic BigQuery Storage Read API (Arrow IPC / Avro) & Storage Write API
    └── bq_lite_test_support/           # Shared fixtures, HTTP/SDK test helpers
```

### Approved Leaf Dependencies (No Framework Lock-In)
- **Core:** `serde`, `serde_json`, `thiserror`, `parking_lot`, `chrono`, `chrono-tz`, `regex`, `rust_decimal` (or fixed-precision `i128`/`i256` for `NUMERIC`/`BIGNUMERIC`), `uuid`, `hex`, `base64`, `md-5`, `sha1`, `sha2`, `redb` (optional `persistence` feature).
- **File / Wire Formats:** `arrow` + `parquet` + `csv` (used strictly for Load/Extract jobs and Storage Read/Write API serialization — **never** dictating internal expression semantics).
- **Server:** `tokio`, `axum`, `tonic`, `prost`, `tower-http`, `clap`, `tracing`.
- **Optional JS UDFs:** `rquickjs` or `boa_engine` behind a `js-udf` Cargo feature.

---

## 3. Core Architecture Details (BigQuery-Specific)

### 3.1 Value & Type System (`types/`)
Unlike `tsql-lite`'s flat scalars, BigQuery's type system is **recursively nested** from day one. Model `BqType` and `BqValue` natively:
- Scalars: `Bool`, `Int64`, `Float64`, `Numeric` (38 digits, 9 fractional), `BigNumeric` (76.76 digits, 38 fractional), `String`, `Bytes`, `Date`, `Datetime`, `Time`, `Timestamp` (UTC microsecond precision), `Interval`, `Json`, `Geography` (WKT/GeoJSON), `Range<T>`.
- Composite: `Array(Box<BqType>)` (note: BigQuery forbids `ARRAY<ARRAY<T>>` — enforce this!), `Struct(Vec<StructField>)` (named or anonymous fields, positional matching rules).
- Special: `UntypedNull` (coerces to any type during binding).
- Implement BigQuery's **supertype / implicit coercion matrix** centrally in `types/coercion.rs` (e.g., `INT64 -> NUMERIC -> BIGNUMERIC -> FLOAT64`, string literal coercion to `DATE`/`TIMESTAMP`/`NUMERIC` in typed contexts).

### 3.2 `SAFE.` Error Suppression
A defining BigQuery feature is the `SAFE.` prefix (`SAFE.PARSE_DATE(...)`, `SAFE.DIVIDE(...)`, `SAFE.SUBSTR(...)`, `SAFE_CAST(...)`, `SAFE_OFFSET(i)`, `SAFE_ORDINAL(i)`).
- Distinguish **`BqErrorKind::Semantic`** (overflow, invalid cast/date, out-of-bounds array index, divide-by-zero) from **`BqErrorKind::Query`** (unknown column, type mismatch, syntax error).
- When evaluating a `SAFE.`-prefixed call, catch **only** `BqErrorKind::Semantic` and convert it to `BqValue::Null` at the row level without aborting the query.

### 3.3 Catalog & CoW Isolation (`catalog/` & `storage/`)
- Hierarchy: `Catalog -> HashMap<ProjectId, Arc<Project>> -> HashMap<DatasetId, Arc<Dataset>> -> Tables / Views / MaterializedViews / Routines / RowAccessPolicies`.
- Case-sensitivity rules matching BigQuery:
  - Dataset IDs, Table IDs, Project IDs: **case-sensitive** (unless dataset `is_case_insensitive = TRUE`).
  - Column names, function names, keywords, `STRUCT` field names: **case-insensitive** (preserving display case).
- Wrap table data and catalog registries in `Arc` with `Arc::make_mut` copy-on-write (mirroring `tsql-lite`'s `cow_registry.rs`):
  - Enables `engine.fork_isolated()` in O(1).
  - Enables BigQuery **Table Snapshots**, **Table Clones**, and **Time Travel (`FOR SYSTEM_TIME AS OF`)** by retaining immutable versioned snapshots keyed by timestamp.

### 3.4 BigQuery REST Wire Format (`wire_json.rs`)
Official SDKs are notoriously picky about the BigQuery REST v2 JSON envelope. Build one dedicated, heavily tested serializer:
- Every row is `{"f": [{"v": ...}, ...]}`.
- `INT64`, `NUMERIC`, `BIGNUMERIC` serialize as **JSON strings** (not JSON numbers, to preserve 64-bit / decimal precision).
- `TIMESTAMP` serializes as **epoch seconds string** (e.g. `"1.700000000E9"` or decimal seconds `"1700000000.123456"`).
- `BYTES` serializes as **base64 string**.
- `BOOL` serializes as `"true"` / `"false"` strings in `f/v` rows.
- `ARRAY` (`mode: "REPEATED"`) serializes as `[{"v": elem1}, {"v": elem2}]` (never `null` for an empty array; always `[]`).
- `STRUCT` (`type: "RECORD"`) serializes as `{"f": [{"v": field1}, ...]}`.
- Error responses use Google's exact JSON error envelope:
  ```json
  { "error": { "code": 400, "message": "...", "errors": [{ "message": "...", "domain": "global", "reason": "invalidQuery" }], "status": "INVALID_ARGUMENT" } }
  ```

---

## 4. Phased Implementation Roadmap (Agent Execution Guide)

Agents should execute **strictly one phase at a time**, keeping `cargo test` green and updating `docs/compatibility-matrix.md` and `docs/googlesql-function-parity.md` at the end of every phase.

### Phase 0 — Workspace Bootstrap & Testing Discipline `[DONE]`
- Initialize Cargo workspace (`bq_lite_core`, `bq_lite_server`, `bq_lite_test_support`) with the exact `[workspace.lints]`, `clippy.toml`, `rustfmt.toml`, `deny.toml`, `scripts/check-structure.sh`, and `scripts/quality-gate.sh` defined in [`QUALITY_AND_TESTING.md`](./QUALITY_AND_TESTING.md).
- Create `AGENTS.md`, `docs/compatibility-matrix.md`, `docs/googlesql-function-parity.md`, and `docs/error-parity.md`.
- Implement `BqError`, `BqType`, `BqValue`, type formatting, and `coercion.rs` with thorough unit tests (including `ARRAY<ARRAY<T>>` rejection and `NUMERIC`/`BIGNUMERIC` arithmetic).
- **Exit gate:** `./scripts/quality-gate.sh` passes cleanly; `BqValue` covers all BigQuery types.

### Phase 1 — Hand-Rolled GoogleSQL Lexer, Parser & AST `[DONE]`
- Lexer handling:
  - Backtick identifiers (including single-token 3-part paths `` `my-proj.my_ds.my_table` `` and escaped backticks).
  - Single/double-quoted strings, triple-quoted strings (`'''...'''`, `"""..."""`), raw strings (`r'...'`), byte strings (`b'...'`, `rb'...'`).
  - Comments (`--`, `#`, `/* ... */`).
  - Query parameters (`@named_param`, `?` positional).
- Recursive-descent parser covering:
  - Expressions: operators, `CAST`/`SAFE_CAST`, `STRUCT(...)`, `[...]` array literals, `ARRAY<T>[...]`, `STRUCT<...>(...)`, `arr[OFFSET(i)]`/`SAFE_OFFSET`/`ORDINAL`/`SAFE_ORDINAL`, `SAFE.func(...)`, `CASE`, `IN UNNEST(...)`, `EXISTS`, scalar & `ARRAY(SELECT ...)` subqueries, `EXTRACT(part FROM ...)`, `INTERVAL ...`.
  - Queries: `WITH [RECURSIVE]`, `SELECT [DISTINCT|AS STRUCT|AS VALUE]`, `* EXCEPT(...) REPLACE(...)`, `FROM`, `JOIN`, `UNNEST(...) [WITH OFFSET [AS ...]]`, `WHERE`, `GROUP BY [ALL|ROLLUP|CUBE|GROUPING SETS]`, `HAVING`, `QUALIFY`, `WINDOW`, `ORDER BY ... NULLS FIRST/LAST`, `LIMIT ... OFFSET`, `UNION/INTERSECT/EXCEPT ALL/DISTINCT`.
  - DDL & DML skeletons (`CREATE/ALTER/DROP`, `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE`).
- **Exit gate:** 150+ parser round-trip & edge-case tests covering real BigQuery queries without any external SQL parser crate.

### Phase 2 — Catalog, CoW Storage & Core Query Execution `[DONE]`
- Implement `Catalog` (`Project -> Dataset -> Table/View`) with `Arc` CoW cloning and `Engine::fork_isolated()`.
- Implement `Binder` & `QueryExecutor`:
  - `SELECT` projections (`* EXCEPT`, `* REPLACE`, `AS STRUCT`, `AS VALUE`, alias visibility in `GROUP BY`/`HAVING`/`QUALIFY`/`ORDER BY`).
  - Joins (`INNER`, `LEFT`, `RIGHT`, `FULL`, `CROSS`, `USING`, correlated `CROSS JOIN UNNEST` + `LEFT JOIN UNNEST` + `WITH OFFSET`).
  - CTEs (including `WITH RECURSIVE`), derived tables, scalar/correlated/`ARRAY(...)` subqueries, set operations (`UNION ALL/DISTINCT`, `INTERSECT DISTINCT`, `EXCEPT DISTINCT`), `PIVOT`/`UNPIVOT`.
  - Aggregations (`COUNT`, `COUNTIF`, `SUM`, `AVG`, `MIN`, `MAX`, `ARRAY_AGG` with `IGNORE NULLS`/`ORDER BY`/`LIMIT`, `STRING_AGG`, `ANY_VALUE`, `LOGICAL_AND`, `LOGICAL_OR`, `BIT_AND/OR/XOR`, `STDDEV*`, `VAR*`, `APPROX_COUNT_DISTINCT`, `APPROX_QUANTILES`, `APPROX_TOP_COUNT`, `GROUPING SETS`/`ROLLUP`/`CUBE`/`GROUP BY ALL`).
  - Window functions (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `NTILE`, `LAG`, `LEAD`, `FIRST_VALUE`, `LAST_VALUE`, `NTH_VALUE`, `PERCENT_RANK`, `CUME_DIST`, `PERCENTILE_CONT`, `PERCENTILE_DISC`, `ROWS`/`RANGE` frames, `IGNORE/RESPECT NULLS`, named `WINDOW`, `QUALIFY`).
- **Exit gate:** Core analytical SQL suite passes; `fork_isolated()` verified under concurrent multi-threaded tests.

### Phase 3 — Built-in Function Library & Code-Verified Parity Doc `[DONE]`
- Implement modular `executor/scalar/` registry + `tests/googlesql_function_parity_doc.rs` (failing CI if docs and registry drift):
  - **String & Regex:** `CONCAT`, `SUBSTR`/`SUBSTRING`, `LENGTH`/`BYTE_LENGTH`/`CHAR_LENGTH`, `STRPOS`, `INSTR`, `SPLIT`, `TRIM`/`LTRIM`/`RTRIM`, `LPAD`/`RPAD`, `REPLACE`, `TRANSLATE`, `REVERSE`, `REPEAT`, `LEFT`/`RIGHT`, `LOWER`/`UPPER`/`INITCAP`, `ENDS_WITH`/`STARTS_WITH`, `FORMAT`, `TO_BASE64`/`FROM_BASE64`, `TO_HEX`/`FROM_HEX`, `NORMALIZE`, `REGEXP_CONTAINS`, `REGEXP_EXTRACT`, `REGEXP_EXTRACT_ALL`, `REGEXP_INSTR`, `REGEXP_REPLACE`, `REGEXP_SUBSTR`.
  - **Date / Time / Timestamp:** `CURRENT_DATE/DATETIME/TIME/TIMESTAMP`, `DATE`, `DATETIME`, `TIME`, `TIMESTAMP`, `DATE_ADD`/`SUB`/`DIFF`/`TRUNC`, `DATETIME_ADD`/`SUB`/`DIFF`/`TRUNC`, `TIME_ADD`/`SUB`/`DIFF`/`TRUNC`, `TIMESTAMP_ADD`/`SUB`/`DIFF`/`TRUNC`, `TIMESTAMP_SECONDS/MILLIS/MICROS`, `UNIX_SECONDS/MILLIS/MICROS/DATE`, `DATE_FROM_UNIX_DATE`, `EXTRACT`, `LAST_DAY`, `FORMAT_DATE/DATETIME/TIME/TIMESTAMP`, `PARSE_DATE/DATETIME/TIME/TIMESTAMP`, `GENERATE_DATE_ARRAY`, `GENERATE_TIMESTAMP_ARRAY` (with full `chrono-tz` IANA timezone support).
  - **Math & Hash:** `ABS`, `SIGN`, `ROUND`, `TRUNC`, `CEIL`/`CEILING`, `FLOOR`, `MOD`, `DIV`, `IEEE_DIVIDE`, `SAFE_DIVIDE`, `SAFE_ADD/SUBTRACT/MULTIPLY/NEGATE`, `POW`/`POWER`, `SQRT`, `CBRT`, `EXP`, `LN`, `LOG`, `LOG10`, `GREATEST`, `LEAST`, `RAND`, `RANGE_BUCKET`, `MD5`, `SHA1`, `SHA256`, `SHA512`, `FARM_FINGERPRINT` (exact FarmHash64 implementation!).
  - **Array & Struct:** `ARRAY_LENGTH`, `ARRAY_TO_STRING`, `ARRAY_CONCAT`, `ARRAY_REVERSE`, `ARRAY_FIRST`, `ARRAY_LAST`, `ARRAY_SLICE`, `GENERATE_ARRAY`.
  - **JSON:** `TO_JSON`, `TO_JSON_STRING`, `PARSE_JSON`, `JSON_QUERY`, `JSON_VALUE`, `JSON_QUERY_ARRAY`, `JSON_VALUE_ARRAY`, `JSON_EXTRACT`, `JSON_EXTRACT_SCALAR`, `JSON_EXTRACT_ARRAY`, `JSON_EXTRACT_STRING_ARRAY`, `JSON_TYPE`, `JSON_OBJECT`, `JSON_ARRAY`, `JSON_SET`, `JSON_REMOVE`, `JSON_KEYS`, `INT64`/`FLOAT64`/`BOOL`/`STRING` JSON lax/strict accessors, dot/bracket JSON subscripting (`j.a.b[0]`).
  - **Conditional / Utility:** `IF`, `IFNULL`, `NULLIF`, `COALESCE`, `GENERATE_UUID`, `ERROR`, `COALESCE`, `NET.*`, `SAFE.*` wrapper across all scalar functions.
- **Exit gate:** `googlesql_function_parity_doc` test passes; `FARM_FINGERPRINT` matches Google BigQuery golden outputs bit-for-bit.

### Phase 4 — DDL, DML, Views, Snapshots & Time Travel `[DONE]`
- **DDL:** `CREATE [OR REPLACE] [TEMP] TABLE [IF NOT EXISTS]` (including `AS SELECT`, `LIKE`, `COPY`, `CLONE`, `SNAPSHOT`, `PARTITION BY`, `CLUSTER BY`, `OPTIONS(...)`, `DEFAULT`, `NOT NULL`, `PRIMARY KEY`/`FOREIGN KEY` `NOT ENFORCED`), `ALTER TABLE` (`ADD/DROP/RENAME/ALTER COLUMN`, `SET OPTIONS`, `RENAME TO`), `DROP TABLE`, `CREATE/DROP SCHEMA`, `CREATE [OR REPLACE] [MATERIALIZED] VIEW`, `CREATE/DROP FUNCTION` (SQL UDFs + table functions), `CREATE/DROP PROCEDURE`.
- **DML:** `INSERT INTO ... VALUES / SELECT`, `UPDATE ... SET ... [FROM ...] WHERE ...` (including nested `STRUCT` field assignment), `DELETE [FROM] ... WHERE ...`, `MERGE INTO ... USING ... ON ... WHEN MATCHED / NOT MATCHED [BY TARGET | BY SOURCE]`, `TRUNCATE TABLE`, `THEN RETURN` on `INSERT`/`UPDATE`/`DELETE`.
- **Time Travel & Wildcards:** Retain timestamped CoW table versions for `FOR SYSTEM_TIME AS OF`; support wildcard tables (`` `dataset.events_*` ``) with `_TABLE_SUFFIX` virtual column and `_PARTITIONTIME`/`_PARTITIONDATE` pseudo-columns.
- **Exit gate:** Full DDL/DML/MERGE/wildcard/time-travel integration suite passes.

### Phase 5 — BigQuery Scripting, Procedures & `INFORMATION_SCHEMA` `[DONE — optional js-udf deferred]`
- **Scripting:** Multi-statement scripts, `DECLARE`, `SET`, `CREATE TEMP TABLE`, `CREATE TEMP FUNCTION`, `IF ... ELSEIF ... ELSE ... END IF`, `CASE ... END CASE`, `LOOP`/`WHILE`/`REPEAT`/`FOR ... IN`, `BREAK`/`LEAVE`/`CONTINUE`/`ITERATE`, `BEGIN ... EXCEPTION WHEN ERROR THEN ... END` (`@@error.message`, `@@error.statement_text`), `BEGIN TRANSACTION` / `COMMIT` / `ROLLBACK`, `EXECUTE IMMEDIATE ... INTO ... USING ...`, `CALL`, `RAISE USING MESSAGE`, `ASSERT`, `@@row_count`, `@@script.*`.
- **`INFORMATION_SCHEMA`:** Dataset-scoped and region-scoped (`region-us.INFORMATION_SCHEMA.*`) virtual tables: `SCHEMATA`, `SCHEMATA_OPTIONS`, `TABLES`, `TABLE_OPTIONS`, `COLUMNS`, `COLUMN_FIELD_PATHS`, `VIEWS`, `ROUTINES`, `ROUTINE_OPTIONS`, `PARAMETERS`, `JOBS` / `JOBS_BY_PROJECT`, `PARTITIONS`.
- **Optional `js-udf` feature:** `CREATE FUNCTION ... LANGUAGE js AS r"""..."""` backed by `rquickjs`/`boa_engine`.
- **Exit gate:** Real multi-statement BigQuery migration/ETL scripts and `INFORMATION_SCHEMA` introspection queries pass.

### Phase 6 — BigQuery REST API v2 Server (`bq_lite_server`) `[DONE — v0.1.4]`
- Build standalone `axum` server (default `:9050`, `--memory` or `--data-dir <path>`, `--project <default>`).
- Serve both `/bigquery/v2/projects/{projectId}/...` and `/projects/{projectId}/...` (Node SDK compatibility):
  - **Projects:** `GET /projects`
  - **Datasets:** `GET/POST /datasets`, `GET/PUT/PATCH/DELETE /datasets/{datasetId}`
  - **Tables:** `GET/POST /datasets/{ds}/tables`, `GET/PUT/PATCH/DELETE /datasets/{ds}/tables/{tableId}`
  - **Tabledata:** `GET /tables/{id}/data` (pagination), `POST /tables/{id}/insertAll` (streaming inserts with `insertId` dedup & `skipInvalidRows`/`ignoreUnknownValues`)
  - **Jobs:** `GET/POST /jobs` (query, load, copy, extract; multipart & resumable uploads at `/upload/bigquery/v2/projects/{p}/jobs`), `GET /jobs/{jobId}`, `POST /jobs/{jobId}/cancel`, `GET /queries/{jobId}` (pagination via `pageToken`, `maxResults`, `timeoutMs`)
  - **Sync Queries:** `POST /queries` (including `dryRun`, `parameterMode: NAMED|POSITIONAL`, `queryParameters`, `defaultDataset`, `useLegacySql` rejection)
  - **Routines & Stubs:** `GET/POST/PUT/DELETE /datasets/{ds}/routines`, plus graceful stubs for discovery/models/row-access-policies so SDK probes never crash.
  - **Auth & Health:** Mock OAuth `/token`, `/oauth2/v4/token`, `GET /healthz`, `POST /_bqlite/reset`.
- **Load & Extract Jobs:** CSV, NDJSON, Parquet (and Avro if needed) from multipart upload or `gs://` mapped to local `$DATA_DIR/gcs` or `STORAGE_EMULATOR_HOST`.
- **Exit gate:** `./scripts/test-sdks.sh` passes end-to-end against **official** Python (`google-cloud-bigquery`), Go (`cloud.google.com/go/bigquery`), Node (`@google-cloud/bigquery`), and Rust (`gcp-bigquery-client`) clients.

### Phase 7 — gRPC BigQuery Storage Read & Write API + Persistence `[DONE — v0.1.6]`
- Implement `tonic` gRPC server (default `:9060`) serving:
  - `google.cloud.bigquery.storage.v1.BigQueryRead` (`CreateReadSession`, `ReadRows`, `SplitReadStream`) streaming native **Apache Arrow IPC** record batches (so Python `.to_dataframe(bqstorage_client=...)`, Polars, Spark, and Rust clients work at full speed).
  - `google.cloud.bigquery.storage.v1.BigQueryWrite` (`CreateWriteStream`, `AppendRows`, `FinalizeWriteStream`, `BatchCommitWriteStreams`).
- Implement `redb` persistence backend (`persistence` feature) for catalog metadata, table chunks, and job history across restarts, plus `bq-lite` CLI REPL (`bq-lite repl`).
- **Exit gate:** Storage Read/Write API verified from Python & Rust SDKs; restart persistence & crash recovery tests pass.

### Phase 8 — Governance, Pipe Syntax & Continuous Parity `[DONE — v0.1.6]`
- Row-Level Security (`CREATE ROW ACCESS POLICY`), column-level masking/policy tags (opt-in enforcement mode).
- GoogleSQL **Pipe Syntax** (`FROM t |> WHERE ... |> AGGREGATE ... |> EXTEND ...`).
- Differential test suite (`./scripts/test-compat.sh`) comparing `bq-lite` results & error behavior against live BigQuery Sandbox golden snapshots.

---

## 5. Rules for Autonomous Agents Working on `bq-lite`

> **Mandatory companion spec:** Follow [`QUALITY_AND_TESTING.md`](./QUALITY_AND_TESTING.md) strictly (workspace Clippy lints, 500-line file cap, 80-line function cap, zero-panic prod code, layering DAG, and 7-layer test plan). Every task must pass `./scripts/quality-gate.sh`.

1. **No silent parser shims:** If syntax is recognized by the lexer/parser but not yet implemented in the executor, return an explicit `BqError::unimplemented(...)` (HTTP `501` / clear message) rather than silently producing wrong rows.
2. **Test before moving on:** Every new SQL feature or REST endpoint must ship with:
   - Core unit/integration tests in `crates/bq_lite_core/tests/`.
   - Wire/SDK regression test in `crates/bq_lite_server/tests/` when touching HTTP/gRPC.
   - Updated entries in `docs/compatibility-matrix.md` and `docs/googlesql-function-parity.md`.
3. **Preserve `tsql-lite` quality invariants:**
   - Keep `cargo test --workspace` fast (sub-second core tests).
   - Keep `bq_lite_core` completely independent of `tokio`/`axum`/`tonic` so it stays cleanly embeddable (and future WASM-compatible if desired).
   - Never add `sqlparser-rs` or `datafusion` as core dependencies unless explicitly approved — own the AST, binder, and evaluator.
4. **Verify against real SDK wire contracts:** Never guess BigQuery REST JSON shapes; verify `f/v` encoding, pagination tokens, `dryRun` responses, and error payloads directly against the official Python/Go/Node/Rust client libraries.

---

## 6. Active Execution Backlog (Next Agent Tasks)

Execute **strictly one milestone at a time** in order (`A` → `B` → `C` → `D` → `E` → `F`). Do **not** skip ahead; every milestone must leave `./scripts/quality-gate.sh` passing cleanly and update `docs/compatibility-matrix.md` / `docs/googlesql-function-parity.md` / `docs/error-parity.md` where applicable.

### Milestone A — Quality Gate, Structural Hygiene & Persistence Wiring `[DONE — v0.1.1]`
> **Why first:** Fixes a silent `find` bug in `scripts/check-structure.sh` that allowed 4 source files to cross the 500-line cap, wires persistence into all mutating REST handlers, and prevents unbounded job memory growth.

1. **Fix `scripts/check-structure.sh` file discovery bug:**
   - Replace line 18 (`find crates -path 'crates/*/src/*.rs' -o -path 'crates/*/src/**/*.rs' -print0`, which misses nested files on macOS BSD `find` and drops the first `-path` due to `-o` precedence) with:
     `find crates -type f -name '*.rs' -path '*/src/*' -print0`
2. **Split the 4 source files currently exceeding 500 lines** (keep every `.rs` file under `crates/*/src/` <= 400 lines after splitting):
   - `crates/bq_lite_core/src/executor/scalar/datetime.rs` (563 lines) → split arithmetic/trunc/diff helpers into `datetime_calc.rs`.
   - `crates/bq_lite_core/src/executor/evaluator/mod.rs` (515 lines) → split function/UDF dispatch into `calls.rs`.
   - `crates/bq_lite_server/src/rest/datasets_tables.rs` (513 lines) → split into `datasets.rs`, `tables.rs`, and `tabledata.rs`.
   - `crates/bq_lite_core/src/executor/query/from.rs` (508 lines) → split join execution helpers into `joins.rs`.
3. **Clean up `deny.toml` warnings:**
   - Remove unused license allowances (`ISC`, `Zlib`) from `deny.toml` so `cargo deny check bans licenses` emits no license warnings.
4. **Complete `--data-dir` server persistence wiring (`crates/bq_lite_server/src/`):**
   - Call `state.persist_if_enabled()` after every mutating REST handler: `insert_dataset`, `patch_dataset`, `delete_dataset`, `insert_table`, `patch_table`, `delete_table`, `insert_all`, `sync_query`, and `insert_job` (whenever DDL/DML/load/copy succeeds).
   - Add an integration test in `crates/bq_lite_server/tests/rest_contract.rs` verifying that creating a dataset/table, inserting rows via `insertAll` and `POST /queries`, dropping `ServerState`, and reopening `ServerState::with_data_dir(...)` preserves all datasets, tables, schemas, and rows.
5. **Bound job history memory (`goccy/bigquery-emulator#512`):**
   - In `crates/bq_lite_server/src/state.rs`, cap retained completed jobs (e.g., `MAX_RETAINED_JOBS = 1_000` FIFO/LRU eviction via `VecDeque<String>` alongside the job map) so running thousands of repeated `SELECT 1` queries has O(1) memory and constant latency.
   - Add a regression test asserting bounded job table size and non-growing memory across 1,500 sequential `POST /queries` calls.
- **Exit gate:** `./scripts/quality-gate.sh` passes with 0 files > 500 lines and 0 license warnings.

---

### Milestone B — `goccy/bigquery-emulator` Real-World Parity & Bug Blitz `[DONE — v0.1.2]`
> **Goal:** Systematically audit, fix, and lock down the highest-impact real-world BigQuery bugs filed against `goccy/bigquery-emulator`, turning them into permanent regression suites:
> - Core SQL regressions: `crates/bq_lite_core/tests/goccy_parity.rs`
> - REST & wire regressions: `crates/bq_lite_server/tests/goccy_rest_parity.rs`

1. **Full `queryParameters` decoding (`#509`, `#511`):**
   - In `crates/bq_lite_server/src/rest/jobs_queries.rs` (or a new `rest/params.rs` helper), support recursive BigQuery `QueryParameterType` & `QueryParameterValue`:
     - All scalar types (`INT64`, `FLOAT64`, `BOOL`, `NUMERIC`, `BIGNUMERIC`, `STRING`, `BYTES`, `DATE`, `DATETIME`, `TIME`, `TIMESTAMP`, `JSON`, `GEOGRAPHY`) respecting the declared `parameterType` — e.g. `NUMERIC` must decode as `BqValue::Numeric`, never `BqValue::String` (`#511`).
     - `ARRAY` (`parameterType.type == "ARRAY"`, `parameterType.arrayType`, `parameterValue.arrayValues`) so `ArrayQueryParameter` works for `IN UNNEST(@arr)` (`#509`).
     - `STRUCT` (`parameterType.type == "STRUCT"`, `parameterType.structTypes`, `parameterValue.structValues`).
   - Add `Engine::exec_script_with_params` in `bq_lite_core` and use it in `run_sql_on_engine` so multi-statement scripts and DML (`INSERT`/`UPDATE`/`DELETE`/`MERGE`) also receive `@named` and `?` parameters.
2. **REST status codes, error parity & Terraform (`hashicorp/google`) compatibility (`#514`, `#513`, `#484`, `#532`, `#470`):**
   - Verify duplicate dataset/table creation returns HTTP `409 Conflict` with `reason: "duplicate"` and `status: "ALREADY_EXISTS"` (breaks Python `exists_ok=True` and Terraform if wrong — `#514`).
   - Verify `DELETE` endpoints (`datasets`, `tables`, `routines`) return `204 No Content` with an **empty body** (never `200` + `"null\n"` — `#513`).
   - Ensure dataset & table JSON responses include standard fields required by `hashicorp/google` Terraform provider & official SDKs (`etag`, `selfLink`, `creationTime`, `lastModifiedTime`, `location`, `labels`, `description`, `friendlyName`, `access` on datasets; `type`, `numBytes`, `numRows`, `creationTime`, `lastModifiedTime`, `etag`, `selfLink`, `labels`, `timePartitioning`, `clustering` on tables — `#484`).
   - Add tests proving `tabledata.list` strictly isolates identical table names across different datasets (`#532`) and rows inserted via `tabledata.insertAll` are immediately visible to subsequent `SELECT` queries (`#470`).
3. **Wire JSON, `TIMESTAMP`, `BYTES` & `RECORD` fidelity (`#482`, `#498`, `#500`, `#501`, `#483`):**
   - Verify nested `STRUCT`/`RECORD` and `REPEATED RECORD` `TIMESTAMP` fields encode as epoch-seconds strings in REST `f`/`v` (never RFC3339 — `#482`).
   - Verify `BYTES` values round-trip via `insertAll`, `POST /queries`, and `tabledata.list` with **single** base64 encoding (never double-encoded — `#498`).
   - Verify `CAST(TIMESTAMP ... AS STRING)` defaults to **UTC** (`YYYY-MM-DD HH:MM:SS[.ffffff]+00`), never local machine timezone (`#500`).
   - Enforce BigQuery's **microsecond** precision limit on `TIMESTAMP` string parsing: reject sub-microsecond literals (>6 fractional digits, e.g. `'2024-01-01 00:00:00.1234567 UTC'`) with `BqErrorKind::Semantic` (`#501`).
   - Verify accessing fields inside `ARRAY<STRUCT<...>>` (`REPEATED RECORD`) always binds by field name/position deterministically across multiple fields (`#483`).
4. **JSONL & CSV Load Job edge cases (`#510`, `#435`, `#384`):**
   - In `crates/bq_lite_core/src/io/csv_json.rs` and `crates/bq_lite_server/src/rest/load_arrow.rs`:
     - Verify and fix NDJSON loading for `REPEATED RECORD` (`ARRAY<STRUCT<...>>`) and `REPEATED STRING` (`ARRAY<STRING>`), including missing keys (`[]`) and empty arrays (`#510`).
     - Support CSV load options from `configuration.load`: `nullMarker` (`#435`), `skipLeadingRows`, `fieldDelimiter`, `quote`, `allowQuotedNewlines`, `writeDisposition` (`WRITE_TRUNCATE`, `WRITE_APPEND`, `WRITE_EMPTY`), `createDisposition`, and `autodetect` (`#384`).
     - Support `load_table_from_csv` / `load_table_from_json` multipart AND **resumable upload** protocol (`uploadType=resumable` → `Location` header → `PUT` payload) used by Python/Go/Node SDKs.
5. **Job types, `destinationTable`, `dryRun` schema & failed-script atomicity (`#353`, `#523`):**
   - Implement `configuration.copy` (`CopyConfiguration`: `sourceTable` / `sourceTables`, `destinationTable`, `writeDisposition`, `createDisposition` — `#353`).
   - Implement `configuration.query.destinationTable` + `writeDisposition` (`WRITE_TRUNCATE`/`WRITE_APPEND`/`WRITE_EMPTY`) + `createDisposition` (`CREATE_IF_NEEDED`/`CREATE_NEVER`).
   - Improve `dryRun: true` on `POST /queries` and `POST /jobs` so it executes against an O(1) `eng.fork_isolated()` (or binder) to return real `schema` + `statistics.query.schema` + syntax/binding errors **without** mutating live state.
   - Ensure failed multi-statement scripts in `jobs.insert` either roll back or keep catalog & storage 100% consistent (no stale `tables.get`/`tables.list` — `#523`).
   - Add `GET/POST/PUT/DELETE /datasets/{ds}/routines` REST endpoints backed by the catalog routine registry.
6. **SQL semantics & architectural regression proofs (`#487`, `#502`, `#481`, `#517`, `#313`, `#436`, `#530`):**
   - Reject `SELECT DISTINCT` (and `GROUP BY`) on unhashable `ARRAY` and `JSON` columns with `BqErrorKind::Query` (matching BigQuery's `Grouping by expressions of type ARRAY is not allowed` — `#487`).
   - Support `EXPORT DATA OPTIONS(uri=..., format='JSON'|'CSV', ...) AS <query>` (writing to local path / data-dir GCS root) and ensure `ARRAY<STRUCT>` exports as native JSON arrays of objects, never stringified JSON (`#530`).
   - Add explicit regression tests proving: `QUALIFY` (`#502`), `ALTER TABLE ADD/DROP/RENAME/SET OPTIONS` (`#481`), datasets named `ai`, `ml`, `obj`, `mysql` (`#517`), dropping tables releases storage rows immediately (`#313`), and implicit comma join vs explicit `INNER`/`LEFT JOIN` parity (`#436`).
- **Exit gate:** `goccy_parity` and `goccy_rest_parity` pass; `./scripts/quality-gate.sh` passes.

---

### Milestone C — Test Coverage Push (`QUALITY_AND_TESTING.md` Layer 2 & Layer 7) `[DONE — v0.1.3]`
> **Current baseline:** `70.73%` workspace line coverage.
> **Target:** `>= 85%` `bq_lite_core`, `>= 90%` `parser/`, `types/`, `wire_json.rs`, `>= 80%` `bq_lite_server`.

1. **Raise lowest-coverage core modules to target:**
   - `types/coercion.rs` (37% → >=90%) & `types/bq_value.rs` (45% → >=90%) & `types/bq_type.rs` (64% → >=90%) & `types/numeric.rs` (65% → >=90%): exhaustive coercion matrix, `BIGNUMERIC` ops, `INTERVAL`/`RANGE`/`JSON`/`GEOGRAPHY` casts and `SAFE_CAST` errors.
   - `executor/scalar/datetime.rs` (33% → >=85%), `math_hash.rs` (42% → >=85%), `json.rs` (44% → >=85%), `regex_fn.rs` (51% → >=85%), `array_util.rs` (58% → >=85%), `string.rs` (64% → >=85%): add happy + `NULL` + edge + `SAFE.` error tests for every registered scalar function.
   - `executor/query/set_ops.rs` (42% → >=85%), `evaluator/subquery.rs` (44% → >=85%), `evaluator/ops.rs` (48% → >=85%), `query/pipe.rs` (52% → >=85%): cover `INTERSECT`/`EXCEPT` (`ALL`/`DISTINCT`/`BY NAME`), correlated `EXISTS`/`IN`/scalar subqueries, bitwise/range/like/null-safe operators, and all pipe operators.
2. **Raise server coverage to >=80% (`wire_json.rs` >=90%):**
   - Test every `BqType` (including `NaN`/`Infinity`/`-Infinity`, `NUMERIC`, `BIGNUMERIC`, `BYTES`, `DATE`, `DATETIME`, `TIME`, `TIMESTAMP`, `JSON`, `GEOGRAPHY`, `RANGE`, nested `ARRAY<STRUCT<...>>`) in `wire_json.rs` encode + decode + error paths.
   - Test `PUT`/`PATCH`/`DELETE`, `skipInvalidRows`, `ignoreUnknownValues`, `deleteContents=false` on non-empty dataset, 404/409/400 envelopes, and `/projects/{id}/...` alias routes.
3. **Expand declarative `.slt` golden suite (`crates/bq_lite_core/tests/sql_suite/`):**
   - Split/add `02_arrays_structs_unnest.slt`, `03_joins_ctes_subqueries.slt`, `04_aggregates_and_windows.slt`, `05_dml_and_merge.slt`, `06_ddl_views_snapshots.slt`, `07_scripting_and_procedures.slt`, `08_information_schema.slt`.
- **Exit gate:** `cargo llvm-cov --workspace --all-features --summary-only` meets Layer 7 targets and `./scripts/quality-gate.sh` passes.

---

### Milestone D — Official Client SDK E2E Verification (`scripts/test-sdks.sh`) `[DONE — v0.1.4]`
> **Goal:** Prove unmodified official BigQuery SDKs work end-to-end against `bq-lite-server` with zero hacks.

1. Create `scripts/test-sdks.sh` that builds and launches `bq-lite-server --port <ephemeral> --project test-proj --memory`, waits for `GET /healthz`, runs SDK suites (auto-skipping any toolchain not installed locally, failing if an installed SDK fails), and tears down cleanly via `trap`:
   - **Python (`google-cloud-bigquery`):** `create_dataset(..., exists_ok=True)`, `create_table`, `insert_rows_json`, `load_table_from_file` (CSV + NDJSON with `REPEATED RECORD`), `query` with `ScalarQueryParameter` (`NUMERIC`, `INT64`, `TIMESTAMP`) + `ArrayQueryParameter`, `dry_run=True`, `copy_table`, `list_rows` pagination, `delete_table`, `delete_dataset(delete_contents=True)`.
   - **Go (`cloud.google.com/go/bigquery`):** Dataset/table CRUD, `Inserter.Put`, parameterized `Query.Read`, `Copier.Run`, `LoaderFrom` CSV/JSON.
   - **Node (`@google-cloud/bigquery`):** Verify `/projects/{projectId}/...` non-prefixed paths, `dataset.create`, `table.insert`, `bigquery.query` with named params, `getRows` pagination.
   - **Rust (`gcp-bigquery-client`):** Dataset/table create, `query`, `insert_all`.
2. Fix every subtle REST header/JSON/status discrepancy uncovered by the live SDKs (e.g. `Content-Type`, `etag`, `jobComplete`, `totalRows` omission on empty DML, `pageToken` handling, OAuth stub paths).
3. Document `bq` CLI / `dbt-bigquery` local emulator connection flags in `README.md` (`goccy #423`, `#429`).
- **Exit gate:** `./scripts/test-sdks.sh` passes for all installed SDKs + `./scripts/quality-gate.sh` passes.

---

### Milestone E — Phase 7: `tonic` gRPC BigQuery Storage Read & Write API + Parquet/GCS `[DONE — v0.1.5]`
> **Goal:** Implement native BigQuery Storage v1 gRPC server on `--grpc-port` (default `9060`) + Parquet I/O, explicitly avoiding every known `goccy/bigquery-emulator` Storage API defect.

1. **Proto & `tonic` setup (`crates/bq_lite_server/`):**
   - Add `tonic`, `prost`, `prost-types` (and checked-in or `tonic-build` compiled `google.cloud.bigquery.storage.v1` protos) strictly inside `bq_lite_server` (never in `bq_lite_core`).
   - Run HTTP REST (`--port`) and gRPC (`--grpc-port`) concurrently in `crates/bq_lite_server/src/main.rs` sharing the same `ServerState`.
2. **Storage Read API (`crates/bq_lite_server/src/grpc/read.rs`):**
   - Implement `BigQueryRead`: `CreateReadSession`, `ReadRows`, `SplitReadStream`.
   - **Arrow IPC framing (`goccy #493`, `#398`):** Send the Arrow schema **once** in `ReadSession.arrow_schema.serialized_schema` (bare Arrow IPC `Schema` message) and send **bare Arrow IPC `RecordBatch` messages** (NOT full streams with schema prepended) in each `ReadRowsResponse.arrow_record_batch.serialized_record_batch`.
   - Support column projection (`selected_fields`, including `ARRAY` and nested `STRUCT` columns — `#399`), row restriction predicates (`row_restriction`), `ReadRowsRequest.offset` resumption (`#507`), and reading anonymous query result tables (`_script`/`_anon` result table references from `jobs.insert` — `#527`).
3. **Storage Write API (`crates/bq_lite_server/src/grpc/write.rs`):**
   - Implement `BigQueryWrite`: `CreateWriteStream`, `GetWriteStream`, `AppendRows` (bidirectional stream), `FinalizeWriteStream`, `FlushRows`, `BatchCommitWriteStreams`.
   - Support all 3 stream modes: `COMMITTED` (immediately visible — `#247`), `PENDING` (staged until `FinalizeWriteStream` + `BatchCommitWriteStreams`, supporting multiple sequential `PENDING` streams on the same table — `#352`), and `BUFFERED` (staged until `FlushRows`, honoring append offsets and never double-inserting already-flushed rows — `#505`).
   - Return gRPC `Status::not_found` (never `Status::unknown`) when the target table does not exist (`#504`).
4. **Parquet I/O & local `gs://` bridge:**
   - Implement `crates/bq_lite_core/src/io/parquet_codec.rs` (`load_parquet` / `export_parquet`) behind the existing `arrow` feature (or new `parquet` feature).
   - Implement `crates/bq_lite_server/src/rest/gcs.rs` mapping `gs://bucket/path` to `$DATA_DIR/gcs/bucket/path` (or `STORAGE_EMULATOR_HOST` HTTP proxy) for `load` and `extract` jobs.
5. **Contract & SDK verification:**
   - Add `crates/bq_lite_server/tests/grpc_storage_contract.rs` testing `BigQueryRead` (Arrow IPC decoding, `ARRAY`/`STRUCT`, `offset` resume, anonymous query results) and `BigQueryWrite` (`COMMITTED`, `PENDING` x2 streams, `BUFFERED` + `FlushRows`, `NOT_FOUND` on missing table).
   - Extend `scripts/test-sdks.sh` with Python `google-cloud-bigquery-storage` `.to_dataframe(bqstorage_client=...)`.
- **Exit gate:** gRPC contract tests + Python Storage SDK test + `./scripts/quality-gate.sh` pass.

---

### Milestone F — Remaining Language Completeness, Governance & CLI REPL `[DONE — v0.1.6]`
1. **CLI REPL (`bq-lite repl`):**
   - Add subcommand mode (`bq-lite serve` / `bq-lite repl` while keeping existing flag compatibility) supporting multi-line SQL, pretty ASCII table output, `.tables`, `.schema <table>`, `.quit`, and `--data-dir`.
2. **Remaining GoogleSQL constructs:**
   - Implement `Expr::Lambda` execution for `ARRAY_TRANSFORM(arr, x -> ...)` and `ARRAY_FILTER(arr, x -> ...)`.
   - Implement `Expr::RangeLiteral` (`RANGE<DATE|DATETIME|TIMESTAMP>`) + `RANGE_START`, `RANGE_END`, `RANGE_OVERLAPS`, `RANGE_INTERSECT`, `RANGE_CONTAINS`.
   - Implement ingestion-time partitioning pseudo-columns `_PARTITIONTIME` and `_PARTITIONDATE` (`goccy #317`).
   - Complete remaining `INFORMATION_SCHEMA` views: `SCHEMATA_OPTIONS`, `TABLE_OPTIONS`, `COLUMN_FIELD_PATHS`, `ROUTINE_OPTIONS`, `PARAMETERS`, `JOBS` / `JOBS_BY_PROJECT`, `PARTITIONS`.
   - Implement `NET.*` (`NET.IP_FROM_STRING`, `NET.SAFE_IP_FROM_STRING`, `NET.IP_TO_STRING`, `NET.HOST`, `NET.REG_DOMAIN`, `NET.PUBLIC_SUFFIX`) and core `GEOGRAPHY` (`ST_GEOGPOINT`, `ST_GEOGFROMTEXT`, `ST_ASTEXT`, `ST_DISTANCE`, `ST_WITHIN`, `ST_CONTAINS`).
3. **Governance & optional JS UDFs (Phase 8):**
   - Opt-in Row-Level Security (`CREATE ROW ACCESS POLICY`) predicate enforcement on `SELECT` based on session user/roles.
   - Optional `js-udf` Cargo feature (`CREATE FUNCTION ... LANGUAGE js AS ...`).
   - Differential compatibility harness `scripts/test-compat.sh` for snapshot comparison against live BigQuery Sandbox.
- **Exit gate:** All `docs/compatibility-matrix.md` and `docs/googlesql-function-parity.md` entries updated and verified by `./scripts/quality-gate.sh`.

---

## 7. Post-Review Hardening & Completeness Backlog (Milestones G → K)

> **Origin:** Full codebase audit after `v0.1.6` against real-world BigQuery SQL semantics, official client SDKs, `dbt-bigquery`, Terraform (`hashicorp/google`), and remaining `goccy/bigquery-emulator` failure modes.
> Execute **strictly one milestone at a time** in order (`G` → `H` → `I` → `J` → `K`). Bump `[workspace.package].version` in `Cargo.toml` on each milestone and leave `./scripts/quality-gate.sh` (plus `./scripts/test-compat.sh` and `./scripts/test-sdks.sh`) green.

### Milestone G — Core SQL Correctness, Schema Inference & Short-Circuit Semantics `[DONE — v0.1.7]`
> **Goal:** Eliminate all core query/DML/DDL correctness bugs found during code review so empty-table queries, parameterized DML, combined grouping+windows, and guarded expressions behave identically to BigQuery.

1. **Parameterized DML & DDL + case-insensitive `@param` lookup:**
   - Thread `state.params` and `state.positional_params` into `execute_dml_with_vars` (`executor/script/mod.rs`, `executor/dml/mod.rs`, `executor/dml/merge.rs`) and `execute_ddl` (`executor/ddl/mod.rs`, `table.rs`) so `INSERT`/`UPDATE`/`DELETE`/`MERGE`, CTAS, and `EXPORT DATA` support `@named` and `?` parameters.
   - Normalize named parameter lookup in `evaluator/mod.rs` to case-insensitive matching (`@MyParam` == `@myparam`).
2. **Static / empty-row schema inference (0-row `SELECT`, `dryRun`, empty outer joins):**
   - Preserve projected column names and inferred `BqType`s in `build_select_result` (`executor/query/mod.rs`) even when 0 rows match (fixing `dryRun: true` on empty tables, 0-row SDK queries, and `CREATE TABLE t AS SELECT ... WHERE FALSE`).
   - Derive outer-join `null_template` (`executor/query/joins.rs`) from table/subquery schema metadata rather than `rows.first()` so `LEFT`/`RIGHT`/`FULL JOIN` against an empty table retains nullable columns instead of failing with `Unrecognized name`.
3. **Combined `GROUP BY` + Window Functions / `QUALIFY` & `GROUP BY` ordinals/aliases:**
   - Allow `GROUP BY` + `HAVING` followed by `apply_windows` + `QUALIFY` in `execute_select_full` (`executor/query/mod.rs`) instead of treating grouping and windows as mutually exclusive.
   - Support `GROUP BY 1, 2` (1-based `SELECT` ordinals) and `GROUP BY <select_alias>` in `executor/query/agg.rs`.
   - Fix `SELECT DISTINCT` + `ORDER BY` alignment (`executor/query/mod.rs`) so deduplication does not misalign `projected` with `qualify_src`.
4. **Short-circuit expression evaluation (`AND`/`OR`, `IF`, `COALESCE`, `IFNULL`):**
   - Make `BinaryOp::And` / `BinaryOp::Or` (`evaluator/mod.rs`) and `IF` / `COALESCE` / `IFNULL` (`evaluator/calls.rs`) short-circuit so guarded expressions like `WHERE x != 0 AND 10 / x > 1` or `IF(x = 0, 0, 10 / x)` do not evaluate untaken branches.
5. **`UNNEST(ARRAY<STRUCT<...>>)` auto-unpacking & `SELECT s.*`:**
   - Automatically unpack `STRUCT` fields into row scope when unnesting `ARRAY<STRUCT<...>>` (standard GoogleSQL: `SELECT a, b FROM UNNEST([STRUCT(1 AS a, 2 AS b)])` and `CROSS JOIN UNNEST(t.items)` without requiring alias qualification).
   - Support `SELECT struct_col.* [EXCEPT(...) REPLACE(...)]` expansion.
6. **Case-insensitive dataset storage parity & `STRUCT`/`ARRAY` `NULL` equality:**
   - Resolve canonical table keys via `DatasetMeta::find_table_key` before storage reads/writes (`storage/mod.rs` / `engine.rs` / `from.rs`) so `is_case_insensitive = TRUE` datasets actually return rows when queried with different casing.
   - Fix `values_equal` (`evaluator/ops.rs`) so comparing `STRUCT`/`ARRAY` containing `NULL` yields SQL `NULL` (unknown) under `=` / `!=`, while keeping `IS [NOT] DISTINCT FROM` and `GROUP BY` / `DISTINCT` null-equal.
7. **Partition metadata & `_PARTITIONTIME`/`_PARTITIONDATE` correctness:**
   - Fix `ddl/table.rs` storing `partition_by` via `format!("{e:?}")` (extract clean column name/expression string so REST `timePartitioning.field` is valid).
   - Preserve `pseudo_cols` in `combine_rows` (`executor/query/joins.rs`) so `_PARTITIONTIME`/`_PARTITIONDATE` work in joined queries, and record per-row ingestion timestamps instead of hardcoding `meta.created_at`.
- **Exit gate:** Core regression tests covering all 7 items pass in `bq_lite_core`; `./scripts/quality-gate.sh` and `./scripts/test-compat.sh` pass.

---

### Milestone H — REST v2 Fidelity, Script Atomicity, `autodetect` & Terraform/`dbt-bigquery` Parity `[DONE — v0.1.8]`
> **Goal:** Fix all REST server gaps affecting `dbt-bigquery`, Terraform (`hashicorp/google`), Python/Go/Node/Java SDKs, and binary/large uploads.

1. **Atomic multi-statement script execution (`goccy #523`):**
   - Execute multi-statement scripts in `jobs.insert` / `sync_query` (`rest/jobs_queries.rs`) on `eng.fork_isolated()` and swap into live `Engine` only on success so mid-script failures leave zero partial catalog or storage mutations.
2. **Binary multipart uploads & Load job tracking (`rest/load_arrow.rs`):**
   - Replace `std::str::from_utf8(body)` in `parse_upload_body` with byte-slice boundary scanning so binary Parquet multipart uploads work.
   - Honor caller-supplied `meta_json.jobReference.jobId` and record completed load jobs in `state.jobs` so `GET /jobs/{jobId}` polling succeeds.
3. **Accurate `job_json` & `table_json` metadata (`rest/jobs_queries.rs`, `rest/tables.rs`):**
   - Include `configuration.query.destinationTable` (pointing to `_bqlite_jobs._anon_<jobId>` when implicit, or the explicit destination table) in `job_json` so Python `query_job.to_dataframe(bqstorage_client=...)` auto-discovers query results.
   - Preserve actual job configuration (`query` vs `copy` vs `load` vs `extract`) in `JobRecord` and `job_json`.
   - Report actual `numRows` in `list_tables` and include `view: { "query": ..., "useLegacySql": false }` on view resources (store original SQL string on `TableMeta`).
4. **CSV & NDJSON `autodetect: true` & load options (`io/csv_json.rs`, `rest/load_arrow.rs`):**
   - Implement schema autodetection (`INT64`, `FLOAT64`, `BOOL`, `DATE`, `TIMESTAMP`, `STRING`, nested `RECORD`/`REPEATED` for NDJSON; header inference for CSV) when `autodetect: true` and no schema is supplied.
   - Support `allowQuotedNewlines`, `allowJaggedRows`, `ignoreUnknownValues`, and gzip-compressed upload payloads.
5. **`insertAll` & wire decoding hardening (`rest/tabledata.rs`, `wire_json.rs`):**
   - Validate rows *before* recording `insertId` in `insert_dedup` so rejected batches do not poison subsequent retries.
   - Make nested `STRUCT` field decoding case-insensitive in `decode_insert_all_value`, map JSON `null` on `REPEATED` columns to `[]`, and accept numeric epoch seconds/millis + space-separated UTC strings in `decode_timestamp`.
6. **REST pagination, filtering, `PUT` clearing & full `routines` CRUD:**
   - Implement `maxResults`, `pageToken`, `filter` (labels), `stateFilter`, and `allUsers` on `list_datasets`, `list_tables`, `list_jobs`, and `list_routines`.
   - Support `PUT` / `PATCH` clearing/updating `description`, `friendlyName`, `labels`, `view.query`, `timePartitioning`, `clustering`, `access`, `defaultTableExpirationMs`, and `expirationTime`.
   - Upgrade `rest/routines.rs` to parse/emit `arguments`, `returnType`, `routineType` (`SCALAR_FUNCTION`, `TABLE_VALUED_FUNCTION`, `PROCEDURE`), and add `PUT /datasets/{ds}/routines/{routineId}`.
7. **Bounded server state & GCS bridge completeness (`state.rs`, `storage/mod.rs`, `rest/gcs.rs`, `ddl/mod.rs`):**
   - Add FIFO/LRU caps to `insert_dedup`, `resumable_uploads`, `read_sessions`, `write_streams`, and `TableStorage.history`.
   - Support `STORAGE_EMULATOR_HOST` HTTP GET/PUT in `rest/gcs.rs` alongside local `$DATA_DIR/gcs`, and wire `gs://` resolution + `PARQUET` format into SQL `EXPORT DATA`.
- **Exit gate:** REST contract & `goccy_rest_parity` tests cover all 7 items; `./scripts/quality-gate.sh` and `./scripts/test-sdks.sh` pass.

---

### Milestone I — Arrow/Parquet Type Completeness & gRPC Storage API Production Parity `[DONE — v0.1.9]`
> **Goal:** Make the Arrow/Parquet codec lossless across all BigQuery types and make `BigQueryWrite` / `BigQueryRead` compatible with unmodified official Python, Go, and Java Storage SDKs.

1. **Complete `io/arrow_codec.rs` & `io/parquet_codec.rs`:**
   - Implement native Arrow array builders & extractors for all `ARRAY<T>` element types (`Float64`, `Bool`, `Bytes`, `Date`, `Timestamp`, `Numeric`, `Struct`) — fixing the schema mismatch error on non-`INT64`/`STRING` arrays.
   - Encode `BqType::Struct` as native Arrow `DataType::Struct`, `Numeric` as `Decimal128(38, 9)`, `BigNumeric` as `Decimal256(76, 38)`, `Time` as `Time64(Microsecond)`, and `Datetime` as `Timestamp(Microsecond, None)`.
   - Widen Parquet/Arrow readers (`arrow_to_bq_type` + `extract_arrow_cell`) to accept `Int8`/`16`/`32`, `UInt8`/`16`/`32`/`64`, `Float32`, `LargeUtf8`, `LargeBinary`, `Decimal128`, `Timestamp(Second|Millisecond|Nanosecond, _)`, and `Struct` emitted by pandas, Polars, DuckDB, and Spark.
2. **Storage Write API `proto_rows` + `_default` stream (`grpc/write.rs`, `grpc/proto.rs`):**
   - Implement dynamic protobuf row decoding for `AppendRowsRequest.proto_rows` (field 4: `ProtoSchema` `FileDescriptorProto` + serialized row wire bytes) so unmodified Python `BigQueryWriteClient`, Go `managedwriter`, and Java `StreamWriter` work directly.
   - Support the implicit `_default` write stream (`projects/{p}/datasets/{d}/tables/{t}/streams/_default`) with immediate committed writes and no prior `CreateWriteStream` call.
3. **Storage Read API completeness (`grpc/read.rs`, `grpc/proto.rs`):**
   - Apply `row_restriction` predicates when reading anonymous job result tables (`_bqlite_jobs` / `_anon_*`).
   - Implement `DataFormat.AVRO` serialization (in addition to `ARROW`) for Go/Java/Spark/Beam Storage Read clients.
   - Support multi-stream partitioning in `CreateReadSession` (`max_stream_count > 1`) and real midpoint splitting in `SplitReadStream`.
4. **Live SDK Storage verification:**
   - Extend `scripts/test-sdks.sh` to verify Python `query_job.to_dataframe(bqstorage_client=...)` (auto-handoff via `destinationTable`), `BigQueryWriteClient` (`proto_rows` + `_default`), and Parquet upload/extract round-trips.
- **Exit gate:** All Arrow/Parquet round-trip tests, `grpc_storage_contract`, `./scripts/quality-gate.sh`, and `./scripts/test-sdks.sh` pass.

---

### Milestone J — Governance, `INFORMATION_SCHEMA` Depth, SQL Extensions & Incremental Persistence `[DONE — v0.2.0]`
> **Goal:** Close remaining governance, metadata, SQL dialect, and persistence gaps required by enterprise analytics pipelines and `dbt-bigquery`.

1. **Live & complete `INFORMATION_SCHEMA` (`executor/metadata/`):**
   - Wire `INFORMATION_SCHEMA.JOBS` / `JOBS_BY_PROJECT` to live `ServerState` job records (via an optional job-provider hook on `ExecContext` so `bq_lite_core` stays decoupled from `tokio`/`axum`).
   - Add missing BigQuery columns required by `dbt-bigquery` & BI tools: `VIEWS.view_definition`, `TABLES.ddl` / `creation_time`, `PARTITIONS.total_rows` / `total_logical_bytes`, `COLUMNS.is_partitioning_column` / `clustering_ordinal_position` / `column_default`, and `INFORMATION_SCHEMA.ROW_ACCESS_POLICIES`.
2. **Governance enforcement:**
   - Enforce `grantees` matching in Row Access Policies against session principal (`SESSION_USER()` / configurable session roles) instead of applying all policies unconditionally.
   - Add opt-in column-level data masking / policy tags enforcement mode.
3. **Additional GoogleSQL DDL/DML & functions:**
   - Implement `LOAD DATA [OVERWRITE|INTO] <table> FROM FILES(...)`, `CREATE/DROP EXTERNAL TABLE` (backed by local CSV/NDJSON/Parquet), `ALTER SCHEMA SET OPTIONS`, `ALTER VIEW`, `TABLESAMPLE SYSTEM (N PERCENT)`, and `SEARCH()`.
   - Upgrade `ST_CONTAINS` / `ST_WITHIN` (`scalar/range_geo_net.rs`) to parse WKT/GeoJSON `POINT`/`POLYGON`/`LINESTRING` geometries (ray-casting / spherical containment instead of WKT string equality).
   - Implement optional `js-udf` Cargo feature (`CREATE FUNCTION ... LANGUAGE js AS r"""..."""` via `rquickjs` or `boa_engine`).
4. **Incremental `redb` persistence (`storage/persist.rs`, `state.rs`):**
   - Store catalog and per-table storage chunks under separate `redb` keys (instead of re-serializing the entire database + full history as one JSON blob on every mutation) and surface/log persistence errors instead of discarding them with `let _ = ...`.
- **Exit gate:** Governance, metadata, SQL extension, and incremental persistence crash-recovery tests pass; `docs/googlesql-function-parity.md` and `docs/compatibility-matrix.md` updated; `./scripts/quality-gate.sh` passes.

---

### Milestone K — Full SDK Matrix (`dbt-bigquery`, Rust, Go) & Layer 7 Coverage Lock-In `[DONE — v0.2.1]`
> **Goal:** Verify end-to-end compatibility across all target client ecosystems and re-verify `QUALITY_AND_TESTING.md` Layer 7 coverage after Milestones D–J.

1. **Coverage audit & lock-in (`QUALITY_AND_TESTING.md` Layer 7):**
   - Replaced the hand-rolled JS UDF subset with a sandboxed `boa_engine` 0.20 runtime (`--features js-udf`) with loop-iteration/recursion limits and full `BqValue <-> JsValue` conversion (`STRUCT`, `ARRAY`, `JSON`, `BYTES`/`Uint8Array`, `Date`, `NUMERIC`/`BigInt`).
   - Added targeted core & server tests (`milestone_k_coverage.rs`, `milestone_k_core_gaps.rs`, `milestone_k_server_coverage.rs`) achieving `92.7%` parser, `90.0%` types, `92.86%` `wire_json.rs`, `80.73%` `bq_lite_server`, and `82.36%` `bq_lite_core`.
2. **Complete `scripts/test-sdks.sh` matrix:**
   - Added Rust (`gcp-bigquery-client`), Go (`cloud.google.com/go/bigquery` guarded by `command -v go`), and `dbt-bigquery` (`dbt run` + `dbt test` smoke project) alongside Python (`google-cloud-bigquery` + `bigquery_storage` + `pyarrow`) and Node.js (`@google-cloud/bigquery`).
- **Exit gate:** `./scripts/quality-gate.sh`, `./scripts/test-compat.sh`, and `./scripts/test-sdks.sh` all pass with zero warnings.
