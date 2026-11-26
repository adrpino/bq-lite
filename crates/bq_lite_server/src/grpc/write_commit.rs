use bq_lite_core::BqValue;
use chrono::Utc;
use tonic::{Response, Status};

use crate::state::{ServerState, WriteStreamType};

use super::proto::{
    BatchCommitWriteStreamsRequest, BatchCommitWriteStreamsResponse, FinalizeWriteStreamResponse,
    FlushRowsRequest, FlushRowsResponse, StorageError, Timestamp,
};
use super::read::{bq_err_to_status, parse_table_resource};

pub(super) fn handle_finalize(
    state: &ServerState,
    name: &str,
) -> Result<Response<FinalizeWriteStreamResponse>, Box<Status>> {
    let count = {
        let mut streams = state.write_streams.write();
        let rec = streams.get_mut(name).ok_or_else(|| {
            Box::new(Status::not_found(format!("WriteStream '{name}' not found")))
        })?;
        rec.finalized = true;
        let len = i64::try_from(rec.staged_rows.len()).unwrap_or(i64::MAX);
        drop(streams);
        len
    };
    Ok(Response::new(FinalizeWriteStreamResponse {
        row_count: count,
    }))
}

pub(super) fn handle_flush_rows(
    state: &ServerState,
    req: &FlushRowsRequest,
) -> Result<Response<FlushRowsResponse>, Box<Status>> {
    let (tref, to_insert, flushed_end) = {
        let mut streams = state.write_streams.write();
        let rec = streams.get_mut(&req.write_stream).ok_or_else(|| {
            Box::new(Status::not_found(format!(
                "WriteStream '{}' not found",
                req.write_stream
            )))
        })?;
        if rec.stream_type != WriteStreamType::Buffered {
            return Err(Box::new(Status::invalid_argument(
                "FlushRows is only valid on BUFFERED write streams",
            )));
        }
        let max_idx = rec.staged_rows.len();
        let target_end = req.offset.as_ref().map_or(max_idx, |off| {
            let idx = usize::try_from(off.value.max(0)).unwrap_or(0);
            idx.saturating_add(1).min(max_idx)
        });
        if target_end <= rec.flushed_count {
            let last_off = i64::try_from(rec.flushed_count.saturating_sub(1)).unwrap_or(0);
            return Ok(Response::new(FlushRowsResponse { offset: last_off }));
        }
        let rows: Vec<Vec<BqValue>> = rec
            .staged_rows
            .get(rec.flushed_count..target_end)
            .unwrap_or(&[])
            .to_vec();
        rec.flushed_count = target_end;
        let table = rec.table.clone();
        drop(streams);
        (table, rows, target_end)
    };

    if !to_insert.is_empty() {
        {
            let mut eng = state.engine.write();
            eng.insert_rows(&tref, to_insert)
                .map_err(|e| bq_err_to_status(&e))?;
        }
        state.persist_if_enabled();
    }
    let out_off = i64::try_from(flushed_end.saturating_sub(1)).unwrap_or(0);
    Ok(Response::new(FlushRowsResponse { offset: out_off }))
}

pub(super) fn handle_batch_commit(
    state: &ServerState,
    req: &BatchCommitWriteStreamsRequest,
) -> Result<Response<BatchCommitWriteStreamsResponse>, Box<Status>> {
    let tref = parse_table_resource(&req.parent)?;
    let mut commit_rows = Vec::new();
    let mut errors = Vec::new();
    {
        let mut streams = state.write_streams.write();
        for sname in &req.write_streams {
            let Some(rec) = streams.get_mut(sname) else {
                errors.push(StorageError {
                    code: 5,
                    entity: sname.clone(),
                    error_message: format!("WriteStream '{sname}' not found"),
                });
                continue;
            };
            if rec.stream_type != WriteStreamType::Pending || !rec.finalized {
                errors.push(StorageError {
                    code: 9,
                    entity: sname.clone(),
                    error_message: "WriteStream must be PENDING and finalized".to_owned(),
                });
                continue;
            }
            if rec.flushed_count < rec.staged_rows.len() {
                commit_rows.extend(
                    rec.staged_rows
                        .get(rec.flushed_count..)
                        .unwrap_or(&[])
                        .iter()
                        .cloned(),
                );
                rec.flushed_count = rec.staged_rows.len();
            }
        }
        drop(streams);
    }

    if errors.is_empty() && !commit_rows.is_empty() {
        {
            let mut eng = state.engine.write();
            eng.insert_rows(&tref, commit_rows)
                .map_err(|e| bq_err_to_status(&e))?;
        }
        state.persist_if_enabled();
    }
    Ok(Response::new(BatchCommitWriteStreamsResponse {
        commit_time: Some(Timestamp {
            seconds: Utc::now().timestamp(),
            nanos: 0,
        }),
        stream_errors: errors,
    }))
}
