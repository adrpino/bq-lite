use std::io::Cursor;
use std::sync::Arc;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use bq_lite_core::io::parquet_codec::export_parquet;
use bq_lite_core::{BqType, BqValue, QueryColumn, QueryResult};
use bq_lite_server::grpc::proto::{
    AppendRowsRequest, BatchCommitWriteStreamsRequest, BatchCommitWriteStreamsResponse,
    CreateReadSessionRequest, CreateWriteStreamRequest, DescriptorProto, FieldDescriptorProto,
    ProtoData, ProtoRows, ProtoSchema, ReadRowsRequest, ReadRowsResponse, ReadSession, WriteStream,
};
use bq_lite_server::repl::{run_exec, run_repl_with_io, OutputFormat};
use bq_lite_server::{build_router, ServerState};
use parking_lot::Mutex;
use prost::encoding::{encode_key, encode_varint, WireType};
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use tonic::codec::ProstCodec;
use tonic::Request;

#[test]
fn milestone_k_repl_and_exec_cover_all_formats_and_dot_commands() {
    let dir = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    run_exec(
        "local",
        Some(dir.path()),
        "CREATE SCHEMA rds; CREATE TABLE rds.t (id INT64, name STRING); INSERT INTO rds.t VALUES (1, 'a');",
        OutputFormat::Table,
        &mut out,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&out).contains("OK"));

    let mut json_out = Vec::new();
    run_exec(
        "local",
        Some(dir.path()),
        "SELECT id, name FROM rds.t",
        OutputFormat::Json,
        &mut json_out,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&json_out).contains("\"name\": \"a\""));

    let mut csv_out = Vec::new();
    run_exec(
        "local",
        Some(dir.path()),
        "SELECT id, name FROM rds.t",
        OutputFormat::Csv,
        &mut csv_out,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&csv_out).contains("1,a"));

    let script = ".help\n.tables\n.schema rds.t\n.bogus\nSELECT id\nFROM rds.t;\nBAD SQL;\n.quit\n";
    let mut reader = Cursor::new(script.as_bytes());
    let mut repl_out = Vec::new();
    run_repl_with_io("local", Some(dir.path()), &mut reader, &mut repl_out).unwrap();
    let text = String::from_utf8_lossy(&repl_out);
    assert!(text.contains("rds.t"));
    assert!(text.contains("ERROR:"));
}

async fn spawn_mock_gcs(stored: Arc<Mutex<Vec<u8>>>) -> Result<String, Box<dyn std::error::Error>> {
    let s_get = Arc::clone(&stored);
    let s_put = Arc::clone(&stored);
    let mock_gcs = Router::new()
        .route(
            "/download/storage/v1/b/{bucket}/o/{obj}",
            get(move |Path((_b, _o)): Path<(String, String)>| async move {
                (StatusCode::OK, s_get.lock().clone())
            }),
        )
        .route(
            "/upload/storage/v1/b/{bucket}/o",
            post(move |body: axum::body::Bytes| async move {
                *s_put.lock() = body.to_vec();
                StatusCode::OK
            }),
        );
    let gcs_l = TcpListener::bind("127.0.0.1:0").await?;
    let gcs_a = gcs_l.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(gcs_l, mock_gcs).await;
    });
    Ok(format!("http://{gcs_a}"))
}

