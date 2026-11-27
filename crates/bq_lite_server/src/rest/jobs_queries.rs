use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::{BqError, QueryResult};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::state::{JobRecord, JobStatus, ServerState};
use crate::wire_json::{bq_error_response, encode_rows, encode_schema};

use super::datasets::PaginationParams;
use super::gcs::{execute_extract_job, execute_uri_load_job};
use super::jobs_copy::{
    apply_query_destination, dry_run_sql, execute_copy_job, run_sql_on_engine_ref,
};
use super::params::parse_query_params;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListJobsQuery {
    pub max_results: Option<usize>,
    pub page_token: Option<String>,
    pub state_filter: Option<String>,
    pub min_creation_time: Option<i64>,
    pub max_creation_time: Option<i64>,
}

pub(super) async fn sync_query(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let Some(sql) = body.get("query").and_then(JsonValue::as_str) else {
        return bq_error_response(&BqError::invalid_argument("Missing 'query' field"));
    };
    if body.get("useLegacySql").and_then(JsonValue::as_bool) == Some(true) {
        return bq_error_response(&BqError::invalid_argument(
            "Legacy SQL is not supported; set useLegacySql=false",
        ));
    }
    let dry_run = body
        .get("dryRun")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let max_results = body
        .get("maxResults")
        .and_then(JsonValue::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(10_000);
    let default_ds = body
        .get("defaultDataset")
        .and_then(|d| d.get("datasetId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);

    let (named_params, pos_params) = match parse_query_params(&body) {
        Ok(p) => p,
        Err(err) => return bq_error_response(&err),
    };

    if dry_run {
        return match dry_run_sql(&state, sql, default_ds, &named_params, &pos_params) {
            Ok(qr) => {
                let job_id = format!("job_{}", uuid::Uuid::new_v4());
                Json(json!({
                    "kind": "bigquery#queryResponse",
                    "jobReference": { "projectId": project_id, "jobId": job_id, "location": "US" },
                    "schema": encode_schema(&qr.columns),
                    "jobComplete": true,
                    "totalBytesProcessed": "0",
                    "cacheHit": false,
                }))
                .into_response()
            }
            Err(err) => bq_error_response(&err),
        };
    }

    let qr = {
        let mut eng = state.engine.write();
        match run_sql_on_engine_ref(&mut eng, sql, default_ds, &named_params, &pos_params) {
            Ok(r) => r,
            Err(err) => return bq_error_response(&err),
        }
    };
    state.persist_if_enabled();
    build_sync_query_response(&state, &project_id, sql, qr, max_results)
}

fn build_sync_query_response(
    state: &ServerState,
    project_id: &str,
    sql: &str,
    qr: QueryResult,
    max_results: usize,
) -> Response {
    let job_id = format!("job_{}", uuid::Uuid::new_v4());
    let total_rows = qr.rows.len();
    let page_end = max_results.min(total_rows);
    let encoded_rows = encode_rows(&qr.columns, qr.rows.get(..page_end).unwrap_or(&[]));
    let schema = encode_schema(&qr.columns);
    let dml_affected = qr.rows_affected;

    state.record_job(JobRecord {
        job_id: job_id.clone(),
        project_id: project_id.to_owned(),
        location: "US".to_owned(),
        state: JobStatus::Done,
        created_at: Utc::now(),
        result: Some(std::sync::Arc::new(qr)),
        error: None,
        total_bytes_processed: 0,
        query: Some(sql.to_owned()),
        configuration: None,
        load_output_rows: None,
    });

    let mut resp = json!({
        "kind": "bigquery#queryResponse",
        "jobReference": { "projectId": project_id, "jobId": job_id, "location": "US" },
        "schema": schema,
        "rows": encoded_rows,
        "totalRows": total_rows.to_string(),
        "jobComplete": true,
        "totalBytesProcessed": "0",
        "numDmlAffectedRows": dml_affected.to_string(),
    });
    if page_end < total_rows {
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("pageToken".to_owned(), json!(page_end.to_string()));
        }
    }
    Json(resp).into_response()
}

pub(super) async fn list_jobs(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Query(q): Query<ListJobsQuery>,
) -> Response {
    let all_jobs: Vec<JsonValue> = {
        let map = state.jobs.read();
        map.values()
            .filter(|j| j.project_id == project_id)
            .filter(|j| {
                q.state_filter
                    .as_deref()
                    .is_none_or(|sf| j.state.as_str().eq_ignore_ascii_case(sf))
            })
            .filter(|j| {
                let ms = j.created_at.timestamp_millis();
                q.min_creation_time.is_none_or(|min| ms >= min)
                    && q.max_creation_time.is_none_or(|max| ms <= max)
            })
            .map(job_json)
            .collect()
    };
    let total = all_jobs.len();
    let start = q
        .page_token
        .as_deref()
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total);
    let max_res = q.max_results.unwrap_or(10_000);
    let end = start.saturating_add(max_res).min(total);
    let page = all_jobs.get(start..end).unwrap_or(&[]).to_vec();
    let mut out = json!({
        "kind": "bigquery#jobList",
        "jobs": page,
    });
    if end < total {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("nextPageToken".to_owned(), json!(end.to_string()));
        }
    }
    Json(out).into_response()
}

