use std::collections::BTreeMap;
use std::task::{Context, Poll};

use bq_lite_core::catalog::TableRef;
use bq_lite_core::io::arrow_codec::query_result_to_arrow_storage_parts;
use bq_lite_core::{BqError, QueryResult};
use chrono::Utc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::body::Body;
use tonic::codegen::{http, BoxFuture, Service};
use tonic::server::{Grpc, NamedService, ServerStreamingService, UnaryService};
use tonic::{Request, Response, Status};

use crate::state::{ReadSessionRecord, ServerState};

use super::avro::{query_result_to_avro_rows, query_result_to_avro_schema_json};
use super::proto::{
    ArrowRecordBatch, ArrowSchema, AvroRows, AvroSchema, CreateReadSessionRequest, ReadRowsRequest,
    ReadRowsResponse, ReadSession, ReadStream, SplitReadStreamRequest, SplitReadStreamResponse,
    TableReadOptions, Timestamp,
};

const DATA_FORMAT_AVRO: i32 = 1;
const DATA_FORMAT_AVRO_ALT: i32 = 3;

#[derive(Debug, Clone)]
pub struct BigQueryReadService {
    state: ServerState,
}

impl BigQueryReadService {
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }
}

impl NamedService for BigQueryReadService {
    const NAME: &'static str = "google.cloud.bigquery.storage.v1.BigQueryRead";
}

impl Service<http::Request<Body>> for BigQueryReadService {
    type Response = http::Response<Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let svc = self.clone();
        match req.uri().path() {
            "/google.cloud.bigquery.storage.v1.BigQueryRead/CreateReadSession" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(CreateReadSessionSvc(svc), req).await)
                })
            }
            "/google.cloud.bigquery.storage.v1.BigQueryRead/ReadRows" => Box::pin(async move {
                let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                Ok(grpc.server_streaming(ReadRowsSvc(svc), req).await)
            }),
            "/google.cloud.bigquery.storage.v1.BigQueryRead/SplitReadStream" => {
                Box::pin(async move {
                    let mut grpc = Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(SplitReadStreamSvc(svc), req).await)
                })
            }
            _ => Box::pin(async move {
                Ok(Status::unimplemented("Unknown BigQueryRead RPC").into_http())
            }),
        }
    }
}

struct CreateReadSessionSvc(BigQueryReadService);
impl UnaryService<CreateReadSessionRequest> for CreateReadSessionSvc {
    type Response = ReadSession;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<CreateReadSessionRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(
            async move { handle_create_read_session(&state, req.into_inner()).map_err(|e| *e) },
        )
    }
}

struct ReadRowsSvc(BigQueryReadService);
impl ServerStreamingService<ReadRowsRequest> for ReadRowsSvc {
    type Response = ReadRowsResponse;
    type ResponseStream = ReceiverStream<Result<ReadRowsResponse, Status>>;
    type Future = BoxFuture<Response<Self::ResponseStream>, Status>;

    fn call(&mut self, req: Request<ReadRowsRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move {
            handle_read_rows(&state, &req.into_inner())
                .await
                .map_err(|e| *e)
        })
    }
}

struct SplitReadStreamSvc(BigQueryReadService);
impl UnaryService<SplitReadStreamRequest> for SplitReadStreamSvc {
    type Response = SplitReadStreamResponse;
    type Future = BoxFuture<Response<Self::Response>, Status>;

    fn call(&mut self, req: Request<SplitReadStreamRequest>) -> Self::Future {
        let state = self.0.state.clone();
        Box::pin(async move { handle_split_read_stream(&state, req.into_inner()).map_err(|e| *e) })
    }
}

pub(super) fn parse_table_resource(resource: &str) -> Result<TableRef, Box<Status>> {
    let parts: Vec<&str> = resource.split('/').collect();
    if let ["projects", proj, "datasets", ds, "tables", tbl] = parts.as_slice() {
        return Ok(TableRef::new(*proj, *ds, *tbl));
    }
    Err(Box::new(Status::invalid_argument(format!(
        "Invalid table resource path: '{resource}'"
    ))))
}

