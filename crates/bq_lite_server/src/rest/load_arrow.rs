use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::catalog::{ColumnSchema, TableRef};
use bq_lite_core::io::arrow_codec::query_result_to_arrow_ipc;
use bq_lite_core::io::autodetect::{autodetect_csv_schema, autodetect_ndjson_schema};
use bq_lite_core::io::csv_json::{load_csv_with_options, load_ndjson_with_options, CsvLoadOptions};
use bq_lite_core::io::parquet_codec::load_parquet;
use bq_lite_core::{BqError, BqResult, QueryResult};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::state::{JobRecord, JobStatus, ServerState};
use crate::wire_json::{bq_error_response, decode_schema_fields};

use super::jobs_copy::write_rows_with_dispositions;
use super::jobs_queries::job_json;

#[derive(Debug, Deserialize)]
pub(super) struct UploadQuery {
    #[serde(rename = "uploadType")]
    pub upload_type: Option<String>,
    pub upload_id: Option<String>,
}

pub(super) async fn download_arrow_ipc(
    State(state): State<ServerState>,
    Path(job_id): Path<String>,
) -> Response {
    let Some(rec) = state.jobs.read().get(&job_id).cloned() else {
        return bq_error_response(&BqError::not_found(format!("Job '{job_id}' not found")));
    };
    if let Some(ref err) = rec.error {
        return bq_error_response(err);
    }
    let qr = rec.result.unwrap_or_default();
    match query_result_to_arrow_ipc(&qr) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apache.arrow.stream")],
            bytes,
        )
            .into_response(),
        Err(err) => bq_error_response(&err),
    }
}

/// Handles `POST /upload/bigquery/v2/projects/{projectId}/jobs` (multipart or resumable init).
pub(super) async fn upload_load_job(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if q.upload_type.as_deref() == Some("resumable") && q.upload_id.is_none() {
        let meta: JsonValue = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return bq_error_response(&BqError::invalid_argument(format!(
                    "Invalid resumable metadata JSON: {e}"
                )))
            }
        };
        let upload_id = format!("up_{}", uuid::Uuid::new_v4());
        state.record_resumable_upload(upload_id.clone(), meta);
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("127.0.0.1:9050");
        let scheme = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("http");
        let loc = format!(
            "{scheme}://{host}/upload/bigquery/v2/projects/{project_id}/jobs?uploadType=resumable&upload_id={upload_id}"
        );
        return (
            StatusCode::OK,
            [
                (header::LOCATION, loc),
                (
                    header::HeaderName::from_static("x-guploader-uploadid"),
                    upload_id,
                ),
            ],
        )
            .into_response();
    }

    if let Some(uid) = q.upload_id {
        return finish_resumable_upload(&state, &project_id, &uid, &body);
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (meta_json, payload) = match parse_upload_body(content_type, &body) {
        Ok(pair) => pair,
        Err(err) => return bq_error_response(&err),
    };
    match execute_load_job(&state, &project_id, &meta_json, &payload) {
        Ok(rec) => {
            let resp = job_json(&rec);
            state.record_job(rec);
            state.persist_if_enabled();
            Json(resp).into_response()
        }
        Err(err) => bq_error_response(&err),
    }
}

