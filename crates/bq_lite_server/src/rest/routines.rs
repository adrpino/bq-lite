use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bq_lite_core::catalog::RoutineMeta;
use bq_lite_core::parser::parse_statement;
use bq_lite_core::BqError;
use serde_json::{json, Value as JsonValue};

use crate::state::ServerState;
use crate::wire_json::bq_error_response;

use super::datasets::{paginate_items, PaginationParams};

pub(super) async fn list_routines(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Query(pag): Query<PaginationParams>,
) -> Response {
    let eng = state.engine.read();
    match eng.catalog().get_dataset(&project_id, &dataset_id) {
        Ok(ds) => {
            let all: Vec<JsonValue> = ds
                .routines
                .iter()
                .map(|(name, r)| routine_json(&project_id, &dataset_id, name, r))
                .collect();
            let (routines, next) = paginate_items(&all, pag.page_token.as_deref(), pag.max_results);
            let mut out = json!({ "routines": routines });
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

pub(super) async fn get_routine(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, routine_id)): Path<(String, String, String)>,
) -> Response {
    let resp = {
        let eng = state.engine.read();
        let ds = match eng.catalog().get_dataset(&project_id, &dataset_id) {
            Ok(d) => d,
            Err(err) => return bq_error_response(&err),
        };
        let out = ds
            .routines
            .get(&routine_id)
            .map(|r| routine_json(&project_id, &dataset_id, &routine_id, r));
        drop(eng);
        out
    };
    resp.map_or_else(
        || {
            bq_error_response(&BqError::not_found(format!(
                "Not found: Routine {project_id}:{dataset_id}.{routine_id}"
            )))
        },
        |v| Json(v).into_response(),
    )
}

pub(super) async fn insert_routine(
    State(state): State<ServerState>,
    Path((project_id, dataset_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    let Some(routine_id) = body
        .get("routineReference")
        .and_then(|r| r.get("routineId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
    else {
        return bq_error_response(&BqError::invalid_argument(
            "Missing routineReference.routineId",
        ));
    };
    upsert_routine_impl(state, project_id, dataset_id, routine_id, &body, false).await
}

pub(super) async fn put_routine(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, routine_id)): Path<(String, String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    upsert_routine_impl(state, project_id, dataset_id, routine_id, &body, true).await
}

async fn upsert_routine_impl(
    state: ServerState,
    project_id: String,
    dataset_id: String,
    routine_id: String,
    body: &JsonValue,
    or_replace: bool,
) -> Response {
    let rtype = body
        .get("routineType")
        .and_then(JsonValue::as_str)
        .unwrap_or("SCALAR_FUNCTION");
    let def_body = body
        .get("definitionBody")
        .and_then(JsonValue::as_str)
        .unwrap_or("1");
    let args_sql = format_routine_args(body.get("arguments").and_then(JsonValue::as_array));
    let ret_sql = body
        .get("returnType")
        .and_then(|rt| rt.get("typeKind"))
        .and_then(JsonValue::as_str)
        .map_or_else(String::new, |tk| format!(" RETURNS {tk}"));
    let replace_kw = if or_replace { "OR REPLACE " } else { "" };

    let sql = match rtype {
        "PROCEDURE" => format!(
            "CREATE {replace_kw}PROCEDURE `{project_id}`.`{dataset_id}`.`{routine_id}`({args_sql}) BEGIN {def_body}; END"
        ),
        "TABLE_VALUED_FUNCTION" => format!(
            "CREATE {replace_kw}TABLE FUNCTION `{project_id}`.`{dataset_id}`.`{routine_id}`({args_sql}) AS {def_body}"
        ),
        _ => format!(
            "CREATE {replace_kw}FUNCTION `{project_id}`.`{dataset_id}`.`{routine_id}`({args_sql}){ret_sql} AS ({def_body})"
        ),
    };

    if parse_statement(&sql).is_err() {
        return bq_error_response(&BqError::invalid_argument(
            "Unsupported routine definitionBody",
        ));
    }
    {
        let mut eng = state.engine.write();
        if let Err(err) = eng.exec(&sql) {
            return bq_error_response(&err);
        }
    }
    state.persist_if_enabled();
    get_routine(State(state), Path((project_id, dataset_id, routine_id))).await
}

fn format_routine_args(args: Option<&Vec<JsonValue>>) -> String {
    let Some(list) = args else {
        return String::new();
    };
    list.iter()
        .filter_map(|a| {
            let name = a.get("name").and_then(JsonValue::as_str)?;
            let ty = a
                .get("dataType")
                .and_then(|d| d.get("typeKind"))
                .and_then(JsonValue::as_str)
                .unwrap_or("STRING");
            let mode = match a.get("mode").and_then(JsonValue::as_str) {
                Some("OUT") => "OUT ",
                Some("INOUT") => "INOUT ",
                _ => "",
            };
            Some(format!("{mode}{name} {ty}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) async fn delete_routine(
    State(state): State<ServerState>,
    Path((project_id, dataset_id, routine_id)): Path<(String, String, String)>,
) -> Response {
    {
        let mut eng = state.engine.write();
        let ds = match eng.catalog_mut().get_dataset_mut(&project_id, &dataset_id) {
            Ok(d) => d,
            Err(err) => return bq_error_response(&err),
        };
        let removed = ds.routines.remove(&routine_id).is_some();
        drop(eng);
        if !removed {
            return bq_error_response(&BqError::not_found(format!(
                "Not found: Routine {project_id}:{dataset_id}.{routine_id}"
            )));
        }
    }
    state.persist_if_enabled();
    StatusCode::NO_CONTENT.into_response()
}

const fn standard_sql_type_kind(ty: &bq_lite_core::BqType) -> &'static str {
    use bq_lite_core::BqType;
    match ty {
        BqType::Int64 => "INT64",
        BqType::Float64 => "FLOAT64",
        BqType::Bool => "BOOL",
        BqType::Bytes => "BYTES",
        BqType::Numeric => "NUMERIC",
        BqType::BigNumeric => "BIGNUMERIC",
        BqType::Date => "DATE",
        BqType::Datetime => "DATETIME",
        BqType::Time => "TIME",
        BqType::Timestamp => "TIMESTAMP",
        BqType::Interval => "INTERVAL",
        BqType::Json => "JSON",
        BqType::Geography => "GEOGRAPHY",
        BqType::Range(_) => "RANGE",
        BqType::Array(_) => "ARRAY",
        BqType::Struct(_) => "STRUCT",
        BqType::String | BqType::UntypedNull => "STRING",
    }
}

fn routine_json(project_id: &str, dataset_id: &str, id: &str, r: &RoutineMeta) -> JsonValue {
    let (rtype, params, ret_type) = match r {
        RoutineMeta::Function(cf) => {
            let rt = match cf.kind {
                bq_lite_core::ast::FunctionKind::Scalar => "SCALAR_FUNCTION",
                bq_lite_core::ast::FunctionKind::Table => "TABLE_VALUED_FUNCTION",
            };
            let ret = cf
                .returns
                .as_ref()
                .and_then(|t| t.to_bq_type().ok())
                .map(|b| json!({ "typeKind": standard_sql_type_kind(&b) }));
            (rt, &cf.params, ret)
        }
        RoutineMeta::Procedure(cp) => ("PROCEDURE", &cp.params, None),
    };
    let arguments: Vec<JsonValue> = params
        .iter()
        .map(|p| {
            let tk =
                p.ty.to_bq_type()
                    .map_or("STRING", |b| standard_sql_type_kind(&b));
            json!({
                "name": p.name.value,
                "dataType": { "typeKind": tk },
            })
        })
        .collect();
    let mut out = json!({
        "etag": format!("\"{project_id}:{dataset_id}:{id}\""),
        "routineReference": {
            "projectId": project_id,
            "datasetId": dataset_id,
            "routineId": id,
        },
        "routineType": rtype,
        "language": "SQL",
        "arguments": arguments,
    });
    if let Some(rt) = ret_type {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("returnType".to_owned(), rt);
        }
    }
    out
}
