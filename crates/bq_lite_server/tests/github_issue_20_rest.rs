use bq_lite_server::{build_router, ServerState};
use reqwest::StatusCode;
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;

async fn spawn_test_server() -> std::io::Result<String> {
    let state = ServerState::new("p");
    let app = build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

#[tokio::test]
async fn issue_20_rest_temp_function_and_table_do_not_persist_across_jobs() {
    let base = spawn_test_server().await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("{base}/bigquery/v2/projects/p/queries");

    // Job 1: CREATE TEMP FUNCTION + SELECT
    let r1 = client
        .post(&url)
        .json(&json!({
            "query": "CREATE TEMP FUNCTION f(x INT64) AS (x + 1); SELECT f(1) AS v;",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let b1: JsonValue = r1.json().await.unwrap();
    assert_eq!(b1["rows"][0]["f"][0]["v"], "2");

    // Job 2: Identical request must also return 2 (no Already Exists)
    let r2 = client
        .post(&url)
        .json(&json!({
            "query": "CREATE TEMP FUNCTION f(x INT64) AS (x + 1); SELECT f(1) AS v;",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let b2: JsonValue = r2.json().await.unwrap();
    assert_eq!(b2["rows"][0]["f"][0]["v"], "2");

    // Job 3: Calling f without defining it must fail
    let r3 = client
        .post(&url)
        .json(&json!({
            "query": "SELECT f(1) AS v;",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();
    assert_ne!(r3.status(), StatusCode::OK);

    // Same for CREATE TEMP TABLE across REST jobs
    let t1 = client
        .post(&url)
        .json(&json!({
            "query": "CREATE TEMP TABLE tmp AS SELECT 7 AS n; SELECT n FROM tmp;",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(t1.status(), StatusCode::OK);
    let tb1: JsonValue = t1.json().await.unwrap();
    assert_eq!(tb1["rows"][0]["f"][0]["v"], "7");

    let t2 = client
        .post(&url)
        .json(&json!({
            "query": "SELECT n FROM tmp;",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();
    assert_ne!(t2.status(), StatusCode::OK);
}
