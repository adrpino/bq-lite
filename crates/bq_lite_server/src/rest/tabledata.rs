use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::catalog::{TableMeta, TableRef};
use bq_lite_core::{BqError, BqResult, BqValue, QueryColumn};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{json, Value as JsonValue};

use crate::state::ServerState;
use crate::wire_json::{bq_error_response, decode_insert_all_value, encode_rows};

use super::datasets::PaginationParams;

pub(super) fn split_partition_decorator(table_id: &str) -> (&str, Option<DateTime<Utc>>) {
    let Some((base, suffix)) = table_id.split_once('$') else {
        return (table_id, None);
    };
    if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(d) = NaiveDate::parse_from_str(suffix, "%Y%m%d") {
            if let Some(ndt) = d.and_hms_opt(0, 0, 0) {
                return (base, Some(Utc.from_utc_datetime(&ndt)));
            }
        }
    }
    (base, None)
}

pub(super) async fn list_tabledata(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
    Query(pag): Query<PaginationParams>,
) -> Response {
    let (base_table, part_ts) = split_partition_decorator(&table_id);
    let tref = TableRef::new(&project_id, &dataset_id, base_table);
    let (cols, all_rows) = {
        let eng = state.engine.read();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        let meta = match eng.catalog().get_table(&canonical) {
            Ok(m) => m,
            Err(err) => return bq_error_response(&err),
        };
        let cols: Vec<QueryColumn> = meta
            .columns
            .iter()
            .map(|c| QueryColumn::new(&c.name, c.ty.clone()))
            .collect();
        let rows = part_ts.map_or_else(
            || eng.storage().read_rows(&canonical),
            |pts| {
                eng.storage()
                    .read_rows_with_times(&canonical, meta.created_at)
                    .into_iter()
                    .filter(|(_, ts)| ts.date_naive() == pts.date_naive())
                    .map(|(r, _)| r)
                    .collect()
            },
        );
        drop(eng);
        (cols, rows)
    };
    let total = all_rows.len();
    let start = pag
        .page_token
        .as_deref()
        .and_then(|t| t.parse::<usize>().ok())
        .or(pag.start_index)
        .unwrap_or(0)
        .min(total);
    let max_res = pag.max_results.unwrap_or(10_000);
    let end = start.saturating_add(max_res).min(total);
    let page_rows = encode_rows(&cols, all_rows.get(start..end).unwrap_or(&[]));
    let mut out = json!({
        "kind": "bigquery#tableDataList",
        "totalRows": total.to_string(),
        "rows": page_rows,
    });
    if end < total {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("pageToken".to_owned(), json!(end.to_string()));
        }
    }
    Json(out).into_response()
}

pub(super) async fn insert_all(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, table_id)): Path<(String, String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    let skip_invalid = body
        .get("skipInvalidRows")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let ignore_unknown = body
        .get("ignoreUnknownValues")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let rows_json = body
        .get("rows")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();

    let (base_table, part_ts) = split_partition_decorator(&table_id);
    let tref = TableRef::new(&project_id, &dataset_id, base_table);
    let mut inserted_any = false;
    let insert_errors = {
        let mut eng = state.engine.write();
        let canonical = eng.catalog().canonical_table_ref(&tref);
        let meta = match eng.catalog().get_table(&canonical) {
            Ok(m) => m.clone(),
            Err(err) => return bq_error_response(&err),
        };
        let dedup_prefix = format!("{project_id}.{dataset_id}.{base_table}");
        let collected = collect_insert_all_rows(
            &state,
            &meta,
            &dedup_prefix,
            &rows_json,
            skip_invalid,
            ignore_unknown,
        );
        if collected.early_abort {
            return Json(json!({
                "kind": "bigquery#tableDataInsertAllResponse",
                "insertErrors": collected.errors,
            }))
            .into_response();
        }
        if !collected.prepared.is_empty() {
            let res = match part_ts {
                Some(ts) => eng.insert_rows_at(&canonical, collected.prepared, ts),
                None => eng.insert_rows(&canonical, collected.prepared),
            };
            if let Err(err) = res {
                return bq_error_response(&err);
            }
            for key in collected.dedup_keys {
                let _ = state.check_and_record_dedup(key);
            }
            inserted_any = true;
        }
        drop(eng);
        collected.errors
    };

    if inserted_any {
        state.persist_if_enabled();
    }

    let mut resp = json!({ "kind": "bigquery#tableDataInsertAllResponse" });
    if !insert_errors.is_empty() {
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("insertErrors".to_owned(), JsonValue::Array(insert_errors));
        }
    }
    Json(resp).into_response()
}

struct CollectedInsertRows {
    prepared: Vec<Vec<BqValue>>,
    dedup_keys: Vec<(String, String)>,
    errors: Vec<JsonValue>,
    early_abort: bool,
}

fn collect_insert_all_rows(
    state: &ServerState,
    meta: &TableMeta,
    dedup_prefix: &str,
    rows_json: &[JsonValue],
    skip_invalid: bool,
    ignore_unknown: bool,
) -> CollectedInsertRows {
    let mut prepared = Vec::new();
    let mut dedup_keys = Vec::new();
    let mut seen_in_batch = std::collections::BTreeSet::new();
    let mut insert_errors = Vec::new();
    for (idx, entry) in rows_json.iter().enumerate() {
        let insert_id = entry
            .get("insertId")
            .and_then(JsonValue::as_str)
            .map(|id| (dedup_prefix.to_owned(), id.to_owned()));
        if let Some(ref key) = insert_id {
            if state.insert_dedup.read().contains(key) || !seen_in_batch.insert(key.clone()) {
                continue;
            }
        }
        let json_obj = entry
            .get("json")
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        match decode_insert_all_row(meta, &json_obj, ignore_unknown) {
            Ok(row) => {
                prepared.push(row);
                if let Some(key) = insert_id {
                    dedup_keys.push(key);
                }
            }
            Err(err) => {
                insert_errors.push(json!({
                    "index": idx,
                    "errors": [{
                        "reason": "invalid",
                        "message": err.message,
                    }]
                }));
                if !skip_invalid {
                    return CollectedInsertRows {
                        prepared,
                        dedup_keys,
                        errors: insert_errors,
                        early_abort: true,
                    };
                }
            }
        }
    }
    CollectedInsertRows {
        prepared,
        dedup_keys,
        errors: insert_errors,
        early_abort: false,
    }
}

fn decode_insert_all_row(
    meta: &TableMeta,
    obj: &serde_json::Map<String, JsonValue>,
    ignore_unknown: bool,
) -> BqResult<Vec<BqValue>> {
    if !ignore_unknown {
        for key in obj.keys() {
            if meta.column_index(key).is_none() {
                return Err(BqError::invalid_argument(format!("No such field: {key}")));
            }
        }
    }
    let mut row = Vec::with_capacity(meta.columns.len());
    for col in &meta.columns {
        let raw = obj
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&col.name))
            .map_or(&JsonValue::Null, |(_, v)| v);
        let decoded = decode_insert_all_value(raw, &col.ty)?;
        if !col.nullable && decoded.is_null() {
            return Err(BqError::invalid_argument(format!(
                "Required field '{}' cannot be NULL",
                col.name
            )));
        }
        row.push(decoded);
    }
    Ok(row)
}