#[tokio::test]
async fn milestone_k_gcs_emulator_host_http_and_jobs_filtering() {
    let stored = Arc::new(Mutex::new(Vec::new()));
    let emu_url = spawn_mock_gcs(Arc::clone(&stored)).await.unwrap();
    let qr = QueryResult::new(
        vec![QueryColumn::new("id", BqType::Int64)],
        vec![vec![BqValue::Int64(99)]],
    );
    *stored.lock() = export_parquet(&qr).unwrap();

    let mut state = ServerState::new("test-proj");
    state.storage_emulator_host = Some(emu_url);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    http.post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "gds" } }))
        .send()
        .await
        .unwrap();
    let lres = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({ "configuration": { "load": {
            "sourceUris": ["gs://bkt/in.parquet"],
            "destinationTable": { "datasetId": "gds", "tableId": "t" },
            "sourceFormat": "PARQUET"
        }}}))
        .send()
        .await
        .unwrap();
    assert_eq!(lres.status(), StatusCode::OK);

    let eres = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({ "configuration": { "extract": {
            "sourceTable": { "datasetId": "gds", "tableId": "t" },
            "destinationUris": ["gs://bkt/out.csv"],
            "destinationFormat": "CSV"
        }}}))
        .send()
        .await
        .unwrap();
    assert_eq!(eres.status(), StatusCode::OK);
    assert!(String::from_utf8_lossy(&stored.lock()).contains("99"));

    let jobs: JsonValue = http
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/jobs?stateFilter=DONE&maxResults=1"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(jobs["jobs"].as_array().map(Vec::len), Some(1));
}

async fn setup_proto_table() -> Result<(String, std::net::SocketAddr), Box<dyn std::error::Error>> {
    let state = ServerState::new("test-proj");
    let http_l = TcpListener::bind("127.0.0.1:0").await?;
    let http_a = http_l.local_addr()?;
    let st_http = state.clone();
    tokio::spawn(async move {
        let _ = axum::serve(http_l, build_router(st_http)).await;
    });
    let grpc_l = TcpListener::bind("127.0.0.1:0").await?;
    let grpc_a = grpc_l.local_addr()?;
    tokio::spawn(async move {
        let _ = bq_lite_server::grpc::serve_grpc_with_listener(grpc_l, state).await;
    });
    let http = reqwest::Client::new();
    let base = format!("http://{http_a}");
    http.post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "wds" } }))
        .send()
        .await?;
    http.post(format!(
        "{base}/bigquery/v2/projects/test-proj/datasets/wds/tables"
    ))
    .json(&json!({
        "tableReference": { "tableId": "all_proto" },
        "schema": { "fields": [
            { "name": "f_dbl", "type": "FLOAT64" },
            { "name": "f_bool", "type": "BOOL" },
            { "name": "f_bytes", "type": "BYTES" },
            { "name": "f_sint", "type": "INT64" },
            { "name": "f_nums", "type": "INT64", "mode": "REPEATED" },
            { "name": "f_rec", "type": "RECORD", "fields": [{ "name": "k", "type": "STRING" }] }
        ]}
    }))
    .send()
    .await?;
    Ok((base, grpc_a))
}

#[tokio::test]
async fn milestone_k_proto_rows_all_wire_types_and_nested_struct() {
    let (base, grpc_a) = setup_proto_table().await.unwrap();
    let http = reqwest::Client::new();

    let (desc, payload) = build_rich_proto_payload();
    let ch = tonic::transport::Endpoint::from_shared(format!("http://{grpc_a}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = tonic::client::Grpc::new(ch);
    client.ready().await.unwrap();
    let codec: ProstCodec<AppendRowsRequest, bq_lite_server::grpc::proto::AppendRowsResponse> =
        ProstCodec::default();
    let req = AppendRowsRequest {
        write_stream: "projects/test-proj/datasets/wds/tables/all_proto/streams/_default"
            .to_owned(),
        offset: None,
        proto_rows: Some(ProtoData {
            writer_schema: Some(ProtoSchema {
                proto_descriptor: Some(desc),
            }),
            rows: Some(ProtoRows {
                serialized_rows: vec![payload],
            }),
        }),
        arrow_rows: None,
    };
    let mut resp = client
        .streaming(
            Request::new(tokio_stream::iter(vec![req])),
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/google.cloud.bigquery.storage.v1.BigQueryWrite/AppendRows",
            ),
            codec,
        )
        .await
        .unwrap()
        .into_inner();
    assert!(resp.next().await.unwrap().is_ok());

    let q: JsonValue = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT f_sint, f_rec.k, ARRAY_LENGTH(f_nums) FROM wds.all_proto", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q["rows"][0]["f"][0]["v"], "-5");
    assert_eq!(q["rows"][0]["f"][1]["v"], "sub");
    assert_eq!(q["rows"][0]["f"][2]["v"], "2");
}

fn fd(
    name: &str,
    number: i32,
    label: i32,
    ftype: i32,
    tname: Option<&str>,
) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: Some(name.to_owned()),
        number: Some(number),
        label: Some(label),
        r#type: Some(ftype),
        type_name: tname.map(str::to_owned),
    }
}

