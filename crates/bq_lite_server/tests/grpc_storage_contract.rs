use bq_lite_core::io::arrow_codec::{
    arrow_storage_parts_to_query_result, query_result_to_arrow_storage_parts,
};
use bq_lite_core::{BqResult, BqType, BqValue, QueryColumn, QueryResult};
use bq_lite_server::grpc::proto::{
    AppendRowsRequest, ArrowData, ArrowRecordBatch, ArrowSchema, BatchCommitWriteStreamsRequest,
    CreateReadSessionRequest, CreateWriteStreamRequest, FinalizeWriteStreamRequest,
    FlushRowsRequest, Int64Value, ReadRowsRequest, ReadSession, SplitReadStreamRequest,
    TableReadOptions, WriteStream,
};
use bq_lite_server::grpc::serve_grpc_with_listener;
use bq_lite_server::{build_router, ServerState};
use reqwest::StatusCode;
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use tonic::codec::ProstCodec;
use tonic::Request;

async fn spawn_http_and_grpc() -> Result<(String, String, ServerState), Box<dyn std::error::Error>>
{
    let state = ServerState::new("test-proj");
    let http_listener = TcpListener::bind("127.0.0.1:0").await?;
    let http_addr = http_listener.local_addr()?;
    let grpc_listener = TcpListener::bind("127.0.0.1:0").await?;
    let grpc_addr = grpc_listener.local_addr()?;

    let s1 = state.clone();
    tokio::spawn(async move {
        let _ = axum::serve(http_listener, build_router(s1)).await;
    });
    let s2 = state.clone();
    tokio::spawn(async move {
        let _ = serve_grpc_with_listener(grpc_listener, s2).await;
    });

    Ok((
        format!("http://{http_addr}"),
        format!("http://{grpc_addr}"),
        state,
    ))
}

async fn unary_rpc<Req, Resp>(
    channel: &tonic::transport::Channel,
    path: &'static str,
    req: Req,
) -> Result<Resp, Box<tonic::Status>>
where
    Req: prost::Message + Default + 'static,
    Resp: prost::Message + Default + 'static,
{
    let mut client = tonic::client::Grpc::new(channel.clone());
    client
        .ready()
        .await
        .map_err(|e| Box::new(tonic::Status::unavailable(e.to_string())))?;
    let codec: ProstCodec<Req, Resp> = ProstCodec::default();
    let p = tonic::codegen::http::uri::PathAndQuery::from_static(path);
    client
        .unary(Request::new(req), p, codec)
        .await
        .map(tonic::Response::into_inner)
        .map_err(Box::new)
}

async fn read_rows_rpc(
    channel: &tonic::transport::Channel,
    stream_name: &str,
    offset: i64,
) -> Result<Vec<bq_lite_server::grpc::proto::ReadRowsResponse>, Box<tonic::Status>> {
    let mut client = tonic::client::Grpc::new(channel.clone());
    client
        .ready()
        .await
        .map_err(|e| Box::new(tonic::Status::unavailable(e.to_string())))?;
    let codec: ProstCodec<ReadRowsRequest, bq_lite_server::grpc::proto::ReadRowsResponse> =
        ProstCodec::default();
    let p = tonic::codegen::http::uri::PathAndQuery::from_static(
        "/google.cloud.bigquery.storage.v1.BigQueryRead/ReadRows",
    );
    let mut stream = client
        .server_streaming(
            Request::new(ReadRowsRequest {
                read_stream: stream_name.to_owned(),
                offset,
            }),
            p,
            codec,
        )
        .await
        .map_err(Box::new)?
        .into_inner();
    drop(client);
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.map_err(Box::new)?);
    }
    Ok(out)
}

