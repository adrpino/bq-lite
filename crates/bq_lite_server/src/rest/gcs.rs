use std::path::PathBuf;

use bq_lite_core::io::csv_json::{export_csv, export_ndjson};
use bq_lite_core::io::parquet_codec::export_parquet;
use bq_lite_core::{BqError, BqResult, QueryColumn, QueryResult};
use chrono::Utc;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::state::{JobRecord, JobStatus, ServerState};

use super::jobs_copy::{parse_table_ref, write_rows_with_dispositions};
use super::load_arrow::decode_load_payload;

pub(super) fn resolve_uri_path(state: &ServerState, uri: &str) -> BqResult<PathBuf> {
    let base = state.data_dir.as_ref().map_or_else(
        || {
            std::env::var("BQ_LITE_GCS_DIR")
                .map_or_else(|_| std::env::temp_dir().join("bq-lite-gcs"), PathBuf::from)
        },
        |d| d.join("gcs"),
    );
    let Some(rel) = uri.strip_prefix("gs://") else {
        if let Some(file) = uri.strip_prefix("file://") {
            return Ok(PathBuf::from(file));
        }
        return Ok(PathBuf::from(uri));
    };
    if rel.contains("..") || rel.is_empty() {
        return Err(BqError::invalid_argument(format!("Invalid URI: '{uri}'")));
    }
    Ok(base.join(rel))
}

async fn emulator_http_request(
    emu_host: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let trimmed = emu_host.trim().trim_end_matches('/');
    let host_and_prefix = if let Some(rest) = trimmed.strip_prefix("http://") {
        rest
    } else if trimmed.contains("://") {
        return None;
    } else {
        trimmed
    };
    let (authority, prefix) = match host_and_prefix.split_once('/') {
        Some((auth, p)) => (auth, format!("/{}", p.trim_matches('/'))),
        None => (host_and_prefix, String::new()),
    };
    let full_path = if prefix.is_empty() || prefix == "/" {
        path_and_query.to_owned()
    } else {
        format!("{prefix}{path_and_query}")
    };
    let mut stream = TcpStream::connect(authority).await.ok()?;
    let payload = body.unwrap_or(&[]);
    let mut req =
        format!("{method} {full_path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    if !payload.is_empty() || method == "POST" {
        use std::fmt::Write as _;
        let _ = write!(req, "Content-Length: {}\r\n", payload.len());
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    if !payload.is_empty() {
        stream.write_all(payload).await.ok()?;
    }

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.ok()?;
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(raw.get(..header_end)?).ok()?;
    let status_line = head.lines().next()?;
    let code: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    if !(200..300).contains(&code) {
        return None;
    }
    let body_bytes = raw.get(header_end + 4..)?;
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked_body(body_bytes)
    } else {
        Some(body_bytes.to_vec())
    }
}

fn decode_chunked_body(mut bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let crlf = bytes.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(bytes.get(..crlf)?).ok()?;
        let hex_part = size_line.split(';').next()?.trim();
        let size = usize::from_str_radix(hex_part, 16).ok()?;
        bytes = bytes.get(crlf + 2..)?;
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(bytes.get(..size)?);
        bytes = bytes.get(size + 2..)?;
    }
}

async fn read_uri_bytes(state: &ServerState, uri: &str) -> BqResult<Vec<u8>> {
    if let Some(rel) = uri.strip_prefix("gs://") {
        if let Some(ref emu_host) = state.storage_emulator_host {
            if let Some((bucket, obj)) = rel.split_once('/') {
                let path = format!("/download/storage/v1/b/{bucket}/o/{obj}?alt=media");
                if let Some(bytes) = emulator_http_request(emu_host, "GET", &path, None).await {
                    return Ok(bytes);
                }
            }
        }
    }
    let path = resolve_uri_path(state, uri)?;
    std::fs::read(&path)
        .map_err(|e| BqError::not_found(format!("Failed to read sourceUri '{uri}': {e}")))
}

async fn write_uri_bytes(state: &ServerState, uri: &str, payload: &[u8]) -> BqResult<()> {
    if let Some(rel) = uri.strip_prefix("gs://") {
        if let Some(ref emu_host) = state.storage_emulator_host {
            if let Some((bucket, obj)) = rel.split_once('/') {
                let path = format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={obj}");
                if emulator_http_request(emu_host, "POST", &path, Some(payload))
                    .await
                    .is_some()
                {
                    return Ok(());
                }
            }
        }
    }
    let path = resolve_uri_path(state, uri)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| BqError::internal(format!("Failed to create GCS dir: {e}")))?;
        }
    }
    std::fs::write(&path, payload)
        .map_err(|e| BqError::internal(format!("Failed to write extract file: {e}")))
}

