use std::path::PathBuf;

use bq_lite_core::io::csv_json::{export_csv, export_ndjson};
use bq_lite_core::io::parquet_codec::export_parquet;
use bq_lite_core::{BqError, BqResult, QueryColumn, QueryResult};
use chrono::Utc;
use serde_json::{json, Value as JsonValue};

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

async fn read_uri_bytes(state: &ServerState, uri: &str) -> BqResult<Vec<u8>> {
    if let Some(rel) = uri.strip_prefix("gs://") {
        if let Some(ref emu_host) = state.storage_emulator_host {
            if let Some((bucket, obj)) = rel.split_once('/') {
                let base = emu_host.trim_end_matches('/');
                let base_url = if base.starts_with("http://") || base.starts_with("https://") {
                    base.to_owned()
                } else {
                    format!("http://{base}")
                };
                let url = format!("{base_url}/download/storage/v1/b/{bucket}/o/{obj}?alt=media");
                if let Ok(resp) = reqwest::Client::new().get(&url).send().await {
                    if resp.status().is_success() {
                        if let Ok(bytes) = resp.bytes().await {
                            return Ok(bytes.to_vec());
                        }
                    }
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
                let base = emu_host.trim_end_matches('/');
                let base_url = if base.starts_with("http://") || base.starts_with("https://") {
                    base.to_owned()
                } else {
                    format!("http://{base}")
                };
                let url = format!(
                    "{base_url}/upload/storage/v1/b/{bucket}/o?uploadType=media&name={obj}"
                );
                if let Ok(resp) = reqwest::Client::new()
                    .post(&url)
                    .body(payload.to_vec())
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        return Ok(());
                    }
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
        result: Some(QueryResult::empty()),
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
        result: Some(QueryResult::empty()),
        error: None,
        total_bytes_processed: 0,
        query: None,
        configuration: Some(json!({ "extract": extract_cfg })),
        load_output_rows: None,
    })
}
