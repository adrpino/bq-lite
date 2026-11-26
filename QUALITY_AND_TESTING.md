# bq-lite — Code Structure, Linting, Clippy & Testing Plan

This document defines the **hard engineering guardrails** for `bq-lite`. Every autonomous agent and contributor must follow these rules and pass the full quality gate before marking any task or phase complete.

The explicit goal is to prevent common AI-agent failure modes in Rust database engines:
- 3,000-line monolithic `parser.rs` / `evaluator.rs` files
- `.unwrap()` / `.expect()` / slice indexing panics in query parsing or execution
- Lint suppression hacks (`#[allow(clippy::all)]`, `#[allow(dead_code)]`, `#[allow(warnings)]`)
- Silent `todo!()` stubs or wrong fallback values instead of explicit `BqError::Unimplemented`
- Shallow tests that only check `res.is_ok()` without asserting types, `NULL`s, errors, or wire format

---

## 1. Compiler, Clippy & Formatter Configuration

### 1.1 Workspace Lints (Root `Cargo.toml`)
Every crate in the workspace **must** inherit workspace lints via `[lints] workspace = true`.

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"
unused_results = "allow" # noisy on builders; enforce via #[must_use] on Results
elided_lifetimes_in_paths = "warn"

[workspace.lints.clippy]
# Lint groups (priority -1 so individual overrides take precedence)
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }

# Zero-panic policy in production/library code (relaxed for #[cfg(test)] in clippy.toml)
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
indexing_slicing = "deny"
unreachable = "warn"

# Anti-rot & anti-hack rules
allow_attributes = "deny"                  # Force #[expect(..., reason = "...")]
allow_attributes_without_reason = "deny"
dbg_macro = "deny"
print_stdout = "deny"                      # Use `tracing` in server; zero stdout in core
print_stderr = "deny"
empty_drop = "deny"
exit = "deny"
mem_forget = "deny"

# Numeric / SQL correctness
float_cmp = "deny"
lossy_float_literal = "deny"
cast_possible_truncation = "warn"
cast_sign_loss = "warn"
cast_possible_wrap = "warn"

# Structural complexity (thresholds configured in clippy.toml)
too_many_lines = "deny"
cognitive_complexity = "deny"
too_many_arguments = "deny"
large_enum_variant = "deny"
result_large_err = "deny"
wildcard_enum_match_arm = "warn"           # Forces exhaustive matches when adding AST/BqType variants
rest_pat_in_fully_bound_structs = "deny"

# Pragmatic pedantic opt-outs (avoid churn, keep real signal)
module_name_repetitions = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
must_use_candidate = "allow"
```

### 1.2 `clippy.toml` (Repository Root)
```toml
# Force decomposition into small, readable, testable functions
too-many-lines-threshold = 80
cognitive-complexity-threshold = 20
too-many-arguments-threshold = 6
type-complexity-threshold = 200
enum-variant-size-threshold = 128

# Allow convenient assertions/unwraps inside test code ONLY
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-indexing-slicing-in-tests = true
allow-dbg-in-tests = true
allow-print-in-tests = true

# Prevent accidental blocking or unsafe patterns
disallowed-methods = [
    { path = "std::env::set_var", reason = "UB in multi-threaded tests; pass config structs instead" },
    { path = "std::env::remove_var", reason = "UB in multi-threaded tests; pass config structs instead" },
]
disallowed-types = [
    { path = "std::sync::Mutex", reason = "Use parking_lot::Mutex (no poisoning) or explicit state design" },
    { path = "std::sync::RwLock", reason = "Use parking_lot::RwLock (no poisoning)" },
]
```

### 1.3 `rustfmt.toml` (Repository Root)
```toml
edition = "2021"
max_width = 100
tab_spaces = 4
use_field_init_shorthand = true
use_try_shorthand = true
reorder_imports = true
reorder_modules = true
```

### 1.4 Dependency Hygiene (`deny.toml` via `cargo-deny` + `cargo-machete`)
- Ban `sqlparser`, `datafusion`, `rusqlite`, `duckdb`, and `tokio` inside `bq-lite-core`.
- Enforce permissive licenses (`MIT`, `Apache-2.0`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`) and reject duplicate major crate versions where avoidable.
- Run `cargo machete` in CI so agents don't leave unused crates in `Cargo.toml`.