pub(super) async fn insert_job(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let job_id = body
        .get("jobReference")
        .and_then(|r| r.get("jobId"))
        .and_then(JsonValue::as_str)
        .map_or_else(|| format!("job_{}", uuid::Uuid::new_v4()), str::to_owned);
    let location = body
        .get("jobReference")
        .and_then(|r| r.get("location"))
        .and_then(JsonValue::as_str)
        .unwrap_or("US")
        .to_owned();

    if let Some(copy_cfg) = body.get("configuration").and_then(|c| c.get("copy")) {
        return match execute_copy_job(&state, &project_id, job_id, location, copy_cfg) {
            Ok(rec) => {
                let resp = job_json(&rec);
                state.record_job(rec);
                Json(resp).into_response()
            }
            Err(err) => bq_error_response(&err),
        };
    }
    if let Some(load_cfg) = body.get("configuration").and_then(|c| c.get("load")) {
        return match execute_uri_load_job(&state, &project_id, job_id, location, load_cfg).await {
            Ok(rec) => {
                let resp = job_json(&rec);
                state.record_job(rec);
                Json(resp).into_response()
            }
            Err(err) => bq_error_response(&err),
        };
    }
    if let Some(extract_cfg) = body.get("configuration").and_then(|c| c.get("extract")) {
        return match execute_extract_job(&state, &project_id, job_id, location, extract_cfg).await {
            Ok(rec) => {
                let resp = job_json(&rec);
                state.record_job(rec);
                Json(resp).into_response()
            }
            Err(err) => bq_error_response(&err),
        };
    }

    let Some(query_cfg) = body.get("configuration").and_then(|c| c.get("query")) else {
        let rec = JobRecord {
            job_id,
            project_id,
            location,
            state: JobStatus::Done,
            created_at: Utc::now(),
            result: Some(std::sync::Arc::new(QueryResult::empty())),
            error: None,
            total_bytes_processed: 0,
            query: None,
            configuration: body.get("configuration").cloned(),
            load_output_rows: None,
        };
        let resp = job_json(&rec);
        state.record_job(rec);
        return Json(resp).into_response();
    };

    run_insert_query_job(&state, project_id, job_id, location, &body, query_cfg)
}

