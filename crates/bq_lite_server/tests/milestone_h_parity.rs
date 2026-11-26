use bq_lite_core::io::parquet_codec::export_parquet;
use bq_lite_core::{BqType, BqValue, QueryColumn, QueryResult};
use bq_lite_server::{build_router, ServerState};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value as JsonValue};

type TestErr = Box<dyn std::error::Error>;

async fn spawn_server() -> Result<(String, Client), TestErr> {
    let state = ServerState::new("test-proj");
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{addr}"), Client::new()))
}

async fn req_json(
    client: &Client,
    base: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<JsonValue>,
) -> Result<(StatusCode, JsonValue), TestErr> {
    let mut builder = client.request(method, format!("{base}{path}"));
    if let Some(v) = body {
        builder = builder.json(&v);
    }
    let resp = builder.send().await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let json_val = if bytes.is_empty() {
        JsonValue::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(JsonValue::Null)
    };
    Ok((status, json_val))
}

async fn send_multipart(
    client: &Client,
    base: &str,
    boundary: &str,
    meta: &JsonValue,
    data_ct: &str,
    payload: &[u8],
) -> Result<(StatusCode, JsonValue), TestErr> {
    let mut mp_body = Vec::new();
    mp_body.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
    );
    mp_body.extend_from_slice(meta.to_string().as_bytes());
    mp_body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Type: {data_ct}\r\n\r\n").as_bytes(),
    );
    mp_body.extend_from_slice(payload);
    mp_body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = client
        .post(format!("{base}/upload/bigquery/v2/projects/test-proj/jobs"))
        .header(
            "content-type",
            format!("multipart/related; boundary={boundary}"),
        )
        .body(mp_body)
        .send()
        .await?;
    let status = resp.status();
    let body: JsonValue = resp.json().await?;
    Ok((status, body))
}

#[tokio::test]
async fn milestone_h_binary_parquet_multipart_and_load_job_tracking() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    let (st, _) = req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({ "datasetReference": { "datasetId": "mh_ds" } })),
    )
    .await?;
    assert_eq!(st, StatusCode::OK);

    let qr = QueryResult::new(
        vec![
            QueryColumn::new("id", BqType::Int64),
            QueryColumn::new("label", BqType::String),
        ],
        vec![
            vec![BqValue::Int64(1), BqValue::String("one".to_owned())],
            vec![BqValue::Int64(2), BqValue::String("two".to_owned())],
        ],
    );
    let pq_bytes = export_parquet(&qr)?;
    let meta = json!({
        "jobReference": { "jobId": "custom_pq_job_1", "location": "US" },
        "configuration": {
            "load": {
                "destinationTable": { "projectId": "test-proj", "datasetId": "mh_ds", "tableId": "pq_tbl" },
                "sourceFormat": "PARQUET",
                "writeDisposition": "WRITE_TRUNCATE"
            }
        }
    });
    let (up_st, up_json) = send_multipart(
        &client,
        &base,
        "bqlite_bound",
        &meta,
        "application/octet-stream",
        &pq_bytes,
    )
    .await?;
    assert_eq!(up_st, StatusCode::OK);
    assert_eq!(up_json["jobReference"]["jobId"], "custom_pq_job_1");
    assert_eq!(up_json["statistics"]["load"]["outputRows"], "2");

    let (jst, polled) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/jobs/custom_pq_job_1",
        None,
    )
    .await?;
    assert_eq!(jst, StatusCode::OK);
    assert_eq!(polled["status"]["state"], "DONE");

    let (lst, list_tbls) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets/mh_ds/tables",
        None,
    )
    .await?;
    assert_eq!(lst, StatusCode::OK);
    assert_eq!(list_tbls["tables"][0]["numRows"], "2");
    Ok(())
}

