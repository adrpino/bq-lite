use std::task::{Context, Poll};

use bq_lite_core::io::arrow_codec::arrow_storage_parts_to_query_result;
use bq_lite_core::BqValue;
use chrono::Utc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::body::Body;
use tonic::codegen::{http, BoxFuture, Service};
use tonic::server::{Grpc, NamedService, StreamingService, UnaryService};
use tonic::{Request, Response, Status, Streaming};

use crate::state::{ServerState, WriteStreamRecord, WriteStreamType};

use super::proto::{
    columns_to_table_schema, AppendResult, AppendRowsRequest, AppendRowsResponse,
    BatchCommitWriteStreamsRequest, BatchCommitWriteStreamsResponse, CreateWriteStreamRequest,
    DescriptorProto, FinalizeWriteStreamRequest, FinalizeWriteStreamResponse, FlushRowsRequest,
    FlushRowsResponse, GetWriteStreamRequest, Int64Value, Timestamp, WriteStream,
};
use super::proto_rows::decode_proto_rows;
use super::read::{bq_err_to_status, parse_table_resource};
use super::write_commit::{handle_batch_commit, handle_finalize, handle_flush_rows};

#[derive(Debug, Clone)]
pub struct BigQueryWriteService {
    state: ServerState,
}

impl BigQueryWriteService {
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }
}

impl NamedService for BigQueryWriteService {
    const NAME: &'static str = "google.cloud.bigquery.storage.v1.BigQueryWrite";
}

impl Service<http::Request<Body>> for BigQueryWriteService {
    type Response = http::Response<Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let svc = self.clone();
        match req.uri().path() {
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/CreateWriteStream" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(CreateWriteStreamSvc(svc), req).await)
                })
            }
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/GetWriteStream" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(GetWriteStreamSvc(svc), req).await)
                })
            }
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/AppendRows" => Box::pin(async move {
                let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                Ok(grpc.streaming(AppendRowsSvc(svc), req).await)
            }),
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/FinalizeWriteStream" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(FinalizeWriteStreamSvc(svc), req).await)
                })
            }
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/FlushRows" => Box::pin(async move {
                let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                Ok(grpc.unary(FlushRowsSvc(svc), req).await)
            }),
            "/google.cloud.bigquery.storage.v1.BigQueryWrite/BatchCommitWriteStreams" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(BatchCommitSvc(svc), req).await)
                })
            }
            _ => Box::pin(async move {
                Ok(Status::unimplemented("Unknown BigQueryWrite RPC").into_http())
            }),
        }
    }
}

struct CreateWriteStreamSvc(BigQueryWriteService);
impl UnaryService<CreateWriteStreamRequest> for CreateWriteStreamSvc {
    type Response = WriteStream;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<CreateWriteStreamRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(
            async move { handle_create_write_stream(&state, req.into_inner()).map_err(|e| *e) },
        )
    }
}

struct GetWriteStreamSvc(BigQueryWriteService);
impl UnaryService<GetWriteStreamRequest> for GetWriteStreamSvc {
    type Response = WriteStream;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<GetWriteStreamRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(
            async move { handle_get_write_stream(&state, &req.into_inner().name).map_err(|e| *e) },
        )
    }
}

struct AppendRowsSvc(BigQueryWriteService);
impl StreamingService<AppendRowsRequest> for AppendRowsSvc {
    type Response = AppendRowsResponse;
    type ResponseStream = ReceiverStream<Result<AppendRowsResponse, Status>>;
    type Future = BoxFuture<Response<Self::ResponseStream>, Status>;

    fn call(&mut self, req: Request<Streaming<AppendRowsRequest>>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move { Ok(handle_append_rows(&state, req.into_inner())) })
    }
}

struct FinalizeWriteStreamSvc(BigQueryWriteService);
impl UnaryService<FinalizeWriteStreamRequest> for FinalizeWriteStreamSvc {
    type Response = FinalizeWriteStreamResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<FinalizeWriteStreamRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move { handle_finalize(&state, &req.into_inner().name).map_err(|e| *e) })
    }
}

struct FlushRowsSvc(BigQueryWriteService);
impl UnaryService<FlushRowsRequest> for FlushRowsSvc {
    type Response = FlushRowsResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<FlushRowsRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move { handle_flush_rows(&state, &req.into_inner()).map_err(|e| *e) })
    }
}