---

## 2. Code Structure & Architectural Invariants

### 2.1 File & Function Size Budgets (Enforced in CI)
Agents naturally bloat files until they become unmaintainable. Enforce hard limits via `scripts/check-structure.sh`:
- **Max lines per non-test `.rs` file:** **500 lines** (warn at 350, fail CI at 500).
- **Max lines per function:** **80 lines** (enforced by Clippy `too_many_lines`).
- **Max cognitive complexity per function:** **20** (enforced by Clippy `cognitive_complexity`).
- **When a file approaches 350 lines:** Split by cohesive responsibility into a submodule directory (`foo/mod.rs`, `foo/bar.rs`, `foo/baz.rs`), never by arbitrary `_part1.rs` / `_part2.rs` numbering.

### 2.2 Strict Module Dependency DAG (`bq_lite_core`)
Enforce unidirectional layering (verified by `scripts/check-structure.sh`):

```text
error.rs          (no internal deps)
   ▲
types/            (depends on: error)
   ▲
ast/              (depends on: types)
   ▲
parser/           (depends on: ast, types, error — NEVER catalog/storage/executor)
   ▲
catalog/          (depends on: types, ast, error — CoW Arc registries only)
   ▲
storage/          (depends on: types, catalog, error — pure data chunks & redb)
   ▲
io/               (depends on: types, storage, error — Arrow/Parquet/CSV/NDJSON codecs)
   ▲
executor/         (depends on: all of the above — NEVER tokio/axum/tonic/http)
```

Server layering (`bq_lite_server`):
- Only interacts with `bq_lite_core` through its **public API** (`Engine`, `Session`, `QueryResult`, `BqType`, `BqValue`, `BqError`, `CatalogSnapshot`).
- `wire_json.rs` is the **sole** place allowed to serialize BigQuery REST `f`/`v` JSON rows and schemas.

### 2.3 Typed Errors & Exhaustive Matching
- All fallible operations return `Result<T, Box<BqError>>` (boxed to satisfy `result_large_err`).
- `BqError` carries structured fields:
  - `kind: BqErrorKind` (`Syntax`, `Query`, `Semantic`, `NotFound`, `AlreadyExists`, `InvalidArgument`, `PermissionDenied`, `Unimplemented`, `Internal`)
  - `http_status: u16` (`400`, `403`, `404`, `409`, `501`, `500`)
  - `reason: &'static str` (BigQuery API reason: `"invalidQuery"`, `"notFound"`, `"duplicate"`, `"notImplemented"`, etc.)
  - `span: Option<SourceSpan>` (byte offset / line / col for syntax & binding errors)
  - `message: String`
- **`SAFE.` rule:** Only `BqErrorKind::Semantic` is ever caught by `SAFE.` calls and converted to `BqValue::Null`. `Query`/`Syntax`/`Unimplemented` errors must always propagate.
- **No stringly-typed error matching in code or tests:** Match on `BqErrorKind` and `reason`, never substring-matching fragile English prose unless testing diagnostic formatting specifically.

### 2.4 Banned Agent Anti-Patterns (Checked by `scripts/check-structure.sh`)
CI fails if any non-test `.rs` file contains:
- `#[allow(clippy::all)]`, `#[allow(warnings)]`, `#![allow(...)]`
- `#[allow(dead_code)]` (unless replaced with `#[expect(dead_code, reason = "...")]` and linked to an active phase)
- `todo!(`, `unimplemented!(`, `.unwrap()`, `.expect(` outside `#[cfg(test)]`
- `#[ignore]` in test files without an explicit reason string: `#[ignore = "requires live BigQuery Sandbox"]`
- `tokio`, `axum`, `tonic`, or `reqwest` imported inside `crates/bq_lite_core/src/`