async fn setup_dataset_table(
    http: &reqwest::Client,
    http_base: &str,
    ds: &str,
    tbl: &str,
    fields: JsonValue,
) -> Result<(), Box<dyn std::error::Error>> {
    http.post(format!(
        "{http_base}/bigquery/v2/projects/test-proj/datasets"
    ))
    .json(&json!({ "datasetReference": { "datasetId": ds } }))
    .send()
    .await?;
    http.post(format!(
        "{http_base}/bigquery/v2/projects/test-proj/datasets/{ds}/tables"
    ))
    .json(&json!({
        "tableReference": { "tableId": tbl },
        "schema": { "fields": fields }
    }))
    .send()
    .await?;
    Ok(())
}

#[tokio::test]
async fn grpc_storage_read_projects_arrays_and_honors_offset() {
    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    setup_dataset_table(
        &http,
        &http_base,
        "gds",
        "items",
        json!([
            { "name": "id", "type": "INT64" },
            { "name": "tags", "type": "STRING", "mode": "REPEATED" }
        ]),
    )
    .await
    .unwrap();
    http.post(format!(
        "{http_base}/bigquery/v2/projects/test-proj/queries"
    ))
    .json(&json!({
        "query": "INSERT INTO gds.items VALUES (1, ['a']), (2, ['b', 'c']), (3, ['d'])",
        "useLegacySql": false
    }))
    .send()
    .await
    .unwrap();

    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let session: ReadSession = unary_rpc(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryRead/CreateReadSession",
        CreateReadSessionRequest {
            parent: "projects/test-proj".to_owned(),
            read_session: Some(ReadSession {
                name: String::new(),
                expire_time: None,
                data_format: 2,
                avro_schema: None,
                arrow_schema: None,
                table: "projects/test-proj/datasets/gds/tables/items".to_owned(),
                read_options: Some(TableReadOptions {
                    selected_fields: vec!["id".to_owned(), "tags".to_owned()],
                    row_restriction: "id >= 2".to_owned(),
                    sample_percentage: None,
                }),
                streams: Vec::new(),
                estimated_row_count: 0,
            }),
            max_stream_count: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(session.estimated_row_count, 2);
    let schema_bytes = session.arrow_schema.unwrap().serialized_schema;
    let stream_name = session.streams[0].name.clone();

    let _split: bq_lite_server::grpc::proto::SplitReadStreamResponse = unary_rpc(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryRead/SplitReadStream",
        SplitReadStreamRequest {
            name: stream_name.clone(),
            fraction: 0.5,
        },
    )
    .await
    .unwrap();

    let resps = read_rows_rpc(&channel, &stream_name, 1).await.unwrap();
    drop(channel);
    assert_eq!(resps.len(), 1);
    let batch_bytes = resps[0]
        .arrow_record_batch
        .as_ref()
        .unwrap()
        .serialized_record_batch
        .clone();
    let decoded = arrow_storage_parts_to_query_result(&schema_bytes, &batch_bytes).unwrap();
    assert_eq!(decoded.rows.len(), 1);
    assert_eq!(decoded.rows[0][0], BqValue::Int64(3));
}

#[tokio::test]
async fn grpc_storage_read_supports_anonymous_job_results() {
    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    let q_resp: JsonValue = http
        .post(format!(
            "{http_base}/bigquery/v2/projects/test-proj/queries"
        ))
        .json(&json!({ "query": "SELECT 77 AS magic", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let jid = q_resp["jobReference"]["jobId"].as_str().unwrap();

    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let anon_sess: ReadSession = unary_rpc(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryRead/CreateReadSession",
        CreateReadSessionRequest {
            parent: "projects/test-proj".to_owned(),
            read_session: Some(ReadSession {
                name: String::new(),
                expire_time: None,
                data_format: 2,
                avro_schema: None,
                arrow_schema: None,
                table: format!("projects/test-proj/datasets/_bqlite_jobs/tables/{jid}"),
                read_options: None,
                streams: Vec::new(),
                estimated_row_count: 0,
            }),
            max_stream_count: 1,
        },
    )
    .await
    .unwrap();
    drop(channel);
    assert_eq!(anon_sess.estimated_row_count, 1);
}

#[tokio::test]
async fn grpc_storage_write_committed_pending_and_not_found() {
    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    setup_dataset_table(
        &http,
        &http_base,
        "wds",
        "logs",
        json!([{ "name": "id", "type": "INT64" }]),
    )
    .await
    .unwrap();

    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let missing_err = unary_rpc::<CreateWriteStreamRequest, WriteStream>(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/CreateWriteStream",
        CreateWriteStreamRequest {
            parent: "projects/test-proj/datasets/wds/tables/nope".to_owned(),
            write_stream: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(missing_err.code(), tonic::Code::NotFound);

    let table_parent = "projects/test-proj/datasets/wds/tables/logs";
    let (schema_bytes, b1) = make_id_batch(&[10]).unwrap();
    let (_, b2) = make_id_batch(&[20]).unwrap();

    let ws_comm = create_ws(&channel, table_parent, 1).await.unwrap();
    append_one_rpc(&channel, &ws_comm.name, &schema_bytes, &b1, Some(0))
        .await
        .unwrap();

    let ws_p1 = create_ws(&channel, table_parent, 2).await.unwrap();
    let ws_p2 = create_ws(&channel, table_parent, 2).await.unwrap();
    append_one_rpc(&channel, &ws_p1.name, &schema_bytes, &b2, Some(0))
        .await
        .unwrap();
    finalize_ws(&channel, &ws_p1.name).await.unwrap();
    append_one_rpc(&channel, &ws_p2.name, &schema_bytes, &b2, Some(0))
        .await
        .unwrap();
    finalize_ws(&channel, &ws_p2.name).await.unwrap();

    let commit_res: bq_lite_server::grpc::proto::BatchCommitWriteStreamsResponse = unary_rpc(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/BatchCommitWriteStreams",
        BatchCommitWriteStreamsRequest {
            parent: table_parent.to_owned(),
            write_streams: vec![ws_p1.name, ws_p2.name],
        },
    )
    .await
    .unwrap();
    drop(channel);
    assert_eq!(commit_res.stream_errors.len(), 0);

    let count_q: JsonValue = http
        .post(format!(
            "{http_base}/bigquery/v2/projects/test-proj/queries"
        ))
        .json(&json!({ "query": "SELECT COUNT(*) FROM wds.logs", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(count_q["rows"][0]["f"][0]["v"], "3");
}

#[tokio::test]
async fn grpc_storage_write_buffered_flush_rows_honors_offsets() {
    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    setup_dataset_table(
        &http,
        &http_base,
        "bds",
        "buf",
        json!([{ "name": "id", "type": "INT64" }]),
    )
    .await
    .unwrap();

    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let table_parent = "projects/test-proj/datasets/bds/tables/buf";
    let (schema_bytes, batch) = make_id_batch(&[30, 40]).unwrap();
    let ws_buf = create_ws(&channel, table_parent, 3).await.unwrap();
    append_one_rpc(&channel, &ws_buf.name, &schema_bytes, &batch, Some(0))
        .await
        .unwrap();

    for off in [0_i64, 1] {
        let _: bq_lite_server::grpc::proto::FlushRowsResponse = unary_rpc(
            &channel,
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/FlushRows",
            FlushRowsRequest {
                write_stream: ws_buf.name.clone(),
                offset: Some(Int64Value { value: off }),
            },
        )
        .await
        .unwrap();
    }
    drop(channel);

    let count_q: JsonValue = http
        .post(format!(
            "{http_base}/bigquery/v2/projects/test-proj/queries"
        ))
        .json(&json!({ "query": "SELECT COUNT(*) FROM bds.buf", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(count_q["rows"][0]["f"][0]["v"], "2");
}

#[tokio::test]
async fn rest_gcs_bridge_loads_and_extracts_parquet_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = ServerState::with_data_dir("test-proj", dir.path().to_path_buf()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    http.post(format!("{base}/bigquery/v2/projects/test-proj/datasets"))
        .json(&json!({ "datasetReference": { "datasetId": "pq_ds" } }))
        .send()
        .await
        .unwrap();
    http.post(format!("{base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({
            "query": "CREATE TABLE pq_ds.src (id INT64, name STRING); INSERT INTO pq_ds.src VALUES (1, 'ada'), (2, 'bob');",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap();

    let ext: JsonValue = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({
            "configuration": { "extract": {
                "sourceTable": { "projectId": "test-proj", "datasetId": "pq_ds", "tableId": "src" },
                "destinationUris": ["gs://my-bucket/out.parquet"],
                "destinationFormat": "PARQUET"
            }}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ext["status"]["state"], "DONE");
    assert!(dir.path().join("gcs/my-bucket/out.parquet").exists());

    let ld: JsonValue = http
        .post(format!("{base}/bigquery/v2/projects/test-proj/jobs"))
        .json(&json!({
            "configuration": { "load": {
                "sourceUris": ["gs://my-bucket/out.parquet"],
                "sourceFormat": "PARQUET",
                "destinationTable": { "projectId": "test-proj", "datasetId": "pq_ds", "tableId": "reloaded" }
            }}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ld["status"]["state"], "DONE");
    let check_res = http
        .get(format!(
            "{base}/bigquery/v2/projects/test-proj/datasets/pq_ds/tables/reloaded"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(check_res.status(), StatusCode::OK);
    let tbl_json: JsonValue = check_res.json().await.unwrap();
    assert_eq!(tbl_json["numRows"], "2");
}

async fn create_ws(
    channel: &tonic::transport::Channel,
    parent: &str,
    stream_type: i32,
) -> Result<WriteStream, Box<tonic::Status>> {
    unary_rpc(
        channel,
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/CreateWriteStream",
        CreateWriteStreamRequest {
            parent: parent.to_owned(),
            write_stream: Some(WriteStream {
                name: String::new(),
                r#type: stream_type,
                create_time: None,
                commit_time: None,
                table_schema: None,
                location: String::new(),
            }),
        },
    )
    .await
}

async fn finalize_ws(
    channel: &tonic::transport::Channel,
    name: &str,
) -> Result<(), Box<tonic::Status>> {
    let _: bq_lite_server::grpc::proto::FinalizeWriteStreamResponse = unary_rpc(
        channel,
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/FinalizeWriteStream",
        FinalizeWriteStreamRequest {
            name: name.to_owned(),
        },
    )
    .await?;
    Ok(())
}

fn make_id_batch(ids: &[i64]) -> BqResult<(Vec<u8>, Vec<u8>)> {
    let rows = ids.iter().map(|&i| vec![BqValue::Int64(i)]).collect();
    let qr = QueryResult::new(vec![QueryColumn::new("id", BqType::Int64)], rows);
    query_result_to_arrow_storage_parts(&qr)
}

async fn append_one_rpc(
    channel: &tonic::transport::Channel,
    stream_name: &str,
    schema_bytes: &[u8],
    batch_bytes: &[u8],
    offset: Option<i64>,
) -> Result<(), Box<tonic::Status>> {
    let mut client = tonic::client::Grpc::new(channel.clone());
    client
        .ready()
        .await
        .map_err(|e| Box::new(tonic::Status::unavailable(e.to_string())))?;
    let codec: ProstCodec<AppendRowsRequest, bq_lite_server::grpc::proto::AppendRowsResponse> =
        ProstCodec::default();
    let p = tonic::codegen::http::uri::PathAndQuery::from_static(
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/AppendRows",
    );
    let req = AppendRowsRequest {
        write_stream: stream_name.to_owned(),
        offset: offset.map(|v| Int64Value { value: v }),
        proto_rows: None,
        arrow_rows: Some(ArrowData {
            writer_schema: Some(ArrowSchema {
                serialized_schema: schema_bytes.to_vec(),
            }),
            rows: Some(ArrowRecordBatch {
                serialized_record_batch: batch_bytes.to_vec(),
                row_count: 1,
            }),
        }),
    };
    let mut resp_stream = client
        .streaming(Request::new(tokio_stream::iter(vec![req])), p, codec)
        .await
        .map_err(Box::new)?
        .into_inner();
    drop(client);
    if let Some(first) = resp_stream.next().await {
        let _ = first.map_err(Box::new)?;
    }
    Ok(())
}

async fn create_read_session_with_fmt(
    channel: &tonic::transport::Channel,
    table: &str,
    data_format: i32,
    row_restriction: &str,
    max_stream_count: i32,
) -> Result<ReadSession, Box<tonic::Status>> {
    unary_rpc(
        channel,
        "/google.cloud.bigquery.storage.v1.BigQueryRead/CreateReadSession",
        CreateReadSessionRequest {
            parent: "projects/test-proj".to_owned(),
            read_session: Some(ReadSession {
                name: String::new(),
                expire_time: None,
                data_format,
                avro_schema: None,
                arrow_schema: None,
                table: table.to_owned(),
                read_options: Some(TableReadOptions {
                    selected_fields: vec!["id".to_owned()],
                    row_restriction: row_restriction.to_owned(),
                    sample_percentage: None,
                }),
                streams: Vec::new(),
                estimated_row_count: 0,
            }),
            max_stream_count,
        },
    )
    .await
}

#[tokio::test]
async fn grpc_storage_read_avro_multistream_split_and_anon_row_restriction() {
    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    let q_resp: JsonValue = http
        .post(format!(
            "{http_base}/bigquery/v2/projects/test-proj/queries"
        ))
        .json(&json!({
            "query": "SELECT id FROM UNNEST([1, 2, 3, 4]) AS id ORDER BY id",
            "useLegacySql": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let jid = q_resp["jobReference"]["jobId"].as_str().unwrap();
    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();

    let avro_sess = create_read_session_with_fmt(
        &channel,
        &format!("projects/test-proj/datasets/_bqlite_jobs/tables/_anon_{jid}"),
        1,
        "id >= 2",
        2,
    )
    .await
    .unwrap();
    assert_eq!(avro_sess.estimated_row_count, 3);
    assert!(avro_sess.avro_schema.is_some());
    assert_eq!(avro_sess.streams.len(), 2);
    let r0 = read_rows_rpc(&channel, &avro_sess.streams[0].name, 0)
        .await
        .unwrap();
    let r1 = read_rows_rpc(&channel, &avro_sess.streams[1].name, 0)
        .await
        .unwrap();
    assert_eq!(r0[0].row_count + r1[0].row_count, 3);
    assert!(r0[0].avro_rows.is_some());

    let arrow_sess = create_read_session_with_fmt(
        &channel,
        &format!("projects/test-proj/datasets/_bqlite_jobs/tables/{jid}"),
        2,
        "",
        1,
    )
    .await
    .unwrap();
    let split: bq_lite_server::grpc::proto::SplitReadStreamResponse = unary_rpc(
        &channel,
        "/google.cloud.bigquery.storage.v1.BigQueryRead/SplitReadStream",
        SplitReadStreamRequest {
            name: arrow_sess.streams[0].name.clone(),
            fraction: 0.5,
        },
    )
    .await
    .unwrap();
    let p_rows = read_rows_rpc(&channel, &split.primary_stream.unwrap().name, 0)
        .await
        .unwrap();
    let rem_rows = read_rows_rpc(&channel, &split.remainder_stream.unwrap().name, 0)
        .await
        .unwrap();
    drop(channel);
    assert_eq!(p_rows[0].row_count, 2);
    assert_eq!(rem_rows[0].row_count, 2);
}

fn build_sample_proto_payload() -> (bq_lite_server::grpc::proto::DescriptorProto, Vec<u8>) {
    use bq_lite_server::grpc::proto::{DescriptorProto, FieldDescriptorProto};
    use prost::encoding::{encode_key, encode_varint, WireType};

    let mut row_bytes = Vec::new();
    encode_key(1, WireType::Varint, &mut row_bytes);
    encode_varint(42, &mut row_bytes);
    encode_key(2, WireType::LengthDelimited, &mut row_bytes);
    encode_varint(5, &mut row_bytes);
    row_bytes.extend_from_slice(b"grace");
    encode_key(3, WireType::LengthDelimited, &mut row_bytes);
    encode_varint(2, &mut row_bytes);
    row_bytes.extend_from_slice(b"go");

    let desc = DescriptorProto {
        name: Some("Row".to_owned()),
        field: vec![
            FieldDescriptorProto {
                name: Some("id".to_owned()),
                number: Some(1),
                label: Some(1),
                r#type: Some(3),
                type_name: None,
            },
            FieldDescriptorProto {
                name: Some("name".to_owned()),
                number: Some(2),
                label: Some(1),
                r#type: Some(9),
                type_name: None,
            },
            FieldDescriptorProto {
                name: Some("tags".to_owned()),
                number: Some(3),
                label: Some(3),
                r#type: Some(9),
                type_name: None,
            },
        ],
        nested_type: Vec::new(),
    };
    (desc, row_bytes)
}

#[tokio::test]
async fn grpc_storage_write_proto_rows_and_default_stream() {
    use bq_lite_server::grpc::proto::{ProtoData, ProtoRows, ProtoSchema};

    let (http_base, grpc_base, _state) = spawn_http_and_grpc().await.unwrap();
    let http = reqwest::Client::new();
    http.post(format!(
        "{http_base}/bigquery/v2/projects/test-proj/datasets"
    ))
    .json(&json!({ "datasetReference": { "datasetId": "pds" } }))
    .send()
    .await
    .unwrap();
    http.post(format!(
        "{http_base}/bigquery/v2/projects/test-proj/datasets/pds/tables"
    ))
    .json(&json!({
        "tableReference": { "tableId": "events" },
        "schema": { "fields": [
            { "name": "id", "type": "INT64" },
            { "name": "name", "type": "STRING" },
            { "name": "tags", "type": "STRING", "mode": "REPEATED" }
        ]}
    }))
    .send()
    .await
    .unwrap();

    let (desc, row_bytes) = build_sample_proto_payload();
    let channel = tonic::transport::Endpoint::from_shared(grpc_base)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = tonic::client::Grpc::new(channel.clone());
    client.ready().await.unwrap();
    let codec: ProstCodec<AppendRowsRequest, bq_lite_server::grpc::proto::AppendRowsResponse> =
        ProstCodec::default();
    let p = tonic::codegen::http::uri::PathAndQuery::from_static(
        "/google.cloud.bigquery.storage.v1.BigQueryWrite/AppendRows",
    );
    let req = AppendRowsRequest {
        write_stream: "projects/test-proj/datasets/pds/tables/events/streams/_default".to_owned(),
        offset: None,
        proto_rows: Some(ProtoData {
            writer_schema: Some(ProtoSchema {
                proto_descriptor: Some(desc),
            }),
            rows: Some(ProtoRows {
                serialized_rows: vec![row_bytes],
            }),
        }),
        arrow_rows: None,
    };
    let mut resp_stream = client
        .streaming(Request::new(tokio_stream::iter(vec![req])), p, codec)
        .await
        .unwrap()
        .into_inner();
    assert!(resp_stream.next().await.unwrap().is_ok());
    drop(client);
    drop(channel);

    let q_res: JsonValue = http
        .post(format!("{http_base}/bigquery/v2/projects/test-proj/queries"))
        .json(&json!({ "query": "SELECT id, name, ARRAY_LENGTH(tags) FROM pds.events", "useLegacySql": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(q_res["rows"][0]["f"][0]["v"], "42");
    assert_eq!(q_res["rows"][0]["f"][1]["v"], "grace");
    assert_eq!(q_res["rows"][0]["f"][2]["v"], "1");
}
