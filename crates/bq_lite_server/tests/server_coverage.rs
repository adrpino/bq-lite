use bq_lite_core::{
    BqError, BqInterval, BqRange, BqStructFieldValue, BqType, BqValue, QueryColumn,
    RangeElementType, StructField,
};
use bq_lite_server::wire_json::{
    bq_error_response, decode_insert_all_value, decode_schema_fields, decode_wire_fv_value,
    encode_rows, encode_schema, encode_wire_value,
};
use bq_lite_server::{build_router, ServerState};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use reqwest::StatusCode;
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;

async fn spawn_server() -> Result<String, Box<dyn std::error::Error>> {
    let state = ServerState::new("test-proj");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    Ok(format!("http://{addr}"))
}

#[test]
fn wire_json_covers_scalar_types_and_floats() {
    let d = NaiveDate::from_ymd_opt(2024, 5, 19).unwrap();
    let t = NaiveTime::from_hms_micro_opt(12, 30, 45, 500_000).unwrap();
    let dt = NaiveDateTime::new(d, t);
    let ts = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);

    let cols = vec![
        QueryColumn::new("f_nan", BqType::Float64),
        QueryColumn::new("f_inf", BqType::Float64),
        QueryColumn::new("f_ninf", BqType::Float64),
        QueryColumn::new("bn", BqType::BigNumeric),
        QueryColumn::new("dt", BqType::Datetime),
        QueryColumn::new("tm", BqType::Time),
        QueryColumn::new("ts", BqType::Timestamp),
        QueryColumn::new("js", BqType::Json),
        QueryColumn::new("geo", BqType::Geography),
        QueryColumn::new("iv", BqType::Interval),
        QueryColumn::new("rg", BqType::Range(RangeElementType::Date)),
    ];
    let schema = encode_schema(&cols);
    assert_eq!(schema["fields"].as_array().unwrap().len(), 11);

    let row = vec![
        BqValue::Float64(f64::NAN),
        BqValue::Float64(f64::INFINITY),
        BqValue::Float64(f64::NEG_INFINITY),
        BqValue::BigNumeric(Box::new("123.456".parse().unwrap())),
        BqValue::Datetime(dt),
        BqValue::Time(t),
        BqValue::Timestamp(ts),
        BqValue::Json(Box::new(json!({"x": 1}))),
        BqValue::Geography("POINT(0 0)".to_owned()),
        BqValue::Interval(BqInterval::new(1, 2, 3)),
        BqValue::Range(Box::new(BqRange {
            elem_type: RangeElementType::Date,
            start: Some(BqValue::Date(d)),
            end: None,
        })),
    ];
    let encoded = encode_rows(&cols, &[row]);
    let cells = encoded[0]["f"].as_array().unwrap();
    assert_eq!(cells[0]["v"], "NaN");
    assert_eq!(cells[1]["v"], "Infinity");
    assert_eq!(cells[2]["v"], "-Infinity");
    assert!(decode_wire_fv_value(&cells[3]["v"], &BqType::BigNumeric).is_ok());
    assert!(decode_wire_fv_value(&cells[4]["v"], &BqType::Datetime).is_ok());
    assert!(decode_wire_fv_value(&cells[5]["v"], &BqType::Time).is_ok());
    assert!(decode_wire_fv_value(&cells[6]["v"], &BqType::Timestamp).is_ok());
    assert!(decode_wire_fv_value(&cells[7]["v"], &BqType::Json).is_ok());
    assert!(decode_wire_fv_value(&cells[8]["v"], &BqType::Geography).is_ok());
}