fn run_insert_query_job(
    state: &ServerState,
    project_id: String,
    job_id: String,
    location: String,
    body: &JsonValue,
    query_cfg: &JsonValue,
) -> Response {
    let sql = query_cfg
        .get("query")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let dry_run = body
        .get("configuration")
        .and_then(|c| c.get("dryRun"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let default_ds = query_cfg
        .get("defaultDataset")
        .and_then(|d| d.get("datasetId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let (named_params, pos_params) = match parse_query_params(query_cfg) {
        Ok(p) => p,
        Err(err) => return bq_error_response(&err),
    };

    let (result, error) = if dry_run {
        match dry_run_sql(state, sql, default_ds, &named_params, &pos_params) {
            Ok(qr) => (Some(std::sync::Arc::new(qr)), None),
            Err(err) => (None, Some(*err)),
        }
    } else {
        let mut eng = state.engine.write();
        let mut fork = eng.fork_isolated();
        match run_sql_on_engine_ref(&mut fork, sql, default_ds, &named_params, &pos_params) {
            Ok(qr) => match apply_query_destination(&mut fork, &project_id, query_cfg, &qr) {
                Ok(()) => {
                    *eng = fork;
                    drop(eng);
                    (Some(std::sync::Arc::new(qr)), None)
                }
                Err(err) => (None, Some(*err)),
            },
            Err(err) => (None, Some(*err)),
        }
    };

    if !dry_run && error.is_none() {
        state.persist_if_enabled();
    }
    let rec = JobRecord {
        job_id,
        project_id,
        location,
        state: JobStatus::Done,
        created_at: Utc::now(),
        result,
        error,
        total_bytes_processed: 0,
        query: Some(sql.to_owned()),
        configuration: body.get("configuration").cloned(),
        load_output_rows: None,
    };
    let resp = job_json(&rec);
    state.record_job(rec);
    Json(resp).into_response()
}

pub(super) async fn get_job(
    State(state): State<ServerState>,
    Path((project_id, job_id)): Path<(String, String)>,
) -> Response {
    state.jobs.read().get(&job_id).map_or_else(
        || {
            bq_error_response(&BqError::not_found(format!(
                "Not found: Job {project_id}:{job_id}"
            )))
        },
        |rec| Json(job_json(rec)).into_response(),
    )
}

pub(super) async fn cancel_job(
    State(state): State<ServerState>,
    Path((project_id, job_id)): Path<(String, String)>,
) -> Response {
    match state.jobs.write().get_mut(&job_id) {
        Some(rec) => {
            rec.state = JobStatus::Done;
            Json(json!({ "kind": "bigquery#jobCancelResponse", "job": job_json(rec) }))
                .into_response()
        }
        None => bq_error_response(&BqError::not_found(format!(
            "Not found: Job {project_id}:{job_id}"
        ))),
    }
}

pub(super) async fn get_query_results(
    State(state): State<ServerState>,
    Path((project_id, job_id)): Path<(String, String)>,
    Query(pag): Query<PaginationParams>,
) -> Response {
    let Some(rec) = state.jobs.read().get(&job_id).cloned() else {
        return bq_error_response(&BqError::not_found(format!(
            "Not found: Job {project_id}:{job_id}"
        )));
    };
    if let Some(ref err) = rec.error {
        return bq_error_response(err);
    }
    let empty = QueryResult::empty();
    let qr = rec.result.as_deref().unwrap_or(&empty);
    let total = qr.rows.len();
    let start = pag
        .page_token
        .as_deref()
        .and_then(|t| t.parse::<usize>().ok())
        .or(pag.start_index)
        .unwrap_or(0)
        .min(total);
    let max_res = pag.max_results.unwrap_or(10_000);
    let end = start.saturating_add(max_res).min(total);
    let rows = encode_rows(&qr.columns, qr.rows.get(start..end).unwrap_or(&[]));

    let mut resp = json!({
        "kind": "bigquery#getQueryResultsResponse",
        "jobReference": { "projectId": project_id, "jobId": job_id, "location": rec.location },
        "schema": encode_schema(&qr.columns),
        "rows": rows,
        "totalRows": total.to_string(),
        "jobComplete": true,
        "totalBytesProcessed": rec.total_bytes_processed.to_string(),
        "numDmlAffectedRows": qr.rows_affected.to_string(),
    });
    if end < total {
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("pageToken".to_owned(), json!(end.to_string()));
        }
    }
    Json(resp).into_response()
}

pub(super) fn job_json(rec: &JobRecord) -> JsonValue {
    let mut status = json!({ "state": rec.state.as_str() });
    if let Some(ref err) = rec.error {
        if let Some(obj) = status.as_object_mut() {
            obj.insert(
                "errorResult".to_owned(),
                json!({ "reason": err.api_reason(), "message": err.message }),
            );
        }
    }
    let mut query_stats = json!({
        "totalBytesProcessed": rec.total_bytes_processed.to_string(),
    });
    if let Some(ref qr) = rec.result {
        if !qr.columns.is_empty() {
            if let Some(obj) = query_stats.as_object_mut() {
                obj.insert("schema".to_owned(), encode_schema(&qr.columns));
            }
        }
    }
    let mut job_stats = json!({
        "creationTime": rec.created_at.timestamp_millis().to_string(),
        "totalBytesProcessed": rec.total_bytes_processed.to_string(),
        "query": query_stats,
    });
    if let Some(out_rows) = rec.load_output_rows {
        if let Some(obj) = job_stats.as_object_mut() {
            obj.insert(
                "load".to_owned(),
                json!({ "outputRows": out_rows.to_string() }),
            );
        }
    }
    let config = build_job_configuration(rec);
    json!({
        "kind": "bigquery#job",
        "id": format!("{}:{}.{}", rec.project_id, rec.location, rec.job_id),
        "jobReference": {
            "projectId": rec.project_id,
            "jobId": rec.job_id,
            "location": rec.location,
        },
        "status": status,
        "statistics": job_stats,
        "configuration": config,
    })
}

fn build_job_configuration(rec: &JobRecord) -> JsonValue {
    if let Some(mut cfg) = rec.configuration.clone() {
        if let Some(qobj) = cfg.get_mut("query").and_then(JsonValue::as_object_mut) {
            if !qobj.contains_key("destinationTable") {
                qobj.insert(
                    "destinationTable".to_owned(),
                    json!({
                        "projectId": rec.project_id,
                        "datasetId": "_bqlite_jobs",
                        "tableId": format!("_anon_{}", rec.job_id),
                    }),
                );
            }
        }
        return cfg;
    }
    json!({
        "query": {
            "query": rec.query.as_deref().unwrap_or(""),
            "destinationTable": {
                "projectId": rec.project_id,
                "datasetId": "_bqlite_jobs",
                "tableId": format!("_anon_{}", rec.job_id),
            }
        }
    })
}