---

## 3. Multi-Layer Testing Plan

Every feature must be verified at the lowest layer where its logic lives **plus** the integration boundary where users consume it.

### Layer 1 — Fine-Grained Module Unit Tests (`#[cfg(test)]`)
Colocated inside each module (or `tests/` sibling for larger modules):

| Subsystem | Mandatory Unit Test Coverage |
|---|---|
| **`types/`** | All `BqType` variants; `ARRAY<ARRAY<T>>` rejection; `NUMERIC`/`BIGNUMERIC` rounding, overflow, scale limits; `TIMESTAMP`/`DATETIME`/`DATE`/`TIME` microsecond & leap-year boundaries; supertype coercion matrix (`INT64 -> NUMERIC -> BIGNUMERIC -> FLOAT64`, untyped `NULL`, positional `STRUCT` coercion). |
| **`parser/`** | Every token kind (single-token 3-part backticks `` `p.d.t` ``, raw/byte/triple-quoted strings, `#`/`--`/`/*` comments, `@params`, `?`); operator precedence; every expression/query/DDL/DML/script AST node; **malformed SQL produces `BqErrorKind::Syntax` with accurate `SourceSpan` and NEVER panics**. |
| **`catalog/` & `storage/`** | Case sensitivity (project/dataset/table vs column/struct field); `fork_isolated()` CoW isolation (mutations in fork never bleed into parent or sibling forks); time-travel snapshot retention & lookup; `redb` persistence round-trip across reopen. |
| **`executor/scalar/`** | Each function module gets its own test file verifying: normal values, `NULL` propagation, boundary/edge values (empty string/array, UTF-8 multibyte, timezone DST transitions, `NaN`/`Inf`), error cases (`Semantic` vs `Query`), and `SAFE.<func>(...)` null-on-error behavior. |
| **`executor/query/`, `dml/`, `ddl/`, `script/`** | `UNNEST` + `WITH OFFSET` (including empty & `NULL` arrays), `* EXCEPT`/`REPLACE`, `QUALIFY`, `WINDOW` frames (`ROWS`/`RANGE`), `GROUP BY ALL`/`ROLLUP`/`CUBE`/`GROUPING SETS`, `MERGE` (matched/not matched by target/source + duplicate match error), deep-`STRUCT` `UPDATE`, `BEGIN..EXCEPTION`, `EXECUTE IMMEDIATE`. |
| **`wire_json.rs`** | Exact JSON snapshot tests for every `BqType` in BigQuery `f`/`v` format (`INT64`/`NUMERIC`/`BOOL` as strings, `TIMESTAMP` epoch seconds, `BYTES` base64, empty `ARRAY` as `[]`, nested `STRUCT` inside `ARRAY`, `NULL` fields) and Google JSON error envelopes. |

#### Anti-Trash-Test Rules (Mandatory for All Unit Tests)
1. **No `assert!(res.is_ok())`-only tests:** Every query test must assert **exact schema (column names + `BqType`s)** and **exact row values** via a shared `assert_query_eq!` helper.
2. **No mock-of-the-thing-under-test:** Test real execution through `Engine::new()`, not fake stubs.
3. **Every new feature PR/commit must include at least 4 test categories:**
   - Happy path (typical realistic query)
   - `NULL` & empty-input path (0 rows, `NULL` operands, empty `ARRAY`)
   - Edge / type-coercion path (nested `STRUCT`/`ARRAY`, timezone, boundary numeric)
   - Error & `SAFE.` path (verify exact `BqErrorKind`, HTTP code, and `SAFE.` conversion where applicable)