#[test]
fn wire_json_covers_nested_and_error_cases() {
    let empty_arr = encode_wire_value(&BqValue::Null, Some(&BqType::array(BqType::Int64).unwrap()));
    assert_eq!(empty_arr, json!([]));
    assert!(decode_wire_fv_value(&json!(null), &BqType::array(BqType::Int64).unwrap()).is_ok());
    assert!(decode_wire_fv_value(&json!(null), &BqType::Interval).is_ok());
    assert!(decode_wire_fv_value(&json!("bad"), &BqType::Interval).is_err());
    assert!(decode_wire_fv_value(&json!(true), &BqType::Bool).is_ok());
    assert!(decode_wire_fv_value(&json!(10), &BqType::Int64).is_ok());
    assert!(decode_wire_fv_value(&json!(1.5), &BqType::Float64).is_ok());
    assert!(decode_wire_fv_value(&json!("nope"), &BqType::Bool).is_err());
    assert!(decode_wire_fv_value(&json!("nope"), &BqType::Date).is_err());

    let st_ty = BqType::structure(vec![StructField::named("k", BqType::String)]);
    let st_val = BqValue::Struct(vec![BqStructFieldValue {
        name: Some("k".to_owned()),
        value: BqValue::String("v".to_owned()),
    }]);
    let st_enc = encode_wire_value(&st_val, Some(&st_ty));
    assert_eq!(decode_wire_fv_value(&st_enc, &st_ty).unwrap(), st_val);
    assert!(decode_insert_all_value(&json!({"k": "v"}), &st_ty).is_ok());
    assert!(decode_insert_all_value(&json!("not_obj"), &st_ty).is_err());
    assert!(decode_schema_fields(&[json!({"name":"x","type":"UNKNOWN"})]).is_err());
    assert_eq!(
        bq_error_response(&BqError::not_found("nf")).status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn wire_json_covers_schema_types_and_decode_errors() {
    let schema = decode_schema_fields(&[
        json!({"name": "a", "type": "BYTES", "mode": "NULLABLE"}),
        json!({"name": "b", "type": "NUMERIC"}),
        json!({"name": "c", "type": "BIGNUMERIC"}),
        json!({"name": "d", "type": "DATE"}),
        json!({"name": "e", "type": "DATETIME"}),
        json!({"name": "f", "type": "TIME"}),
        json!({"name": "g", "type": "TIMESTAMP"}),
        json!({"name": "h", "type": "JSON"}),
        json!({"name": "i", "type": "GEOGRAPHY"}),
        json!({"name": "j", "type": "FLOAT64"}),
        json!({"name": "k", "type": "RECORD", "fields": [{"name": "x", "type": "BOOL"}]}),
    ])
    .unwrap();
    assert_eq!(schema.len(), 11);

    for bad in [
        json!({}),
        json!({"name": "x"}),
        json!({"name": "x", "type": "RECORD"}),
        json!("not_obj"),
    ] {
        assert!(decode_schema_fields(&[bad]).is_err());
    }

    for (val, ty) in [
        (json!("!!!"), BqType::Bytes),
        (json!(123), BqType::String),
        (json!("bad"), BqType::Int64),
        (json!("bad"), BqType::Float64),
        (json!("bad"), BqType::Datetime),
        (json!("bad"), BqType::Time),
        (json!("bad"), BqType::Timestamp),
        (json!("{bad"), BqType::Json),
        (json!("not_arr"), BqType::array(BqType::Int64).unwrap()),
        (json!({}), BqType::structure(vec![])),
    ] {
        assert!(decode_wire_fv_value(&val, &ty).is_err());
    }
    assert!(decode_wire_fv_value(&json!("1700000000.5"), &BqType::Timestamp).is_ok());
    assert!(decode_wire_fv_value(&json!("YWJj"), &BqType::Bytes).is_ok());
}

#[tokio::test]
async fn server_covers_projects_datasets_and_stubs() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    for path in ["/projects", "/discovery/v1/apis/bigquery/v2/rest"] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/oauth2/v4/token"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "cov_ds" }, "location": "US" }))
        .send()
        .await
        .unwrap();
    for path in [
        "/bigquery/v2/projects/test-proj/datasets",
        "/bigquery/v2/projects/test-proj/datasets/cov_ds",
        "/bigquery/v2/projects/test-proj/datasets/cov_ds/models",
        "/bigquery/v2/projects/test-proj/datasets/cov_ds/routines",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        client
            .patch(format!(
                "{base}/bigquery/v2/projects/test-proj/datasets/cov_ds"
            ))
            .json(&json!({ "description": "updated", "labels": { "a": "b" } }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn server_covers_tables_and_insert_all_errors() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "t_ds" } }))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/projects/test-proj/datasets/t_ds/tables"))
        .json(&json!({
            "tableReference": { "tableId": "t1" },
            "schema": { "fields": [{ "name": "id", "type": "INT64", "mode": "REQUIRED" }] }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/projects/test-proj/datasets/t_ds/tables"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .patch(format!("{base}/projects/test-proj/datasets/t_ds/tables/t1"))
            .json(&json!({ "description": "patched" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let ins_err: JsonValue = client
        .post(format!(
            "{base}/projects/test-proj/datasets/t_ds/tables/t1/insertAll"
        ))
        .json(&json!({
            "skipInvalidRows": true,
            "rows": [
                { "json": { "id": 1 } },
                { "json": { "id": "not_int" } },
                { "json": { "unknown": 5 } }
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ins_err["insertErrors"].as_array().unwrap().len(), 2);

    assert_eq!(
        client
            .delete(format!("{base}/projects/test-proj/datasets/t_ds"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .delete(format!(
                "{base}/projects/test-proj/datasets/t_ds?deleteContents=true"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn server_covers_legacy_sql_and_job_lifecycle() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    assert_eq!(
        client
            .post(format!("{base}/projects/test-proj/queries"))
            .json(&json!({ "query": "SELECT 1", "useLegacySql": true }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let job: JsonValue = client
        .post(format!("{base}/projects/test-proj/jobs"))
        .json(&json!({ "configuration": { "query": { "query": "SELECT 1" } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let jid = job["jobReference"]["jobId"].as_str().unwrap();
    assert_eq!(
        client
            .post(format!("{base}/projects/test-proj/jobs/{jid}/cancel"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/projects/test-proj/jobs"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn repl_renders_ascii_table() {
    let qr = bq_lite_core::QueryResult::new(
        vec![QueryColumn::new("id", BqType::Int64)],
        vec![vec![BqValue::Int64(42)]],
    );
    let mut out = Vec::new();
    bq_lite_server::repl::render_table(&qr, &mut out).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(rendered.contains("id"));
    assert!(rendered.contains("42"));
}