pub(super) async fn execute_uri_load_job(
    state: &ServerState,
    project_id: &str,
    job_id: String,
    location: String,
    load_cfg: &JsonValue,
) -> BqResult<JobRecord> {
    let uris = load_cfg
        .get("sourceUris")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| BqError::invalid_argument("Load job requires sourceUris"))?;
    let dest = load_cfg
        .get("destinationTable")
        .ok_or_else(|| BqError::invalid_argument("Missing destinationTable"))?;
    let tref = parse_table_ref(dest, project_id)?;
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

    let mut payloads = Vec::with_capacity(uris.len());
    for u in uris {
        let uri_str = u
            .as_str()
            .ok_or_else(|| BqError::invalid_argument("sourceUris entries must be strings"))?;
        payloads.push(read_uri_bytes(state, uri_str).await?);
    }

    let mut eng = state.engine.write();
    let mut final_cols = Vec::new();
    let mut all_rows = Vec::new();
    for bytes in &payloads {
        let (cols, rows) = decode_load_payload(&eng, &tref, load_cfg, format, bytes)?;
        if final_cols.is_empty() {
            final_cols = cols;
        }
        all_rows.extend(rows);
    }
    let loaded_count = all_rows.len();
    write_rows_with_dispositions(
        &mut eng,
        &tref,
        &final_cols,
        all_rows,
        write_disp,
        create_disp,
    )?;
    drop(eng);
    state.persist_if_enabled();

    Ok(JobRecord {
        job_id,
        project_id: project_id.to_owned(),
        location,
        state: JobStatus::Done,
        created_at: Utc::now(),
        result: Some(std::sync::Arc::new(QueryResult::empty())),
        error: None,
        total_bytes_processed: 0,
        query: None,
        configuration: Some(json!({ "load": load_cfg })),
        load_output_rows: Some(loaded_count),
    })
}

pub(super) async fn execute_extract_job(
    state: &ServerState,
    project_id: &str,
    job_id: String,
    location: String,
    extract_cfg: &JsonValue,
) -> BqResult<JobRecord> {
    let src = extract_cfg
        .get("sourceTable")
        .ok_or_else(|| BqError::invalid_argument("Extract job missing sourceTable"))?;
    let tref = parse_table_ref(src, project_id)?;
    let dest_uri = extract_cfg
        .get("destinationUri")
        .and_then(JsonValue::as_str)
        .or_else(|| {
            extract_cfg
                .get("destinationUris")
                .and_then(JsonValue::as_array)
                .and_then(|a| a.first())
                .and_then(JsonValue::as_str)
        })
        .ok_or_else(|| BqError::invalid_argument("Extract job requires destinationUris"))?;
    let format = extract_cfg
        .get("destinationFormat")
        .and_then(JsonValue::as_str)
        .unwrap_or("CSV");

    let (cols, rows) = {
        let eng = state.engine.read();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        let meta = eng.catalog().get_table(&canonical)?.clone();
        let rows = eng.storage().read_rows(&canonical);
        drop(eng);
        (meta.columns, rows)
    };
    let payload = match format.to_ascii_uppercase().as_str() {
        "CSV" => export_csv(&cols, &rows)?,
        "NEWLINE_DELIMITED_JSON" | "JSON" => export_ndjson(&cols, &rows)?,
        "PARQUET" => {
            let qcols: Vec<QueryColumn> = cols
                .iter()
                .map(|c| QueryColumn::new(&c.name, c.ty.clone()))
                .collect();
            export_parquet(&QueryResult::new(qcols, rows))?
        }
        other => {
            return Err(BqError::unimplemented(format!(
                "Extract destinationFormat '{other}' is not implemented"
            )))
        }
    };
    let expanded = dest_uri.replace('*', "000000000000");
    write_uri_bytes(state, &expanded, &payload).await?;

    Ok(JobRecord {
        job_id,
        project_id: project_id.to_owned(),
        location,
        state: JobStatus::Done,
        created_at: Utc::now(),
        result: Some(std::sync::Arc::new(QueryResult::empty())),
        error: None,
        total_bytes_processed: 0,
        query: None,
        configuration: Some(json!({ "extract": extract_cfg })),
        load_output_rows: None,
    })
}
