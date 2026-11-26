use bq_lite_server::{build_router, ServerState};
use reqwest::StatusCode;
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;

async fn spawn_test_server() -> std::io::Result<String> {
    let state = ServerState::new("test-proj");
    let app = build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

#[tokio::test]
async fn rest_health_oauth_and_dataset_table_crud() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();

    let h = client.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(h.status(), StatusCode::OK);
    let tok: JsonValue = client
        .post(format!("{base}/token"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tok["token_type"], "Bearer");

    let ds_resp = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({
            "datasetReference": { "datasetId": "analytics" },
            "location": "US"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ds_resp.status(), StatusCode::OK);

    let tbl_resp = client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/analytics/tables"
        ))
        .json(&json!({
            "tableReference": { "tableId": "events" },
            "schema": {
                "fields": [
                    { "name": "id", "type": "INTEGER", "mode": "REQUIRED" },
                    { "name": "tags", "type": "STRING", "mode": "REPEATED" },
                    { "name": "meta", "type": "RECORD", "mode": "NULLABLE", "fields": [
                        { "name": "active", "type": "BOOLEAN", "mode": "NULLABLE" }
                    ]}
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(tbl_resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn rest_insert_all_dedup_and_tabledata_pagination() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "analytics" } }))
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/analytics/tables"
        ))
        .json(&json!({
            "tableReference": { "tableId": "events" },
            "schema": {
                "fields": [
                    { "name": "id", "type": "INTEGER", "mode": "REQUIRED" },
                    { "name": "tags", "type": "STRING", "mode": "REPEATED" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();

    let ins_resp: JsonValue = client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/analytics/tables/events/insertAll"
        ))
        .json(&json!({
            "rows": [
                { "insertId": "k1", "json": { "id": 1, "tags": ["a", "b"] } },
                { "insertId": "k1", "json": { "id": 999, "tags": [] } },
                { "insertId": "k2", "json": { "id": 2, "tags": ["c"] } },
                { "insertId": "k3", "json": { "id": 3, "tags": [] } }
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(ins_resp.get("insertErrors").is_none());

    let page1: JsonValue = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/analytics/tables/events/data?maxResults=2"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page1["totalRows"], "3");
    assert_eq!(page1["rows"].as_array().unwrap().len(), 2);
    let token = page1["pageToken"].as_str().unwrap();

    let page2: JsonValue = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/analytics/tables/events/data?maxResults=2&pageToken={token}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert!(page2.get("pageToken").is_none());
}

#[tokio::test]
async fn rest_sync_query_params_dry_run_and_error_envelope() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();

    let q_resp: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT @x + 1 AS sum_val, [1, 2] AS arr, TRUE AS ok",
            "useLegacySql": false,
            "queryParameters": [{
                "name": "x",
                "parameterType": { "type": "INT64" },
                "parameterValue": { "value": "41" }
            }]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q_resp["jobComplete"], true);
    assert_eq!(q_resp["rows"][0]["f"][0]["v"], "42");
    assert_eq!(q_resp["rows"][0]["f"][1]["v"][0]["v"], "1");
    assert_eq!(q_resp["rows"][0]["f"][2]["v"], "true");

    let dry: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT 1 AS a", "dryRun": true, "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dry["jobComplete"], true);

    let err_resp = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT FROM", "useLegacySql": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(err_resp.status(), StatusCode::BAD_REQUEST);
    let err_json: JsonValue = err_resp.json().await.unwrap();
    assert_eq!(err_json["error"]["code"], 400);
    assert_eq!(err_json["error"]["errors"][0]["reason"], "invalidQuery");
}

#[tokio::test]
async fn rest_async_jobs_pagination_and_reset() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();

    let job_resp: JsonValue = client
        .post(format!("{base}/projects/test-proj/jobs"))
        .json(&json!({
            "configuration": {
                "query": { "query": "SELECT n FROM UNNEST([10, 20, 30]) AS n ORDER BY n" }
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job_id = job_resp["jobReference"]["jobId"].as_str().unwrap();

    let res1: JsonValue = client
        .get(format!(
            "{base}/projects/test-proj/queries/{job_id}?maxResults=2"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res1["rows"].as_array().unwrap().len(), 2);
    assert_eq!(res1["rows"][0]["f"][0]["v"], "10");

    let reset = client
        .post(format!("{base}/_bqlite/reset"))
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::NO_CONTENT);
    let job_after = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/jobs/{job_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(job_after.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rest_upload_csv_load_job_and_arrow_ipc_stream() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "raw" } }))
        .send()
        .await
        .unwrap();

    let boundary = "bq_boundary_123";
    let meta = json!({
        "configuration": {
            "load": {
                "destinationTable": {
                    "projectId": "test-proj",
                    "datasetId": "raw",
                    "tableId": "users"
                },
                "sourceFormat": "CSV",
                "skipLeadingRows": 1,
                "schema": {
                    "fields": [
                        { "name": "id", "type": "INT64" },
                        { "name": "name", "type": "STRING" }
                    ]
                }
            }
        }
    });
    let body = format!(
        "--{boundary}\r\nContent-Type: application/json\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: text/csv\r\n\r\nid,name\n1,ada\n2,linus\n--{boundary}--\r\n"
    );

    let load_resp: JsonValue = client
        .post(format!("{base}/upload/bigquery/v2/projects/test-proj/jobs"))
        .header(
            "Content-Type",
            format!("multipart/related; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(load_resp["status"]["state"], "DONE");
    assert_eq!(load_resp["statistics"]["load"]["outputRows"], "2");

    let q_resp: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT id, name FROM raw.users ORDER BY id",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job_id = q_resp["jobReference"]["jobId"].as_str().unwrap();

    let arrow_bytes = client
        .get(format!("{base}/_bqlite/arrow/{job_id}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let decoded = bq_lite_core::io::arrow_codec::arrow_ipc_to_query_result(&arrow_bytes).unwrap();
    assert_eq!(decoded.rows.len(), 2);
}

async fn seed_durable_server(
    client: &reqwest::Client,
    dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = ServerState::with_data_dir("test-proj", dir.to_path_buf())?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    let base = format!("http://{addr}");

    for (path, body) in [
        (
            format!("{base}/bigquery/v2/projects/test-proj/datasets"),
            json!({ "datasetReference": { "datasetId": "durable_ds" } }),
        ),
        (
            format!("{base}/bigquery/v2/projects/test-proj/datasets/durable_ds/tables"),
            json!({
                "tableReference": { "tableId": "items" },
                "schema": { "fields": [
                    { "name": "id", "type": "INT64", "mode": "REQUIRED" },
                    { "name": "label", "type": "STRING", "mode": "NULLABLE" }
                ] }
            }),
        ),
        (
            format!(
                "{base}/bigquery/v2/projects/test-proj/datasets/durable_ds/tables/items/insertAll"
            ),
            json!({ "rows": [{ "json": { "id": 1, "label": "from_insert_all" } }] }),
        ),
        (
            format!("{base}/bigquery/v2/projects/test-proj/queries"),
            json!({ "query": "INSERT INTO durable_ds.items VALUES (2, 'from_query')", "useLegacySql": false }),
        ),
    ] {
        assert_eq!(
            client.post(path).json(&body).send().await?.status(),
            StatusCode::OK
        );
    }
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn rest_persistence_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    seed_durable_server(&client, dir.path()).await.unwrap();

    let reopened = ServerState::with_data_dir("test-proj", dir.path().to_path_buf()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(reopened)).await;
    });
    let base = format!("http://{addr}");

    let q_resp: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT id, label FROM durable_ds.items ORDER BY id",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q_resp["totalRows"], "2");
    assert_eq!(q_resp["rows"][0]["f"][1]["v"], "from_insert_all");
    assert_eq!(q_resp["rows"][1]["f"][1]["v"], "from_query");
}

#[tokio::test]
async fn rest_bounded_job_history_across_many_queries() {
    let state = ServerState::new("test-proj");
    let jobs_ref = state.jobs.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    for i in 0..1_100 {
        let resp = client
            .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
            .json(&json!({ "query": format!("SELECT {i} AS n"), "useLegacySql": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    assert!(
        jobs_ref.read().len() <= bq_lite_server::state::MAX_RETAINED_JOBS,
        "retained job count exceeded cap: {}",
        jobs_ref.read().len()
    );
}