struct BatchCommitSvc(BigQueryWriteService);
impl UnaryService<BatchCommitWriteStreamsRequest> for BatchCommitSvc {
    type Response = BatchCommitWriteStreamsResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<BatchCommitWriteStreamsRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move { handle_batch_commit(&state, &req.into_inner()).map_err(|e| *e) })
    }
}

fn handle_create_write_stream(
    state: &ServerState,
    req: CreateWriteStreamRequest,
) -> Result<Response<WriteStream>, Box<Status>> {
    let tref = parse_table_resource(&req.parent)?;
    let cols = {
        let eng = state.engine.read();
        let meta = eng
            .catalog()
            .get_table(&tref)
            .map_err(|e| bq_err_to_status(&e))?
            .clone();
        drop(eng);
        meta.columns
    };
    let req_ws = req.write_stream.unwrap_or(WriteStream {
        name: String::new(),
        r#type: 1,
        create_time: None,
        commit_time: None,
        table_schema: None,
        location: String::new(),
    });
    let stype = match req_ws.r#type {
        2 => WriteStreamType::Pending,
        3 => WriteStreamType::Buffered,
        _ => WriteStreamType::Committed,
    };
    let stream_id = format!("ws_{}", uuid::Uuid::new_v4());
    let name = format!("{}/streams/{stream_id}", req.parent);
    let now = Utc::now();

    state.record_write_stream(
        name.clone(),
        WriteStreamRecord {
            name: name.clone(),
            table: tref,
            stream_type: stype,
            created_at: now,
            finalized: false,
            flushed_count: 0,
            staged_rows: Vec::new(),
        },
    );

    Ok(Response::new(WriteStream {
        name,
        r#type: match stype {
            WriteStreamType::Committed => 1,
            WriteStreamType::Pending => 2,
            WriteStreamType::Buffered => 3,
        },
        create_time: Some(Timestamp {
            seconds: now.timestamp(),
            nanos: 0,
        }),
        commit_time: None,
        table_schema: Some(columns_to_table_schema(&cols)),
        location: "US".to_owned(),
    }))
}

fn handle_get_write_stream(
    state: &ServerState,
    name: &str,
) -> Result<Response<WriteStream>, Box<Status>> {
    let rec = state
        .write_streams
        .read()
        .get(name)
        .cloned()
        .ok_or_else(|| Box::new(Status::not_found(format!("WriteStream '{name}' not found"))))?;
    let cols = {
        let eng = state.engine.read();
        let meta = eng
            .catalog()
            .get_table(&rec.table)
            .map_err(|e| bq_err_to_status(&e))?
            .clone();
        drop(eng);
        meta.columns
    };
    Ok(Response::new(WriteStream {
        name: rec.name,
        r#type: match rec.stream_type {
            WriteStreamType::Committed => 1,
            WriteStreamType::Pending => 2,
            WriteStreamType::Buffered => 3,
        },
        create_time: Some(Timestamp {
            seconds: rec.created_at.timestamp(),
            nanos: 0,
        }),
        commit_time: None,
        table_schema: Some(columns_to_table_schema(&cols)),
        location: "US".to_owned(),
    }))
}

fn handle_append_rows(
    state: &ServerState,
    mut inbound: Streaming<AppendRowsRequest>,
) -> Response<ReceiverStream<Result<AppendRowsResponse, Status>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let st = state.clone();
    tokio::spawn(async move {
        let mut active_stream = String::new();
        let mut cached = CachedAppendSchemas::default();
        while let Some(item) = inbound.next().await {
            let req = match item {
                Ok(r) => r,
                Err(status) => {
                    let _ = tx.send(Err(status)).await;
                    break;
                }
            };
            if !req.write_stream.is_empty() {
                active_stream.clone_from(&req.write_stream);
            }
            let res = process_single_append(&st, &active_stream, &mut cached, req).map_err(|e| *e);
            if tx.send(res).await.is_err() {
                break;
            }
        }
    });
    Response::new(ReceiverStream::new(rx))
}

#[derive(Default)]
struct CachedAppendSchemas {
    arrow: Option<Vec<u8>>,
    proto: Option<DescriptorProto>,
}

