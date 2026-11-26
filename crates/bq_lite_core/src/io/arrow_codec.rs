use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{Field, Schema};

use crate::error::{BqError, BqResult};
use crate::executor::{QueryColumn, QueryResult};
use crate::types::BqValue;

use super::arrow_extract::extract_arrow_cell;
use super::arrow_types::build_arrow_array;
pub use super::arrow_types::{arrow_to_bq_type, bq_type_to_arrow};

/// Converts a [`QueryResult`] into an Apache Arrow [`RecordBatch`].
pub fn query_result_to_record_batch(qr: &QueryResult) -> BqResult<RecordBatch> {
    let fields: Vec<Field> = qr
        .columns
        .iter()
        .map(|c| Field::new(&c.name, bq_type_to_arrow(&c.ty), true))
        .collect();
    let schema = Arc::new(Schema::new(fields));
    let mut arrays = Vec::with_capacity(qr.columns.len());
    for (col_idx, col) in qr.columns.iter().enumerate() {
        let col_vals: Vec<&BqValue> = qr
            .rows
            .iter()
            .map(|r| r.get(col_idx).unwrap_or(&BqValue::Null))
            .collect();
        arrays.push(build_arrow_array(&col.ty, &col_vals)?);
    }
    RecordBatch::try_new(schema, arrays)
        .map_err(|e| BqError::internal(format!("Arrow RecordBatch build failed: {e}")))
}

/// Encodes a [`QueryResult`] into an Arrow IPC stream (`Vec<u8>`).
pub fn query_result_to_arrow_ipc(qr: &QueryResult) -> BqResult<Vec<u8>> {
    let batch = query_result_to_record_batch(qr)?;
    let mut buf = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())
            .map_err(|e| BqError::internal(format!("Arrow IPC writer init failed: {e}")))?;
        writer
            .write(&batch)
            .map_err(|e| BqError::internal(format!("Arrow IPC write failed: {e}")))?;
        writer
            .finish()
            .map_err(|e| BqError::internal(format!("Arrow IPC finish failed: {e}")))?;
    }
    Ok(buf)
}

/// Encodes a [`QueryResult`] into bare Arrow IPC Storage API messages:
/// `(serialized_schema_message, serialized_record_batch_message)`.
pub fn query_result_to_arrow_storage_parts(qr: &QueryResult) -> BqResult<(Vec<u8>, Vec<u8>)> {
    use arrow_ipc::writer::{write_message, DictionaryTracker, IpcDataGenerator, IpcWriteOptions};
    let batch = query_result_to_record_batch(qr)?;
    let opts = IpcWriteOptions::default();
    let gen = IpcDataGenerator::default();
    let mut dict_tracker = DictionaryTracker::new(false);
    let schema_enc =
        gen.schema_to_bytes_with_dictionary_tracker(&batch.schema(), &mut dict_tracker, &opts);
    let mut schema_buf = Vec::new();
    write_message(&mut schema_buf, schema_enc, &opts)
        .map_err(|e| BqError::internal(format!("Arrow schema message write failed: {e}")))?;

    let (_, batch_enc) = gen
        .encoded_batch(&batch, &mut dict_tracker, &opts)
        .map_err(|e| BqError::internal(format!("Arrow batch encode failed: {e}")))?;
    let mut batch_buf = Vec::new();
    write_message(&mut batch_buf, batch_enc, &opts)
        .map_err(|e| BqError::internal(format!("Arrow batch message write failed: {e}")))?;
    Ok((schema_buf, batch_buf))
}

/// Decodes bare Arrow IPC Storage API messages `(schema_msg, batch_msg)` into a [`QueryResult`].
pub fn arrow_storage_parts_to_query_result(
    schema_msg: &[u8],
    batch_msg: &[u8],
) -> BqResult<QueryResult> {
    let mut stream = Vec::with_capacity(schema_msg.len() + batch_msg.len());
    stream.extend_from_slice(schema_msg);
    stream.extend_from_slice(batch_msg);
    arrow_ipc_to_query_result(&stream)
}

/// Decodes an Arrow IPC stream (`&[u8]`) back into a [`QueryResult`].
pub fn arrow_ipc_to_query_result(bytes: &[u8]) -> BqResult<QueryResult> {
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None)
        .map_err(|e| BqError::invalid_argument(format!("Invalid Arrow IPC stream: {e}")))?;
    let schema = reader.schema();
    let mut batches = Vec::new();
    for batch_res in reader {
        let batch = batch_res
            .map_err(|e| BqError::invalid_argument(format!("Arrow batch decode failed: {e}")))?;
        batches.push(batch);
    }
    record_batches_to_query_result(&schema, &batches)
}

/// Converts Arrow [`RecordBatch`]es into a [`QueryResult`].
pub fn record_batches_to_query_result(
    schema: &Schema,
    batches: &[RecordBatch],
) -> BqResult<QueryResult> {
    let cols: Vec<QueryColumn> = schema
        .fields()
        .iter()
        .map(|f| QueryColumn::new(f.name(), arrow_to_bq_type(f.data_type())))
        .collect();
    let mut rows = Vec::new();
    for batch in batches {
        for row_idx in 0..batch.num_rows() {
            let mut row = Vec::with_capacity(cols.len());
            for (col_idx, col) in cols.iter().enumerate() {
                let arr = batch.column(col_idx);
                row.push(extract_arrow_cell(arr.as_ref(), row_idx, &col.ty)?);
            }
            rows.push(row);
        }
    }
    Ok(QueryResult::new(cols, rows))
}