### Layer 2 — Declarative SQL Test Suite (`sqllogictest` / Golden Cases)
- Place readable `.slt` (or structured `.sql` + expected `.json`) suites under `crates/bq_lite_core/tests/sql_suite/` organized by topic:
  - `01_types_and_casts.slt`
  - `02_arrays_structs_unnest.slt`
  - `03_joins_ctes_subqueries.slt`
  - `04_aggregates_and_windows.slt`
  - `05_dml_and_merge.slt`
  - `06_ddl_views_snapshots.slt`
  - `07_scripting_and_procedures.slt`
  - `08_information_schema.slt`
  - `09_functions_*.slt`
- Driven by a single fast Rust test runner (`cargo test -p bq-lite-core --test sql_suite`).
- Adding regression tests for any bug becomes a 5-line `.slt` entry with exact expected types & rows.

### Layer 3 — Property-Based Testing (`proptest`)
Located in `crates/bq_lite_core/tests/proptest_*.rs`:
1. **Parser Fuzz / No-Panic Invariant:** Arbitrary UTF-8 strings and grammar-mutated SQL strings must always return `Ok(_)` or `Err(BqError)` — **never panic or stack-overflow** (enforce max recursion depth in parser!).
2. **Value Round-Trip Invariants:**
   - Arbitrary `BqValue` (including nested `Struct` and `Array`) -> `wire_json` `f`/`v` -> decoded back matches original value & schema.
   - Arbitrary `BqValue` -> Arrow `ArrayRef` -> `BqValue` round-trips losslessly.
   - Arbitrary `Catalog` + tables -> `redb` persist -> reload equals in-memory state.
3. **CoW Isolation Invariant:** Random interleaved DDL/DML operations across parent + `N` `fork_isolated()` engines never corrupt or leak state between forks.
4. **Numeric Arithmetic Invariants:** `NUMERIC`/`BIGNUMERIC` addition/subtraction/multiplication/division match exact rational math or return `BqErrorKind::Semantic` on overflow (never silent wraparound).

### Layer 4 — Parity & Doc Self-Enforcement Tests
Mirroring `tsql-lite`'s `tsql_function_parity_doc.rs`:
1. **`googlesql_function_parity_doc.rs`:** Parses `docs/googlesql-function-parity.md` and asserts **bidirectional exact equality** with the live `scalar`, `aggregate`, `window`, and `tvf` registries. Any undocumented function or stale doc entry fails `cargo test`.
2. **`error_parity_doc.rs`:** Verifies every `BqErrorKind` maps to a valid HTTP status + BigQuery `reason` documented in `docs/error-parity.md`.
3. **`farm_fingerprint_golden.rs`:** Verifies `FARM_FINGERPRINT` against a table of golden outputs captured directly from live Google BigQuery.

### Layer 5 — Server Contract Tests (`crates/bq_lite_server/tests/`)
Spins up `bq_lite_server` on an ephemeral `127.0.0.1:0` port (no hardcoded ports, fully parallel-safe):
- REST v2 CRUD: projects, datasets, tables, routines, views, `tabledata.list` pagination, `tabledata.insertAll` (`insertId` dedup, `skipInvalidRows`, `ignoreUnknownValues`).
- Query jobs: sync `POST /queries`, async `POST /jobs` -> `GET /jobs/{id}` -> `GET /queries/{id}` with `maxResults`/`pageToken`, `dryRun: true` returning schema + `totalBytesProcessed` without executing mutations, `POST /jobs/{id}/cancel`, named & positional `queryParameters`.
- Load & Extract jobs: multipart + resumable `/upload/bigquery/v2/...` and `gs://` local root for CSV, NDJSON, and Parquet.
- gRPC Storage API: `BigQueryRead` returning valid Arrow IPC batches matching REST query output; `BigQueryWrite` `AppendRows` + `BatchCommitWriteStreams`.
- Reset & persistence: `POST /_bqlite/reset` clears state cleanly; server restart with `--data-dir` preserves datasets, tables, and rows.