#[tokio::test]
async fn milestone_h_csv_and_ndjson_autodetect() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({ "datasetReference": { "datasetId": "auto_ds" } })),
    )
    .await?;

    let csv_meta = json!({
        "configuration": {
            "load": {
                "destinationTable": { "projectId": "test-proj", "datasetId": "auto_ds", "tableId": "csv_auto" },
                "sourceFormat": "CSV",
                "autodetect": true
            }
        }
    });
    let (cst, _) = send_multipart(
        &client,
        &base,
        "bqlite_csv",
        &csv_meta,
        "text/csv",
        b"uid,score,active\n10,9.5,true\n20,8.0,false\n",
    )
    .await?;
    assert_eq!(cst, StatusCode::OK);

    let (_, csv_tbl) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets/auto_ds/tables/csv_auto",
        None,
    )
    .await?;
    assert_eq!(csv_tbl["numRows"], "2");
    assert_eq!(csv_tbl["schema"]["fields"][0]["type"], "INTEGER");
    assert_eq!(csv_tbl["schema"]["fields"][1]["type"], "FLOAT");
    assert_eq!(csv_tbl["schema"]["fields"][2]["type"], "BOOLEAN");

    let nd_meta = json!({
        "configuration": {
            "load": {
                "destinationTable": { "projectId": "test-proj", "datasetId": "auto_ds", "tableId": "nd_auto" },
                "sourceFormat": "NEWLINE_DELIMITED_JSON",
                "autodetect": true
            }
        }
    });
    let (nst, _) = send_multipart(
        &client,
        &base,
        "bqlite_nd",
        &nd_meta,
        "application/json",
        b"{\"id\":1,\"meta\":{\"k\":\"v\"}}\n{\"id\":2,\"meta\":{\"k\":\"w\"}}\n",
    )
    .await?;
    assert_eq!(nst, StatusCode::OK);

    let (_, nd_tbl) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets/auto_ds/tables/nd_auto",
        None,
    )
    .await?;
    assert_eq!(nd_tbl["numRows"], "2");
    assert_eq!(nd_tbl["schema"]["fields"][1]["type"], "RECORD");
    Ok(())
}

#[tokio::test]
async fn milestone_h_datasets_pagination_and_put_clearing() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({
            "datasetReference": { "datasetId": "ds_a" },
            "description": "initial",
            "labels": { "env": "prod" }
        })),
    )
    .await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({
            "datasetReference": { "datasetId": "ds_b" },
            "labels": { "env": "dev" }
        })),
    )
    .await?;

    let (_, filtered) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets?filter=labels.env:prod",
        None,
    )
    .await?;
    assert_eq!(filtered["datasets"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        filtered["datasets"][0]["datasetReference"]["datasetId"],
        "ds_a"
    );

    let (_, paged) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets?maxResults=1",
        None,
    )
    .await?;
    assert_eq!(paged["datasets"].as_array().map(Vec::len), Some(1));
    assert!(paged["nextPageToken"].is_string());

    let (put_st, put_ds) = req_json(
        &client,
        &base,
        reqwest::Method::PUT,
        "/bigquery/v2/projects/test-proj/datasets/ds_a",
        Some(json!({
            "datasetReference": { "datasetId": "ds_a" },
            "defaultTableExpirationMs": "3600000"
        })),
    )
    .await?;
    assert_eq!(put_st, StatusCode::OK);
    assert!(put_ds.get("description").is_none());
    assert!(put_ds.get("labels").is_none());
    assert_eq!(put_ds["defaultTableExpirationMs"], "3600000");
    Ok(())
}

#[tokio::test]
async fn milestone_h_views_and_job_destination_table() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({ "datasetReference": { "datasetId": "ds_v" } })),
    )
    .await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets/ds_v/tables",
        Some(json!({
            "tableReference": { "tableId": "my_view" },
            "view": { "query": "SELECT 1 AS x", "useLegacySql": false }
        })),
    )
    .await?;
    let (_, vmeta) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets/ds_v/tables/my_view",
        None,
    )
    .await?;
    assert_eq!(vmeta["view"]["query"], "SELECT 1 AS x");
    assert_eq!(vmeta["view"]["useLegacySql"], false);

    let (_, qjob) = req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/jobs",
        Some(json!({ "configuration": { "query": { "query": "SELECT 42 AS ans" } } })),
    )
    .await?;
    assert_eq!(
        qjob["configuration"]["query"]["destinationTable"]["datasetId"],
        "_bqlite_jobs"
    );
    Ok(())
}