fn handle_create_read_session(
    state: &ServerState,
    req: CreateReadSessionRequest,
) -> Result<Response<ReadSession>, Box<Status>> {
    let rs = req
        .read_session
        .ok_or_else(|| Box::new(Status::invalid_argument("Missing read_session")))?;
    let tref = parse_table_resource(&rs.table)?;
    let opts = rs.read_options.clone().unwrap_or(TableReadOptions {
        selected_fields: Vec::new(),
        row_restriction: String::new(),
        sample_percentage: None,
    });

    let qr = resolve_read_query_result(state, &tref, &opts.selected_fields, &opts.row_restriction)
        .map_err(|e| bq_err_to_status(&e))?;
    let total_rows = qr.rows.len();
    let row_count = i64::try_from(total_rows).unwrap_or(i64::MAX);
    let is_avro = matches!(rs.data_format, DATA_FORMAT_AVRO | DATA_FORMAT_AVRO_ALT);

    let (avro_schema, arrow_schema, out_format) = if is_avro {
        (
            Some(AvroSchema {
                schema: query_result_to_avro_schema_json(&qr),
            }),
            None,
            DATA_FORMAT_AVRO,
        )
    } else {
        let (schema_bytes, _) =
            query_result_to_arrow_storage_parts(&qr).map_err(|e| bq_err_to_status(&e))?;
        (
            None,
            Some(ArrowSchema {
                serialized_schema: schema_bytes,
            }),
            2,
        )
    };

    let session_id = format!("sess_{}", uuid::Uuid::new_v4());
    let parent = if req.parent.is_empty() {
        format!("projects/{}", tref.project)
    } else {
        req.parent
    };
    let session_name = format!("{parent}/locations/US/sessions/{session_id}");
    let desired = usize::try_from(req.max_stream_count.max(1)).unwrap_or(1);
    let stream_count = if total_rows == 0 {
        1
    } else {
        desired.min(total_rows).max(1)
    };
    let (streams, stream_ranges) = partition_streams(&session_name, total_rows, stream_count);

    state.record_read_session(
        session_name.clone(),
        ReadSessionRecord {
            name: session_name.clone(),
            result: std::sync::Arc::new(qr),
            data_format: out_format,
            stream_ranges,
        },
    );

    let expire = Utc::now().timestamp() + 3600;
    Ok(Response::new(ReadSession {
        name: session_name,
        expire_time: Some(Timestamp {
            seconds: expire,
            nanos: 0,
        }),
        data_format: out_format,
        avro_schema,
        arrow_schema,
        table: rs.table,
        read_options: rs.read_options,
        streams,
        estimated_row_count: row_count,
    }))
}

type PartitionedStreams = (Vec<ReadStream>, BTreeMap<String, (usize, usize)>);

fn partition_streams(
    session_name: &str,
    total_rows: usize,
    stream_count: usize,
) -> PartitionedStreams {
    let mut streams = Vec::with_capacity(stream_count);
    let mut ranges = BTreeMap::new();
    let chunk = (total_rows / stream_count).max(1);
    let mut start = 0_usize;
    for idx in 0..stream_count {
        let end = if idx + 1 == stream_count {
            total_rows
        } else {
            (start + chunk).min(total_rows)
        };
        let sname = format!("{session_name}/streams/{idx}");
        streams.push(ReadStream {
            name: sname.clone(),
        });
        ranges.insert(sname, (start, end));
        start = end;
    }
    (streams, ranges)
}

fn handle_split_read_stream(
    state: &ServerState,
    req: SplitReadStreamRequest,
) -> Result<Response<SplitReadStreamResponse>, Box<Status>> {
    let session_name = req
        .name
        .rsplit_once("/streams/")
        .map_or(req.name.as_str(), |(s, _)| s)
        .to_owned();
    let mut sessions = state.read_sessions.write();
    let rec = sessions.get_mut(&session_name).ok_or_else(|| {
        Box::new(Status::not_found(format!(
            "ReadStream '{}' not found",
            req.name
        )))
    })?;
    let (start, end) = rec
        .stream_ranges
        .get(&req.name)
        .copied()
        .unwrap_or((0, rec.result.rows.len()));
    let len = end.saturating_sub(start);
    let frac = req.fraction.clamp(0.0, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "Splitting row slice count by fractional midpoint"
    )]
    let split_delta = ((len as f64) * frac).round() as usize;
    let mid = start + split_delta.min(len);
    if mid <= start || mid >= end {
        return Ok(Response::new(SplitReadStreamResponse {
            primary_stream: Some(ReadStream { name: req.name }),
            remainder_stream: None,
        }));
    }
    let next_idx = rec.stream_ranges.len();
    let primary_name = format!("{}_p{next_idx}", req.name);
    let rem_name = format!("{}_r{next_idx}", req.name);
    rec.stream_ranges.insert(primary_name.clone(), (start, mid));
    rec.stream_ranges.insert(rem_name.clone(), (mid, end));
    drop(sessions);
    Ok(Response::new(SplitReadStreamResponse {
        primary_stream: Some(ReadStream { name: primary_name }),
        remainder_stream: Some(ReadStream { name: rem_name }),
    }))
}