fn build_rich_proto_payload() -> (DescriptorProto, Vec<u8>) {
    let mut sub = Vec::new();
    encode_key(1, WireType::LengthDelimited, &mut sub);
    encode_varint(3, &mut sub);
    sub.extend_from_slice(b"sub");
    let mut packed = Vec::new();
    encode_varint(10, &mut packed);
    encode_varint(20, &mut packed);

    let mut row = Vec::new();
    encode_key(1, WireType::SixtyFourBit, &mut row);
    row.extend_from_slice(&3.5_f64.to_le_bytes());
    encode_key(2, WireType::Varint, &mut row);
    encode_varint(1, &mut row);
    encode_key(3, WireType::LengthDelimited, &mut row);
    encode_varint(2, &mut row);
    row.extend_from_slice(b"hi");
    encode_key(4, WireType::Varint, &mut row);
    encode_varint(9, &mut row);
    encode_key(5, WireType::LengthDelimited, &mut row);
    encode_varint(packed.len() as u64, &mut row);
    row.extend_from_slice(&packed);
    encode_key(6, WireType::LengthDelimited, &mut row);
    encode_varint(sub.len() as u64, &mut row);
    row.extend_from_slice(&sub);

    let desc = DescriptorProto {
        name: Some("RichRow".to_owned()),
        field: vec![
            fd("f_dbl", 1, 1, 1, None),
            fd("f_bool", 2, 1, 8, None),
            fd("f_bytes", 3, 1, 12, None),
            fd("f_sint", 4, 1, 18, None),
            fd("f_nums", 5, 3, 3, None),
            fd("f_rec", 6, 1, 11, Some(".Sub")),
        ],
        nested_type: vec![DescriptorProto {
            name: Some("Sub".to_owned()),
            field: vec![fd("k", 1, 1, 9, None)],
            nested_type: Vec::new(),
        }],
    };
    (desc, row)
}

async fn setup_avro_server() -> Result<(String, std::net::SocketAddr), Box<dyn std::error::Error>> {
    let state = ServerState::new("test-proj");
    let http_l = TcpListener::bind("127.0.0.1:0").await?;
    let http_a = http_l.local_addr()?;
    let st_http = state.clone();
    tokio::spawn(async move {
        let _ = axum::serve(http_l, build_router(st_http)).await;
    });
    let grpc_l = TcpListener::bind("127.0.0.1:0").await?;
    let grpc_a = grpc_l.local_addr()?;
    tokio::spawn(async move {
        let _ = bq_lite_server::grpc::serve_grpc_with_listener(grpc_l, state).await;
    });
    let http = reqwest::Client::new();
    let base = format!("http://{http_a}");
    http.post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "ads" } }))
        .send()
        .await?;
    http.post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "CREATE TABLE ads.rich AS SELECT
                        1 AS i, 2.5 AS f, TRUE AS b, 's' AS s, b'ab' AS by,
                        NUMERIC '3.14' AS num, DATE '2026-01-01' AS d,
                        TIMESTAMP '2026-01-01 00:00:00 UTC' AS ts,
                        [1, 2] AS arr, STRUCT(9 AS k) AS st
                      UNION ALL
                      SELECT NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, [], NULL",
            "useLegacySql": false
        }))
        .send()
        .await?;
    Ok((base, grpc_a))
}

