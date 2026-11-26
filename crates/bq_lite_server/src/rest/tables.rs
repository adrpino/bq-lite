use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::catalog::{TableKind, TableMeta, TableRef};
use bq_lite_core::{BqError, BqResult};
use chrono::Utc;
use serde_json::{json, Value as JsonValue};

use crate::state::ServerState;
use crate::wire_json::{bq_error_response, decode_schema_fields, encode_table_schema};

use super::datasets::{
    apply_opt_str_field, decode_labels, paginate_items, parse_i64_or_str, PaginationParams,
};

pub(super) async fn list_tables(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Query(pag): Query<PaginationParams>,
) -> Response {
    let eng = state.engine.read();
    match eng.catalog().get_dataset(&project_id, &dataset_id) {
        Ok(ds) => {
            let all: Vec<JsonValue> = ds
                .tables
                .values()
                .map(|t| {
                    let tref = TableRef::new(&project_id, &dataset_id, &t.name);
                    let num_rows = eng.storage().read_rows(&tref).len();
                    table_json(&project_id, &dataset_id, t, num_rows)
                })
                .collect();
            let total = all.len();
            let (tables, next) = paginate_items(&all, pag.page_token.as_deref(), pag.max_results);
            let mut out = json!({
                "kind": "bigquery#tableList",
                "tables": tables,
                "totalItems": total,
            });
            if let Some(tok) = next {
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("nextPageToken".to_owned(), json!(tok));
                }
            }
            Json(out).into_response()
        }
        Err(err) => bq_error_response(&err),
    }
}

pub(super) async fn get_table(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
) -> Response {
    let tref = TableRef::new(&project_id, &dataset_id, &table_id);
    let res = {
        let eng = state.engine.read();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        eng.catalog().get_table(&canonical).map(|meta| {
            let num_rows = eng.storage().read_rows(&canonical).len();
            table_json(&project_id, &dataset_id, meta, num_rows)
        })
    };
    match res {
        Ok(json_val) => Json(json_val).into_response(),
        Err(err) => bq_error_response(&err),
    }
}