### Layer 6 — Live Official SDK Verification (`scripts/test-sdks.sh`)
Before shipping Phase 6+, run automated end-to-end suites against a locally launched `bq-lite-server` using the **real, unmodified official SDKs**:
- Python: `google-cloud-bigquery` + `google-cloud-bigquery-storage` (`.to_dataframe(bqstorage_client=...)`)
- Go: `cloud.google.com/go/bigquery`
- Node.js: `@google-cloud/bigquery`
- Rust: `gcp-bigquery-client`

### Layer 7 — Code Coverage & Optional Mutation Testing
- Track coverage with `cargo llvm-cov --workspace --all-features --summary-only`:
  - **`bq_lite_core` overall:** >= **85%** line coverage
  - **`parser/`, `types/`, `wire_json.rs`:** >= **90%** line coverage
  - **`bq_lite_server`:** >= **80%** line coverage
- Periodically run `cargo mutants` on high-risk modules (`types/coercion.rs`, `wire_json.rs`, `executor/dml/merge.rs`) to verify tests actually catch logic bugs.

---

## 4. Automated Quality Gate (`scripts/quality-gate.sh`)

Create this script in **Phase 0** and run it in CI and before every commit. An agent is **not done** until this exits `0`:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "==> 1. Formatting check"
cargo fmt --all -- --check

echo "==> 2. Structure & anti-pattern check"
./scripts/check-structure.sh

echo "==> 3. Clippy (all targets, all features, deny warnings)"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> 4. Clippy (bq-lite-core pure in-memory / no-default-features)"
cargo clippy -p bq-lite-core --no-default-features -- -D warnings

echo "==> 5. Unused dependencies & dependency policy"
command -v cargo-machete >/dev/null && cargo machete --with-metadata
command -v cargo-deny >/dev/null && cargo deny check bans licenses

echo "==> 6. Full workspace test suite (including parity doc & proptests)"
cargo test --workspace --all-features

echo "==> 7. Pure in-memory core test suite"
cargo test -p bq-lite-core --no-default-features

echo "==> Quality gate PASSED"
```

### What `scripts/check-structure.sh` Enforces
1. **File line limit:** Fail if any `.rs` file under `crates/*/src/` exceeds **500 lines** (excluding `#[cfg(test)]` modules at the bottom of a file, or strictly 500 total — strictly 500 total keeps tests in dedicated files once modules grow).
2. **Zero forbidden suppressions:** Fail if `rg` matches `#!?\[allow\((clippy::all|warnings|dead_code|unused)\)\]` in `crates/*/src/`.
3. **Zero forbidden macros in prod code:** Fail if `rg` matches `\b(todo|unimplemented)!\(` in `crates/*/src/` (outside test code).
4. **Layering check:**
   - Fail if `crates/bq_lite_core/src/` imports `tokio`, `axum`, `tonic`, `hyper`, or `reqwest`.
   - Fail if `crates/bq_lite_core/src/parser/` imports `crate::catalog`, `crate::storage`, or `crate::executor`.
   - Fail if `crates/bq_lite_core/src/types/` imports `crate::ast`, `crate::parser`, `crate::catalog`, `crate::storage`, or `crate::executor`.
5. **No unexplained `#[ignore]`:** Fail if `rg` finds bare `#[ignore]` without a `= "reason"` string.

---

## 5. Agent Workflow Checklist (For `AGENTS.md`)

When Phase 0 initializes the repo, copy these hard rules into `bq-lite/AGENTS.md`:
1. **Read before writing:** Inspect existing module layout and `docs/compatibility-matrix.md` before adding code.
2. **Design small modules:** If an edit would push a source file past 350 lines, split it cleanly *before* adding more code.
3. **No shortcuts:** Never silence Clippy by weakening `Cargo.toml` lints, adding `#[allow(...)]`, or dropping `.unwrap()` into non-test code.
4. **Tests are part of the implementation:** Every feature includes unit tests (happy + `NULL`/empty + edge + error/`SAFE.`), `.slt` golden coverage, and parity doc updates in the same change.
5. **Run `./scripts/quality-gate.sh` before finishing:** Never report a task complete with failing formatting, structure checks, Clippy warnings, or tests.
