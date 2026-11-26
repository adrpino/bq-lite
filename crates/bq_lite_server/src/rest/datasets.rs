use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::catalog::DatasetMeta;
use bq_lite_core::BqError;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::state::ServerState;
use crate::wire_json::bq_error_response;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PaginationParams {
    pub max_results: Option<usize>,
    pub page_token: Option<String>,
    pub start_index: Option<usize>,
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DeleteDatasetQuery {
    #[serde(rename = "deleteContents")]
    pub delete_contents: Option<bool>,
}

pub(super) async fn list_projects(State(state): State<ServerState>) -> Response {
    let projects: Vec<JsonValue> = {
        let eng = state.engine.read();
        eng.catalog()
            .list_projects()
            .into_iter()
            .map(|id| {
                json!({
                    "kind": "bigquery#project",
                    "id": id,
                    "numericId": "1",
                    "projectReference": { "projectId": id },
                    "friendlyName": id,
                })
            })
            .collect()
    };
    let total = projects.len();
    Json(json!({
        "kind": "bigquery#projectList",
        "projects": projects,
        "totalItems": total,
    }))
    .into_response()
}

pub(super) async fn list_datasets(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Query(pag): Query<PaginationParams>,
) -> Response {
    let all: Vec<JsonValue> = {
        let eng = state.engine.read();
        eng.catalog()
            .get_project(&project_id)
            .map(|p| {
                p.datasets
                    .values()
                    .filter(|d| d.name != "__temp")
                    .filter(|d| matches_label_filter(d, pag.filter.as_deref()))
                    .map(|d| dataset_json(&project_id, d))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (page, next) = paginate_items(&all, pag.page_token.as_deref(), pag.max_results);
    let mut out = json!({
        "kind": "bigquery#datasetList",
        "datasets": page,
    });
    if let Some(tok) = next {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("nextPageToken".to_owned(), json!(tok));
        }
    }
    Json(out).into_response()
}

fn matches_label_filter(ds: &DatasetMeta, filter: Option<&str>) -> bool {
    let Some(f) = filter.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let expr = f.strip_prefix("labels.").unwrap_or(f);
    if let Some((k, v)) = expr.split_once(':') {
        ds.labels.get(k.trim()).is_some_and(|val| val == v.trim())
    } else {
        ds.labels.contains_key(expr.trim())
    }
}

pub(super) fn paginate_items(
    items: &[JsonValue],
    page_token: Option<&str>,
    max_results: Option<usize>,
) -> (Vec<JsonValue>, Option<String>) {
    let total = items.len();
    let start = page_token
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total);
    let max_res = max_results.unwrap_or(10_000);
    let end = start.saturating_add(max_res).min(total);
    let page = items.get(start..end).unwrap_or(&[]).to_vec();
    let next = (end < total).then(|| end.to_string());
    (page, next)
}

pub(super) async fn get_dataset(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
) -> Response {
    let eng = state.engine.read();
    match eng.catalog().get_dataset(&project_id, &dataset_id) {
        Ok(ds) => Json(dataset_json(&project_id, ds)).into_response(),
        Err(err) => bq_error_response(&err),
    }
}

pub(super) async fn insert_dataset(
    State(state): State<ServerState>,
    Path(project_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let ds_id = body
        .get("datasetReference")
        .and_then(|r| r.get("datasetId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let Some(dataset_id) = ds_id else {
        return bq_error_response(&BqError::invalid_argument(
            "Missing datasetReference.datasetId",
        ));
    };
    let location = body
        .get("location")
        .and_then(JsonValue::as_str)
        .unwrap_or("US")
        .to_owned();
    let mut ds = DatasetMeta::new(dataset_id.clone(), location);
    ds.is_case_insensitive = body
        .get("isCaseInsensitive")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    ds.description = body
        .get("description")
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    ds.friendly_name = body
        .get("friendlyName")
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    ds.labels = decode_labels(body.get("labels"));
    ds.default_table_expiration_ms = parse_i64_or_str(body.get("defaultTableExpirationMs"));
    if let Some(acc) = body.get("access").and_then(JsonValue::as_array) {
        ds.access.clone_from(acc);
    }

    let created_json = {
        let mut eng = state.engine.write();
        if let Err(err) = eng.catalog_mut().create_dataset(&project_id, ds, false) {
            return bq_error_response(&err);
        }
        match eng.catalog().get_dataset(&project_id, &dataset_id) {
            Ok(created) => dataset_json(&project_id, created),
            Err(err) => return bq_error_response(&err),
        }
    };
    state.persist_if_enabled();
    Json(created_json).into_response()
}

pub(super) async fn patch_dataset(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    update_dataset_impl(&state, &project_id, &dataset_id, &body, false)
}

pub(super) async fn put_dataset(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    update_dataset_impl(&state, &project_id, &dataset_id, &body, true)
}

fn update_dataset_impl(
    state: &ServerState,
    project_id: &str,
    dataset_id: &str,
    body: &JsonValue,
    is_put: bool,
) -> Response {
    let resp_json = {
        let mut eng = state.engine.write();
        match eng.catalog_mut().get_dataset_mut(project_id, dataset_id) {
            Ok(ds) => {
                apply_dataset_updates(ds, body, is_put);
                dataset_json(project_id, ds)
            }
            Err(err) => return bq_error_response(&err),
        }
    };
    state.persist_if_enabled();
    Json(resp_json).into_response()
}

fn apply_dataset_updates(ds: &mut DatasetMeta, body: &JsonValue, is_put: bool) {
    if let Some(loc) = body.get("location").and_then(JsonValue::as_str) {
        loc.clone_into(&mut ds.location);
    }
    if let Some(ci) = body.get("isCaseInsensitive").and_then(JsonValue::as_bool) {
        ds.is_case_insensitive = ci;
    }
    apply_opt_str_field(&mut ds.description, body.get("description"), is_put);
    apply_opt_str_field(&mut ds.friendly_name, body.get("friendlyName"), is_put);
    if let Some(lbls) = body.get("labels") {
        ds.labels = if lbls.is_null() {
            BTreeMap::new()
        } else {
            decode_labels(Some(lbls))
        };
    } else if is_put {
        ds.labels.clear();
    }
    if let Some(exp) = body.get("defaultTableExpirationMs") {
        ds.default_table_expiration_ms = parse_i64_or_str(Some(exp));
    } else if is_put {
        ds.default_table_expiration_ms = None;
    }
    if let Some(acc) = body.get("access") {
        ds.access = acc.as_array().cloned().unwrap_or_default();
    } else if is_put {
        ds.access.clear();
    }
}

pub(super) fn apply_opt_str_field(
    target: &mut Option<String>,
    val: Option<&JsonValue>,
    is_put: bool,
) {
    match val {
        Some(v) if v.is_null() => *target = None,
        Some(v) => {
            if let Some(s) = v.as_str() {
                *target = Some(s.to_owned());
            }
        }
        None if is_put => *target = None,
        None => {}
    }
}

pub(super) fn parse_i64_or_str(val: Option<&JsonValue>) -> Option<i64> {
    let v = val?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
}

pub(super) async fn delete_dataset(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Query(q): Query<DeleteDatasetQuery>,
) -> Response {
    let cascade = q.delete_contents.unwrap_or(false);
    {
        let mut eng = state.engine.write();
        if let Err(err) = eng
            .catalog_mut()
            .drop_dataset(&project_id, &dataset_id, false, cascade)
        {
            return bq_error_response(&err);
        }
        eng.storage_mut().drop_dataset(&project_id, &dataset_id);
    }
    state.persist_if_enabled();
    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn decode_labels(val: Option<&JsonValue>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(obj) = val.and_then(JsonValue::as_object) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_owned());
            }
        }
    }
    out
}

fn dataset_json(project_id: &str, ds: &DatasetMeta) -> JsonValue {
    let ms = ds.created_at.timestamp_millis().to_string();
    let access_val = if ds.access.is_empty() {
        json!([{ "role": "OWNER", "specialGroup": "projectOwners" }])
    } else {
        JsonValue::Array(ds.access.clone())
    };
    let mut out = json!({
        "kind": "bigquery#dataset",
        "etag": format!("\"{project_id}:{}:{ms}\"", ds.name),
        "id": format!("{project_id}:{}", ds.name),
        "selfLink": format!("/bigquery/v2/projects/{project_id}/datasets/{}", ds.name),
        "datasetReference": {
            "projectId": project_id,
            "datasetId": ds.name,
        },
        "location": ds.location,
        "isCaseInsensitive": ds.is_case_insensitive,
        "creationTime": ms,
        "lastModifiedTime": ms,
        "access": access_val,
    });
    if let Some(obj) = out.as_object_mut() {
        if let Some(ref desc) = ds.description {
            obj.insert("description".to_owned(), json!(desc));
        }
        if let Some(ref fname) = ds.friendly_name {
            obj.insert("friendlyName".to_owned(), json!(fname));
        }
        if !ds.labels.is_empty() {
            obj.insert("labels".to_owned(), json!(ds.labels));
        }
        if let Some(exp) = ds.default_table_expiration_ms {
            obj.insert(
                "defaultTableExpirationMs".to_owned(),
                json!(exp.to_string()),
            );
        }
    }
    out
}