/// Handles `PUT /upload/bigquery/v2/projects/{projectId}/jobs?uploadType=resumable&upload_id=...`.
pub(super) async fn put_resumable_upload(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Response {
    let Some(uid) = q.upload_id else {
        return bq_error_response(&BqError::invalid_argument(
            "Resumable PUT requires upload_id query parameter",
        ));
    };
    finish_resumable_upload(&state, &project_id, &uid, &body)
}

fn finish_resumable_upload(
    state: &ServerState,
    project_id: &str,
    upload_id: &str,
    body: &[u8],
) -> Response {
    let Some(meta_json) = state.resumable_uploads.write().remove(upload_id) else {
        return bq_error_response(&BqError::not_found(format!(
            "Resumable upload session '{upload_id}' not found"
        )));
    };
    match execute_load_job(state, project_id, &meta_json, body) {
        Ok(rec) => {
            let resp = job_json(&rec);
            state.record_job(rec);
            state.persist_if_enabled();
            Json(resp).into_response()
        }
        Err(err) => bq_error_response(&err),
    }
}

fn parse_upload_body(content_type: &str, body: &[u8]) -> BqResult<(JsonValue, Vec<u8>)> {
    let boundary = extract_boundary(content_type).ok_or_else(|| {
        BqError::invalid_argument(
            "Upload endpoint requires multipart/related, multipart/form-data, or uploadType=resumable",
        )
    })?;
    let delim = format!("--{boundary}");
    let delim_bytes = delim.as_bytes();
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut pos = 0_usize;
    while let Some(rel_start) = find_subslice(body.get(pos..).unwrap_or(&[]), delim_bytes) {
        let seg_start = pos + rel_start + delim_bytes.len();
        let rest = body.get(seg_start..).unwrap_or(&[]);
        if rest.starts_with(b"--") {
            break;
        }
        let next_rel = find_subslice(rest, delim_bytes).unwrap_or(rest.len());
        let seg = rest.get(..next_rel).unwrap_or(&[]);
        if let Some(content) = extract_part_content(seg) {
            parts.push(content.to_vec());
        }
        pos = seg_start + next_rel;
    }
    let [meta_bytes, data_bytes, ..] = parts.as_slice() else {
        return Err(BqError::invalid_argument(
            "Multipart upload requires metadata JSON part and data part",
        ));
    };
    let meta: JsonValue = serde_json::from_slice(meta_bytes)
        .map_err(|e| BqError::invalid_argument(format!("Invalid upload metadata JSON: {e}")))?;
    Ok((meta, data_bytes.clone()))
}

fn extract_part_content(seg: &[u8]) -> Option<&[u8]> {
    let body = if let Some(idx) = find_subslice(seg, b"\r\n\r\n") {
        seg.get(idx + 4..)?
    } else {
        let idx = find_subslice(seg, b"\n\n")?;
        seg.get(idx + 2..)?
    };
    Some(
        body.strip_suffix(b"\r\n")
            .or_else(|| body.strip_suffix(b"\n"))
            .unwrap_or(body),
    )
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_boundary(ct: &str) -> Option<&str> {
    ct.split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))
        .map(|b| b.trim_matches('"'))
}