#[tokio::test]
async fn milestone_k_avro_all_types_rest_params_and_write_commit_errors() {
    let (base, grpc_a) = setup_avro_server().await.unwrap();
    let http = reqwest::Client::new();

    let p_res: JsonValue = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "SELECT @st.x, ARRAY_LENGTH(@arr)",
            "useLegacySql": false,
            "parameterMode": "NAMED",
            "queryParameters": [
                {
                    "name": "st",
                    "parameterType": { "type": "STRUCT", "structTypes": [{ "name": "x", "type": { "type": "INT64" } }] },
                    "parameterValue": { "structValues": { "x": { "value": "77" } } }
                },
                {
                    "name": "arr",
                    "parameterType": { "type": "ARRAY", "arrayType": { "type": "STRING" } },
                    "parameterValue": { "arrayValues": [{ "value": "a" }, { "value": "b" }] }
                }
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(p_res["rows"][0]["f"][0]["v"], "77");
    assert_eq!(p_res["rows"][0]["f"][1]["v"], "2");

    let ch = tonic::transport::Endpoint::from_shared(format!("http://{grpc_a}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = tonic::client::Grpc::new(ch);
    client.ready().await.unwrap();
    verify_avro_read(&mut client).await.unwrap();
    verify_write_commit(&mut client).await.unwrap();
}

async fn verify_avro_read(
    client: &mut tonic::client::Grpc<tonic::transport::Channel>,
) -> Result<(), Box<dyn std::error::Error>> {
    client.ready().await?;
    let create_codec: ProstCodec<CreateReadSessionRequest, ReadSession> = ProstCodec::default();
    let session = client
        .unary(
            Request::new(CreateReadSessionRequest {
                parent: "projects/test-proj".to_owned(),
                read_session: Some(ReadSession {
                    name: String::new(),
                    expire_time: None,
                    data_format: 1,
                    table: "projects/test-proj/datasets/ads/tables/rich".to_owned(),
                    read_options: None,
                    streams: Vec::new(),
                    estimated_row_count: 0,
                    arrow_schema: None,
                    avro_schema: None,
                }),
                max_stream_count: 1,
            }),
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/google.cloud.bigquery.storage.v1.BigQueryRead/CreateReadSession",
            ),
            create_codec,
        )
        .await?
        .into_inner();
    assert!(session.avro_schema.is_some());
    let first_stream = session.streams.first().ok_or("no streams")?.name.clone();
    client.ready().await?;
    let read_codec: ProstCodec<ReadRowsRequest, ReadRowsResponse> = ProstCodec::default();
    let mut stream = client
        .server_streaming(
            Request::new(ReadRowsRequest {
                read_stream: first_stream,
                offset: 0,
            }),
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/google.cloud.bigquery.storage.v1.BigQueryRead/ReadRows",
            ),
            read_codec,
        )
        .await?
        .into_inner();
    assert_eq!(stream.next().await.ok_or("missing chunk")??.row_count, 2);
    Ok(())
}

async fn verify_write_commit(
    client: &mut tonic::client::Grpc<tonic::transport::Channel>,
) -> Result<(), Box<dyn std::error::Error>> {
    client.ready().await?;
    let ws_codec: ProstCodec<CreateWriteStreamRequest, WriteStream> = ProstCodec::default();
    let ws = client
        .unary(
            Request::new(CreateWriteStreamRequest {
                parent: "projects/test-proj/datasets/ads/tables/rich".to_owned(),
                write_stream: Some(WriteStream {
                    name: String::new(),
                    r#type: 2,
                    create_time: None,
                    commit_time: None,
                    table_schema: None,
                    location: String::new(),
                }),
            }),
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/google.cloud.bigquery.storage.v1.BigQueryWrite/CreateWriteStream",
            ),
            ws_codec,
        )
        .await?
        .into_inner();
    client.ready().await?;
    let commit_codec: ProstCodec<BatchCommitWriteStreamsRequest, BatchCommitWriteStreamsResponse> =
        ProstCodec::default();
    let cres = client
        .unary(
            Request::new(BatchCommitWriteStreamsRequest {
                parent: "projects/test-proj/datasets/ads/tables/rich".to_owned(),
                write_streams: vec![ws.name, "missing_stream".to_owned()],
            }),
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/google.cloud.bigquery.storage.v1.BigQueryWrite/BatchCommitWriteStreams",
            ),
            commit_codec,
        )
        .await?
        .into_inner();
    assert_eq!(cres.stream_errors.len(), 2);
    Ok(())
}
