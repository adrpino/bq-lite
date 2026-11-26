use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bq_lite_server::{build_router, ServerState};
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

#[tokio::test]
async fn rest_query_parameters_numeric_array_struct_and_dml() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    let q_resp: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT @num + NUMERIC '1.5' AS total, ARRAY_LENGTH(@ids) AS cnt, @st.label AS lbl WHERE 2 IN UNNEST(@ids)",
            "useLegacySql": false,
            "queryParameters": [
                {
                    "name": "num",
                    "parameterType": { "type": "NUMERIC" },
                    "parameterValue": { "value": "40.5" }
                },
                {
                    "name": "ids",
                    "parameterType": { "type": "ARRAY", "arrayType": { "type": "INT64" } },
                    "parameterValue": { "arrayValues": [{ "value": "1" }, { "value": "2" }, { "value": "3" }] }
                },
                {
                    "name": "st",
                    "parameterType": {
                        "type": "STRUCT",
                        "structTypes": [{ "name": "label", "type": { "type": "STRING" } }]
                    },
                    "parameterValue": { "structValues": { "label": { "value": "ok" } } }
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(q_resp["schema"]["fields"][0]["type"], "NUMERIC");
    assert_eq!(q_resp["rows"][0]["f"][0]["v"], "42");
    assert_eq!(q_resp["rows"][0]["f"][1]["v"], "3");
    assert_eq!(q_resp["rows"][0]["f"][2]["v"], "ok");
}

#[tokio::test]
async fn rest_status_codes_empty_delete_and_terraform_fields() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({
            "datasetReference": { "datasetId": "tf_ds" },
            "description": "Terraform managed",
            "friendlyName": "TF Dataset",
            "labels": { "env": "dev" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let ds_json: JsonValue = created.json().await.unwrap();
    assert!(ds_json["etag"].is_string());
    assert!(ds_json["selfLink"].is_string());
    assert_eq!(ds_json["labels"]["env"], "dev");
    assert_eq!(ds_json["description"], "Terraform managed");

    // #514 Duplicate dataset must return 409 alreadyExists
    let dup = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "tf_ds" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), StatusCode::CONFLICT);
    let dup_json: JsonValue = dup.json().await.unwrap();
    assert_eq!(dup_json["error"]["status"], "ALREADY_EXISTS");

    // #513 DELETE must return 204 No Content with empty body
    let del = client
        .delete(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/tf_ds"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
    assert!(del.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn rest_isolation_insert_all_nested_ts_and_bytes() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    for ds in ["ds_a", "ds_b"] {
        client
            .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
            .json(&json!({ "datasetReference": { "datasetId": ds } }))
            .send()
            .await
            .unwrap();
        client
            .post(format!(
                "{base}/bigquery/v2/projects/test-proj/datasets/{ds}/tables"
            ))
            .json(&json!({
                "tableReference": { "tableId": "shared" },
                "schema": {
                    "fields": [
                        { "name": "src", "type": "STRING" },
                        { "name": "payload", "type": "BYTES" },
                        { "name": "meta", "type": "RECORD", "fields": [
                            { "name": "ts", "type": "TIMESTAMP" }
                        ]}
                    ]
                }
            }))
            .send()
            .await
            .unwrap();
    }

    let b64 = BASE64.encode(b"hello-bytes");
    client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/ds_a/tables/shared/insertAll"
        ))
        .json(&json!({
            "rows": [{ "json": {
                "src": "from_a",
                "payload": b64,
                "meta": { "ts": "2024-01-01T00:00:00Z" }
            }}]
        }))
        .send()
        .await
        .unwrap();

    // #532 ds_b.shared must remain empty
    let list_b: JsonValue = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/ds_b/tables/shared/data"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list_b["totalRows"], "0");

    // #470, #482, #498: visible to query, nested ts as epoch seconds, single base64 bytes
    let q_resp: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT src, payload, meta FROM ds_a.shared", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q_resp["rows"][0]["f"][0]["v"], "from_a");
    assert_eq!(q_resp["rows"][0]["f"][1]["v"], b64);
    assert_eq!(q_resp["rows"][0]["f"][2]["v"]["f"][0]["v"], "1704067200");
}

#[tokio::test]
async fn rest_resumable_ndjson_copy_and_dry_run() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "jobs_ds" } }))
        .send()
        .await
        .unwrap();

    let init = client
        .post(format!(
            "{base}/upload/bigquery/v2/projects/test-proj/jobs?uploadType=resumable"
        ))
        .json(&json!({
            "configuration": { "load": {
                "destinationTable": { "projectId": "test-proj", "datasetId": "jobs_ds", "tableId": "src_tbl" },
                "sourceFormat": "NEWLINE_DELIMITED_JSON",
                "schema": { "fields": [
                    { "name": "id", "type": "INT64" },
                    { "name": "events", "type": "RECORD", "mode": "REPEATED", "fields": [{ "name": "kind", "type": "STRING" }] }
                ]}
            }}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK);
    let loc = init.headers().get("location").unwrap().to_str().unwrap();
    let put_url = if loc.starts_with("http") {
        loc.to_owned()
    } else {
        format!("{base}{loc}")
    };

    let put_res = client
        .put(put_url)
        .body("{\"id\":1,\"events\":[{\"kind\":\"click\"}]}\n")
        .send()
        .await
        .unwrap();
    assert_eq!(put_res.status(), StatusCode::OK);

    let copy_res: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({
            "configuration": { "copy": {
                "sourceTable": { "projectId": "test-proj", "datasetId": "jobs_ds", "tableId": "src_tbl" },
                "destinationTable": { "projectId": "test-proj", "datasetId": "jobs_ds", "tableId": "dst_tbl" },
                "writeDisposition": "WRITE_TRUNCATE"
            }}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(copy_res["status"]["state"], "DONE");

    let dry: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT id, events FROM jobs_ds.dst_tbl", "dryRun": true, "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dry["schema"]["fields"][0]["name"], "id");
    assert_eq!(dry["schema"]["fields"][1]["mode"], "REPEATED");
}

#[tokio::test]
async fn rest_failed_script_sync_and_routines() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "jobs_ds" } }))
        .send()
        .await
        .unwrap();

    let _ = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({
            "configuration": {
                "query": { "query": "CREATE TABLE jobs_ds.partial (x INT64); SELECT * FROM jobs_ds.nonexistent;" }
            }
        }))
        .send()
        .await
        .unwrap();
    let get_partial = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/jobs_ds/tables/partial"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(get_partial.status(), StatusCode::NOT_FOUND);
    let list_partial: JsonValue = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/jobs_ds/tables"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list_partial["tables"].as_array().unwrap().len(), 0);

    let r_post = client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/jobs_ds/routines"
        ))
        .json(&json!({ "routineReference": { "routineId": "fn_one" }, "definitionBody": "42" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r_post.status(), StatusCode::OK);
    let r_del = client
        .delete(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/jobs_ds/routines/fn_one"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r_del.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn rest_partition_decorator_insert_all_and_list() {
    let base = spawn_server().await.unwrap();
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "pds" } }))
        .send()
        .await
        .unwrap();
    client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/pds/tables"
        ))
        .json(&json!({
            "tableReference": { "tableId": "events" },
            "timePartitioning": { "type": "DAY" },
            "schema": { "fields": [{ "name": "id", "type": "INT64" }] }
        }))
        .send()
        .await
        .unwrap();

    // #317: insertAll to partition decorator events$20240517
    let ins = client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/pds/tables/events$20240517/insertAll"
        ))
        .json(&json!({ "rows": [{ "json": { "id": 10 } }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(ins.status(), StatusCode::OK);

    let ins2 = client
        .post(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/pds/tables/events$20240518/insertAll"
        ))
        .json(&json!({ "rows": [{ "json": { "id": 20 } }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(ins2.status(), StatusCode::OK);

    let list_p1: JsonValue = client
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/pds/tables/events$20240517/data"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list_p1["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(list_p1["rows"][0]["f"][0]["v"], "10");

    let q_res: JsonValue = client
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT id, _PARTITIONDATE FROM pds.events ORDER BY id",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q_res["rows"][0]["f"][1]["v"], "2024-05-17");
    assert_eq!(q_res["rows"][1]["f"][1]["v"], "2024-05-18");
}

#[test]
fn cli_exec_json_does_not_double_escape_json_columns() {
    let mut out = Vec::new();
    bq_lite_server::repl::run_exec(
        "local",
        None,
        "SELECT JSON '{\"a\":1}' AS j, [1, 2] AS arr, STRUCT('x' AS k) AS st, TRUE AS flag",
        bq_lite_server::repl::OutputFormat::Json,
        &mut out,
    )
    .unwrap();
    let parsed: JsonValue = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed[0]["j"]["a"], 1);
    assert_eq!(parsed[0]["arr"][1], 2);
    assert_eq!(parsed[0]["st"]["k"], "x");
    assert_eq!(parsed[0]["flag"], true);
}