fn execute_load_job(
    state: &ServerState,
    project_id: &str,
    meta_json: &JsonValue,
    payload: &[u8],
) -> BqResult<JobRecord> {
    let load_cfg = meta_json
        .get("configuration")
        .and_then(|c| c.get("load"))
        .ok_or_else(|| BqError::invalid_argument("Missing configuration.load in upload job"))?;
    let dest = load_cfg
        .get("destinationTable")
        .ok_or_else(|| BqError::invalid_argument("Missing destinationTable"))?;
    let ds_id = dest
        .get("datasetId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Missing destinationTable.datasetId"))?;
    let tbl_id = dest
        .get("tableId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Missing destinationTable.tableId"))?;
    let dest_proj = dest
        .get("projectId")
        .and_then(JsonValue::as_str)
        .unwrap_or(project_id);

    let format = load_cfg
        .get("sourceFormat")
        .and_then(JsonValue::as_str)
        .unwrap_or("CSV");
    let write_disp = load_cfg
        .get("writeDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("WRITE_APPEND");
    let create_disp = load_cfg
        .get("createDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("CREATE_IF_NEEDED");

    let (base_tbl, _) = super::tabledata::split_partition_decorator(tbl_id);
    let tref = TableRef::new(dest_proj, ds_id, base_tbl);
    let mut eng = state.engine.write();
    let (columns, rows) = decode_load_payload(&eng, &tref, load_cfg, format, payload)?;
    let loaded_count = rows.len();
    write_rows_with_dispositions(&mut eng, &tref, &columns, rows, write_disp, create_disp)?;
    drop(eng);

    let job_id = meta_json
        .get("jobReference")
        .and_then(|r| r.get("jobId"))
        .and_then(JsonValue::as_str)
        .map_or_else(|| format!("job_{}", uuid::Uuid::new_v4()), str::to_owned);
    let location = meta_json
        .get("jobReference")
        .and_then(|r| r.get("location"))
        .and_then(JsonValue::as_str)
        .unwrap_or("US")
        .to_owned();
    Ok(JobRecord {
        job_id,
        project_id: project_id.to_owned(),
        location,
        state: JobStatus::Done,
        created_at: Utc::now(),
        result: Some(QueryResult::empty()),
        error: None,
        total_bytes_processed: 0,
        query: None,
        configuration: meta_json.get("configuration").cloned(),
        load_output_rows: Some(loaded_count),
    })
}

pub(super) type DecodedLoadPayload = (Vec<ColumnSchema>, Vec<Vec<bq_lite_core::BqValue>>);

pub(super) fn decode_load_payload(
    eng: &bq_lite_core::Engine,
    tref: &TableRef,
    load_cfg: &JsonValue,
    format: &str,
    payload: &[u8],
) -> BqResult<DecodedLoadPayload> {
    let autodetect = load_cfg
        .get("autodetect")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let ignore_unknown = load_cfg
        .get("ignoreUnknownValues")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let mut columns = resolve_load_columns_opt(eng, tref, load_cfg)?;

    match format.to_ascii_uppercase().as_str() {
        "CSV" => {
            let mut csv_opts = parse_csv_load_options(load_cfg);
            if columns.is_empty() && autodetect {
                let (detected, skip) = autodetect_csv_schema(payload, &csv_opts)?;
                columns = detected;
                csv_opts.skip_leading_rows = skip;
            }
            if columns.is_empty() {
                return Err(BqError::invalid_argument(
                    "Destination table missing and no schema.fields or autodetect=true provided",
                ));
            }
            let rows = load_csv_with_options(payload, &columns, &csv_opts)?;
            Ok((columns, rows))
        }
        "NEWLINE_DELIMITED_JSON" => {
            if columns.is_empty() && autodetect {
                columns = autodetect_ndjson_schema(payload)?;
            }
            if columns.is_empty() {
                return Err(BqError::invalid_argument(
                    "Destination table missing and no schema.fields or autodetect=true provided",
                ));
            }
            let rows = load_ndjson_with_options(payload, &columns, ignore_unknown || autodetect)?;
            Ok((columns, rows))
        }
        "PARQUET" => {
            let (pq_cols, pq_rows) = load_parquet(payload)?;
            if columns.is_empty() {
                columns = pq_cols;
            }
            Ok((columns, pq_rows))
        }
        other => Err(BqError::unimplemented(format!(
            "Load sourceFormat '{other}' is not implemented"
        ))),
    }
}

pub(super) fn parse_csv_load_options(load_cfg: &JsonValue) -> CsvLoadOptions {
    let skip_leading_rows = load_cfg
        .get("skipLeadingRows")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0);
    let field_delimiter = load_cfg
        .get("fieldDelimiter")
        .and_then(JsonValue::as_str)
        .and_then(|s| s.as_bytes().first().copied())
        .unwrap_or(b',');
    let quote = load_cfg
        .get("quote")
        .and_then(JsonValue::as_str)
        .and_then(|s| s.as_bytes().first().copied())
        .unwrap_or(b'"');
    let null_marker = load_cfg
        .get("nullMarker")
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let allow_jagged_rows = load_cfg
        .get("allowJaggedRows")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let ignore_unknown_values = load_cfg
        .get("ignoreUnknownValues")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    CsvLoadOptions {
        skip_leading_rows,
        field_delimiter,
        quote,
        null_marker,
        allow_jagged_rows,
        ignore_unknown_values,
    }
}

pub(super) fn resolve_load_columns_opt(
    eng: &bq_lite_core::Engine,
    tref: &TableRef,
    load_cfg: &JsonValue,
) -> BqResult<Vec<ColumnSchema>> {
    let canonical = eng.catalog().canonical_table_ref(tref);
    if let Ok(meta) = eng.catalog().get_table(&canonical) {
        return Ok(meta.columns.clone());
    }
    if let Some(fields) = load_cfg
        .get("schema")
        .and_then(|s| s.get("fields"))
        .and_then(JsonValue::as_array)
    {
        return decode_schema_fields(fields);
    }
    Ok(Vec::new())
}