pub(super) async fn insert_table(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    let Some(table_id) = body
        .get("tableReference")
        .and_then(|r| r.get("tableId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
    else {
        return bq_error_response(&BqError::invalid_argument("Missing tableReference.tableId"));
    };

    let meta = match build_new_table_meta(table_id, &body) {
        Ok(m) => m,
        Err(err) => return bq_error_response(&err),
    };
    let tref = TableRef::new(&project_id, &dataset_id, &meta.name);
    let created_json = {
        let mut eng = state.engine.write();
        if let Err(err) = eng.create_table(&tref, meta) {
            return bq_error_response(&err);
        }
        match eng.catalog().get_table(&tref) {
            Ok(created) => table_json(&project_id, &dataset_id, created, 0),
            Err(err) => return bq_error_response(&err),
        }
    };
    state.persist_if_enabled();
    Json(created_json).into_response()
}

fn build_new_table_meta(table_id: String, body: &JsonValue) -> BqResult<TableMeta> {
    let columns = match body
        .get("schema")
        .and_then(|s| s.get("fields"))
        .and_then(JsonValue::as_array)
    {
        Some(fields) => decode_schema_fields(fields)?,
        None => Vec::new(),
    };
    let view_query_str = body
        .get("view")
        .and_then(|v| v.get("query"))
        .and_then(JsonValue::as_str);
    let (kind, parsed_vq, view_sql) = match view_query_str {
        Some(sql) => (
            TableKind::View,
            Some(Box::new(bq_lite_core::parser::parse_query(sql)?)),
            Some(sql.to_owned()),
        ),
        None => (TableKind::Table, None, None),
    };
    let partition_by = parse_time_partitioning(body);
    let cluster_by = parse_clustering(body);
    let now = Utc::now();
    Ok(TableMeta {
        name: table_id,
        kind,
        columns,
        partition_by,
        cluster_by,
        view_query: parsed_vq,
        created_at: now,
        updated_at: now,
        options: BTreeMap::new(),
        description: body
            .get("description")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        friendly_name: body
            .get("friendlyName")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
        labels: decode_labels(body.get("labels")),
        view_sql,
        expiration_ms: parse_i64_or_str(body.get("expirationTime")),
        external_uris: Vec::new(),
        external_format: None,
    })
}

fn parse_time_partitioning(body: &JsonValue) -> Option<String> {
    body.get("timePartitioning")
        .and_then(|tp| tp.get("field").or_else(|| tp.get("type")))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn parse_clustering(body: &JsonValue) -> Vec<String> {
    body.get("clustering")
        .and_then(|c| c.get("fields"))
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) async fn patch_table(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    update_table_impl(&state, &project_id, &dataset_id, &table_id, &body, false)
}

pub(super) async fn put_table(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    update_table_impl(&state, &project_id, &dataset_id, &table_id, &body, true)
}

fn update_table_impl(
    state: &ServerState,
    project_id: &str,
    dataset_id: &str,
    table_id: &str,
    body: &JsonValue,
    is_put: bool,
) -> Response {
    let tref = TableRef::new(project_id, dataset_id, table_id);
    let resp_json = {
        let mut eng = state.engine.write();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        let existing = match eng.catalog().get_table(&canonical) {
            Ok(m) => m.clone(),
            Err(err) => return bq_error_response(&err),
        };
        let mut updated = existing;
        if let Err(err) = apply_table_updates(&mut updated, body, is_put) {
            return bq_error_response(&err);
        }
        updated.updated_at = Utc::now();
        if let Err(err) = eng
            .catalog_mut()
            .put_table(&canonical, updated.clone(), true, false)
        {
            return bq_error_response(&err);
        }
        let cnt = eng.storage().read_rows(&canonical).len();
        drop(eng);
        table_json(project_id, dataset_id, &updated, cnt)
    };
    state.persist_if_enabled();
    Json(resp_json).into_response()
}

fn apply_table_updates(updated: &mut TableMeta, body: &JsonValue, is_put: bool) -> BqResult<()> {
    if let Some(fields) = body
        .get("schema")
        .and_then(|s| s.get("fields"))
        .and_then(JsonValue::as_array)
    {
        updated.columns = decode_schema_fields(fields)?;
    }
    apply_opt_str_field(&mut updated.description, body.get("description"), is_put);
    apply_opt_str_field(&mut updated.friendly_name, body.get("friendlyName"), is_put);
    if let Some(lbls) = body.get("labels") {
        updated.labels = if lbls.is_null() {
            BTreeMap::new()
        } else {
            decode_labels(Some(lbls))
        };
    } else if is_put {
        updated.labels.clear();
    }
    if let Some(vsql) = body
        .get("view")
        .and_then(|v| v.get("query"))
        .and_then(JsonValue::as_str)
    {
        updated.view_query = Some(Box::new(bq_lite_core::parser::parse_query(vsql)?));
        updated.view_sql = Some(vsql.to_owned());
    }
    if body.get("timePartitioning").is_some() {
        updated.partition_by = parse_time_partitioning(body);
    } else if is_put {
        updated.partition_by = None;
    }
    if body.get("clustering").is_some() {
        updated.cluster_by = parse_clustering(body);
    } else if is_put {
        updated.cluster_by.clear();
    }
    if let Some(exp) = body.get("expirationTime") {
        updated.expiration_ms = parse_i64_or_str(Some(exp));
    } else if is_put {
        updated.expiration_ms = None;
    }
    Ok(())
}

pub(super) async fn delete_table(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
) -> Response {
    let tref = TableRef::new(&project_id, &dataset_id, &table_id);
    {
        let mut eng = state.engine.write();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        if let Err(err) = eng.catalog_mut().drop_table(&canonical, false) {
            return bq_error_response(&err);
        }
        eng.storage_mut().drop_table(&canonical);
    }
    state.persist_if_enabled();
    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn table_json(
    project_id: &str,
    dataset_id: &str,
    meta: &TableMeta,
    num_rows: usize,
) -> JsonValue {
    let type_str = match meta.kind {
        TableKind::Table => "TABLE",
        TableKind::View => "VIEW",
        TableKind::MaterializedView => "MATERIALIZED_VIEW",
        TableKind::Snapshot => "SNAPSHOT",
        TableKind::External => "EXTERNAL",
    };
    let mod_ms = meta.updated_at.timestamp_millis().to_string();
    let mut out = json!({
        "kind": "bigquery#table",
        "etag": format!("\"{project_id}:{dataset_id}:{}:{mod_ms}\"", meta.name),
        "id": format!("{project_id}:{dataset_id}.{}", meta.name),
        "selfLink": format!("/bigquery/v2/projects/{project_id}/datasets/{dataset_id}/tables/{}", meta.name),
        "tableReference": {
            "projectId": project_id,
            "datasetId": dataset_id,
            "tableId": meta.name,
        },
        "type": type_str,
        "schema": encode_table_schema(&meta.columns),
        "numRows": num_rows.to_string(),
        "numBytes": (num_rows * 64).to_string(),
        "creationTime": meta.created_at.timestamp_millis().to_string(),
        "lastModifiedTime": mod_ms,
    });
    if let Some(obj) = out.as_object_mut() {
        if let Some(ref desc) = meta.description {
            obj.insert("description".to_owned(), json!(desc));
        }
        if let Some(ref fname) = meta.friendly_name {
            obj.insert("friendlyName".to_owned(), json!(fname));
        }
        if !meta.labels.is_empty() {
            obj.insert("labels".to_owned(), json!(meta.labels));
        }
        if let Some(ref part) = meta.partition_by {
            obj.insert(
                "timePartitioning".to_owned(),
                json!({ "type": "DAY", "field": part }),
            );
        }
        if !meta.cluster_by.is_empty() {
            obj.insert(
                "clustering".to_owned(),
                json!({ "fields": meta.cluster_by }),
            );
        }
        if matches!(meta.kind, TableKind::View | TableKind::MaterializedView) {
            if let Some(ref vsql) = meta.view_sql {
                obj.insert(
                    "view".to_owned(),
                    json!({ "query": vsql, "useLegacySql": false }),
                );
            }
        }
        if let Some(exp) = meta.expiration_ms {
            obj.insert("expirationTime".to_owned(), json!(exp.to_string()));
        }
    }
    out
}