fn resolve_read_query_result(
    state: &ServerState,
    tref: &TableRef,
    selected_fields: &[String],
    row_restriction: &str,
) -> Result<QueryResult, Box<BqError>> {
    if tref.dataset == "_bqlite_jobs" || tref.table.starts_with("_anon_") {
        let jid = tref.table.strip_prefix("_anon_").unwrap_or(&tref.table);
        let base = {
            let jobs = state.jobs.read();
            let rec = jobs.get(jid).ok_or_else(|| {
                BqError::not_found(format!("Anonymous result table '{jid}' not found"))
            })?;
            let qr = rec
                .result
                .clone()
                .unwrap_or_else(|| std::sync::Arc::new(QueryResult::empty()));
            drop(jobs);
            qr
        };
        let eng = state.engine.read();
        return eng.project_and_filter_result(&base, selected_fields, row_restriction);
    }

    let eng = state.engine.read();
    let canonical = eng.catalog().canonical_table_ref(tref);
    let _ = eng.catalog().get_table(&canonical)?;
    let cols_sql = if selected_fields.is_empty() {
        "*".to_owned()
    } else {
        selected_fields
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let where_sql = if row_restriction.trim().is_empty() {
        String::new()
    } else {
        format!(" WHERE {row_restriction}")
    };
    let sql = format!(
        "SELECT {cols_sql} FROM `{}`.`{}`.`{}`{where_sql}",
        canonical.project, canonical.dataset, canonical.table
    );
    eng.query(&sql)
}

async fn handle_read_rows(
    state: &ServerState,
    req: &ReadRowsRequest,
) -> Result<Response<ReceiverStream<Result<ReadRowsResponse, Status>>>, Box<Status>> {
    let session_name = req
        .read_stream
        .rsplit_once("/streams/")
        .map_or(req.read_stream.as_str(), |(s, _)| s);
    let rec = state
        .read_sessions
        .read()
        .get(session_name)
        .cloned()
        .ok_or_else(|| {
            Box::new(Status::not_found(format!(
                "ReadStream '{}' not found",
                req.read_stream
            )))
        })?;

    let (range_start, range_end) = rec
        .stream_ranges
        .get(&req.read_stream)
        .copied()
        .unwrap_or((0, rec.result.rows.len()));
    let rel_off = usize::try_from(req.offset.max(0)).unwrap_or(0);
    let slice_start = range_start.saturating_add(rel_off).min(range_end);
    let total_est = i64::try_from(range_end.saturating_sub(range_start)).unwrap_or(i64::MAX);
    let (tx, rx) = tokio::sync::mpsc::channel(4);

    if slice_start < range_end {
        let sliced_rows = rec
            .result
            .rows
            .get(slice_start..range_end)
            .unwrap_or(&[])
            .to_vec();
        let batch_count = i64::try_from(sliced_rows.len()).unwrap_or(i64::MAX);
        let sliced_qr = QueryResult::new(rec.result.columns.clone(), sliced_rows);
        let resp = if rec.data_format == DATA_FORMAT_AVRO {
            let schema_json = query_result_to_avro_schema_json(&sliced_qr);
            let avro_bytes =
                query_result_to_avro_rows(&sliced_qr).map_err(|e| bq_err_to_status(&e))?;
            ReadRowsResponse {
                avro_rows: Some(AvroRows {
                    serialized_binary_rows: avro_bytes,
                    row_count: batch_count,
                }),
                arrow_record_batch: None,
                row_count: batch_count,
                avro_schema: Some(AvroSchema {
                    schema: schema_json,
                }),
                arrow_schema: None,
                total_estimated_row_count: total_est,
            }
        } else {
            let (schema_bytes, batch_bytes) = query_result_to_arrow_storage_parts(&sliced_qr)
                .map_err(|e| bq_err_to_status(&e))?;
            ReadRowsResponse {
                avro_rows: None,
                arrow_record_batch: Some(ArrowRecordBatch {
                    serialized_record_batch: batch_bytes,
                    row_count: batch_count,
                }),
                row_count: batch_count,
                avro_schema: None,
                arrow_schema: Some(ArrowSchema {
                    serialized_schema: schema_bytes,
                }),
                total_estimated_row_count: total_est,
            }
        };
        let _ = tx.send(Ok(resp)).await;
    }

    Ok(Response::new(ReceiverStream::new(rx)))
}

pub(super) fn bq_err_to_status(err: &BqError) -> Box<Status> {
    let s = match err.kind {
        bq_lite_core::BqErrorKind::NotFound => Status::not_found(err.message.clone()),
        bq_lite_core::BqErrorKind::AlreadyExists => Status::already_exists(err.message.clone()),
        bq_lite_core::BqErrorKind::PermissionDenied => {
            Status::permission_denied(err.message.clone())
        }
        bq_lite_core::BqErrorKind::Unimplemented => Status::unimplemented(err.message.clone()),
        bq_lite_core::BqErrorKind::InvalidArgument
        | bq_lite_core::BqErrorKind::Syntax
        | bq_lite_core::BqErrorKind::Query
        | bq_lite_core::BqErrorKind::Semantic => Status::invalid_argument(err.message.clone()),
        bq_lite_core::BqErrorKind::Internal => Status::internal(err.message.clone()),
    };
    Box::new(s)
}
