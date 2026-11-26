# BigQuery Error Parity Reference

This document is **code-verified** by `crates/bq_lite_core/tests/error_parity_doc.rs`.
Every `BqErrorKind` variant in `crates/bq_lite_core/src/error.rs` must appear in the table below with its exact HTTP status code, BigQuery JSON `reason`, gRPC status string, and `SAFE.` suppression behavior.

| Variant | HTTP Status | API Reason | API Status | SAFE. Suppressible | Notes |
|---|---|---|---|---|---|
| `Syntax` | `400` | `invalidQuery` | `INVALID_ARGUMENT` | `false` | Lexer or parser syntax error with `SourceSpan` |
| `Query` | `400` | `invalidQuery` | `INVALID_ARGUMENT` | `false` | Binder, unknown identifier, type mismatch, or invalid `ARRAY<ARRAY<T>>` |
| `Semantic` | `400` | `invalidQuery` | `INVALID_ARGUMENT` | `true` | Runtime value error (div-by-zero, overflow, bad cast, out-of-bounds index) |
| `InvalidArgument` | `400` | `invalid` | `INVALID_ARGUMENT` | `false` | Invalid API request, parameter, or configuration value |
| `PermissionDenied` | `403` | `accessDenied` | `PERMISSION_DENIED` | `false` | Policy/RLS or read-only/permission failure |
| `NotFound` | `404` | `notFound` | `NOT_FOUND` | `false` | Missing project, dataset, table, routine, or job |
| `AlreadyExists` | `409` | `duplicate` | `ALREADY_EXISTS` | `false` | Duplicate dataset, table, routine, or job ID |
| `Unimplemented` | `501` | `notImplemented` | `UNIMPLEMENTED` | `false` | Valid BigQuery syntax/feature not yet implemented |
| `Internal` | `500` | `internalError` | `INTERNAL` | `false` | Unexpected internal storage or serialization failure |