fn decode_append_rows_payload(
    state: &ServerState,
    tref: &bq_lite_core::catalog::TableRef,
    cached: &mut CachedAppendSchemas,
    req: &AppendRowsRequest,
) -> Result<Vec<Vec<BqValue>>, Box<Status>> {
    if let Some(ref arrow_data) = req.arrow_rows {
        if let Some(ref ws) = arrow_data.writer_schema {
            if !ws.serialized_schema.is_empty() {
                cached.arrow = Some(ws.serialized_schema.clone());
            }
        }
        let schema_bytes = cached.arrow.as_deref().ok_or_else(|| {
            Box::new(Status::invalid_argument(
                "Missing writer_schema in arrow_rows",
            ))
        })?;
        let batch_bytes = arrow_data
            .rows
            .as_ref()
            .map_or(&[] as &[u8], |b| b.serialized_record_batch.as_slice());
        let decoded_qr = arrow_storage_parts_to_query_result(schema_bytes, batch_bytes)
            .map_err(|e| bq_err_to_status(&e))?;
        return Ok(decoded_qr.rows);
    }

    if let Some(ref proto_data) = req.proto_rows {
        if let Some(ref ws) = proto_data.writer_schema {
            if let Some(ref d) = ws.proto_descriptor {
                cached.proto = Some(d.clone());
            }
        }
        let desc = cached.proto.as_ref().ok_or_else(|| {
            Box::new(Status::invalid_argument(
                "Missing writer_schema.proto_descriptor in proto_rows",
            ))
        })?;
        let raw_rows = proto_data
            .rows
            .as_ref()
            .map_or(&[][..], |r| r.serialized_rows.as_slice());
        let cols = {
            let eng = state.engine.read();
            let canonical = eng.catalog().canonical_table_ref(tref);
            let meta = eng
                .catalog()
                .get_table(&canonical)
                .map_err(|e| bq_err_to_status(&e))?
                .clone();
            drop(eng);
            meta.columns
        };
        return decode_proto_rows(desc, raw_rows, &cols).map_err(|e| bq_err_to_status(&e));
    }

    Err(Box::new(Status::invalid_argument(
        "AppendRows requires arrow_rows or proto_rows",
    )))
}

fn process_single_append(
    state: &ServerState,
    stream_name: &str,
    cached: &mut CachedAppendSchemas,
    req: AppendRowsRequest,
) -> Result<AppendRowsResponse, Box<Status>> {
    if let Some(table_path) = stream_name.strip_suffix("/streams/_default") {
        let tref = parse_table_resource(table_path)?;
        let rows = decode_append_rows_payload(state, &tref, cached, &req)?;
        if !rows.is_empty() {
            {
                let mut eng = state.engine.write();
                eng.insert_rows(&tref, rows)
                    .map_err(|e| bq_err_to_status(&e))?;
            }
            state.persist_if_enabled();
        }
        return Ok(AppendRowsResponse {
            append_result: Some(AppendResult {
                offset: Some(Int64Value { value: 0 }),
            }),
            error: None,
            write_stream: stream_name.to_owned(),
        });
    }

    let tref = {
        let streams = state.write_streams.read();
        let rec = streams.get(stream_name).ok_or_else(|| {
            Box::new(Status::not_found(format!(
                "WriteStream '{stream_name}' not found"
            )))
        })?;
        if rec.finalized {
            return Err(Box::new(Status::failed_precondition(
                "Stream is already finalized",
            )));
        }
        let table = rec.table.clone();
        drop(streams);
        table
    };

    let rows = decode_append_rows_payload(state, &tref, cached, &req)?;
    let rows_len = rows.len();
    let (stype, base_offset) = {
        let mut streams = state.write_streams.write();
        let rec = streams.get_mut(stream_name).ok_or_else(|| {
            Box::new(Status::not_found(format!(
                "WriteStream '{stream_name}' not found"
            )))
        })?;
        let cur_len = rec.staged_rows.len();
        if let Some(off) = req.offset {
            let expected = i64::try_from(cur_len).unwrap_or(i64::MAX);
            if off.value != expected {
                return Err(Box::new(Status::out_of_range(format!(
                    "Offset mismatch: expected {expected}, got {}",
                    off.value
                ))));
            }
        }
        rec.staged_rows.extend(rows.clone());
        let out = (rec.stream_type, cur_len);
        drop(streams);
        out
    };

    if stype == WriteStreamType::Committed {
        {
            let mut eng = state.engine.write();
            eng.insert_rows(&tref, rows)
                .map_err(|e| bq_err_to_status(&e))?;
        }
        state.persist_if_enabled();
        if let Some(rec) = state.write_streams.write().get_mut(stream_name) {
            rec.flushed_count += rows_len;
        }
    }

    let off_i64 = i64::try_from(base_offset).unwrap_or(i64::MAX);
    Ok(AppendRowsResponse {
        append_result: Some(AppendResult {
            offset: Some(Int64Value { value: off_i64 }),
        }),
        error: None,
        write_stream: stream_name.to_owned(),
    })
}