#[tokio::test]
async fn milestone_h_insert_all_dedup_safety() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({ "datasetReference": { "datasetId": "ds_i" } })),
    )
    .await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets/ds_i/tables",
        Some(json!({
            "tableReference": { "tableId": "events" },
            "schema": {
                "fields": [
                    { "name": "id", "type": "INT64", "mode": "REQUIRED" },
                    { "name": "ts", "type": "TIMESTAMP" },
                    { "name": "tags", "type": "STRING", "mode": "REPEATED" },
                    { "name": "info", "type": "RECORD", "fields": [{ "name": "KeyName", "type": "STRING" }] }
                ]
            }
        })),
    )
    .await?;
    let (_, bad_ins) = req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets/ds_i/tables/events/insertAll",
        Some(json!({
            "rows": [
                { "insertId": "dedup_1", "json": { "id": 1, "ts": 1_700_000_000 } },
                { "insertId": "dedup_2", "json": { "ts": "bad" } }
            ]
        })),
    )
    .await?;
    assert!(bad_ins["insertErrors"].is_array());

    let (_, ok_ins) = req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets/ds_i/tables/events/insertAll",
        Some(json!({
            "rows": [{
                "insertId": "dedup_1",
                "json": {
                    "id": 1,
                    "ts": "2024-01-01 12:00:00 UTC",
                    "tags": null,
                    "info": { "keyname": "val" }
                }
            }]
        })),
    )
    .await?;
    assert!(ok_ins.get("insertErrors").is_none());

    let (_, tbl_data) = req_json(
        &client,
        &base,
        reqwest::Method::GET,
        "/bigquery/v2/projects/test-proj/datasets/ds_i/tables/events/data",
        None,
    )
    .await?;
    assert_eq!(tbl_data["totalRows"], "1");
    assert_eq!(tbl_data["rows"][0]["f"][3]["v"]["f"][0]["v"], "val");
    Ok(())
}

#[tokio::test]
async fn milestone_h_routines_put_and_typed_args() -> Result<(), TestErr> {
    let (base, client) = spawn_server().await?;
    req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets",
        Some(json!({ "datasetReference": { "datasetId": "ds_r" } })),
    )
    .await?;

    let (rst, rjson) = req_json(
        &client,
        &base,
        reqwest::Method::POST,
        "/bigquery/v2/projects/test-proj/datasets/ds_r/routines",
        Some(json!({
            "routineReference": { "routineId": "add_one" },
            "routineType": "SCALAR_FUNCTION",
            "arguments": [{ "name": "x", "dataType": { "typeKind": "INT64" } }],
            "returnType": { "typeKind": "INT64" },
            "definitionBody": "x + 1"
        })),
    )
    .await?;
    assert_eq!(rst, StatusCode::OK);
    assert_eq!(rjson["arguments"][0]["name"], "x");
    assert_eq!(rjson["returnType"]["typeKind"], "INT64");

    let (put_status, put_routine) = req_json(
        &client,
        &base,
        reqwest::Method::PUT,
        "/bigquery/v2/projects/test-proj/datasets/ds_r/routines/add_one",
        Some(json!({
            "routineType": "SCALAR_FUNCTION",
            "arguments": [{ "name": "x", "dataType": { "typeKind": "INT64" } }],
            "returnType": { "typeKind": "INT64" },
            "definitionBody": "x + 2"
        })),
    )
    .await?;
    assert_eq!(put_status, StatusCode::OK);
    assert_eq!(put_routine["arguments"][0]["name"], "x");
    Ok(())
}
