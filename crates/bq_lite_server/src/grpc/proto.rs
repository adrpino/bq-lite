use bq_lite_core::catalog::ColumnSchema;
use bq_lite_core::BqType;
use prost::Message;

pub fn columns_to_table_schema(cols: &[ColumnSchema]) -> TableSchema {
    TableSchema {
        fields: cols
            .iter()
            .map(|c| col_to_proto(&c.name, &c.ty, c.nullable))
            .collect(),
    }
}

fn col_to_proto(name: &str, ty: &BqType, nullable: bool) -> TableFieldSchema {
    let (mode, inner) = match ty {
        BqType::Array(el) => (3, el.as_ref()),
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Bytes
        | BqType::Date
        | BqType::Datetime
        | BqType::Time
        | BqType::Timestamp
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_)
        | BqType::Struct(_) => (if nullable { 1 } else { 2 }, ty),
    };
    let (type_num, subfields) = match inner {
        BqType::Int64 => (2, Vec::new()),
        BqType::Float64 => (3, Vec::new()),
        BqType::Bool => (5, Vec::new()),
        BqType::Timestamp => (6, Vec::new()),
        BqType::Bytes => (7, Vec::new()),
        BqType::Date => (8, Vec::new()),
        BqType::Time => (9, Vec::new()),
        BqType::Datetime => (10, Vec::new()),
        BqType::Geography => (11, Vec::new()),
        BqType::Numeric => (12, Vec::new()),
        BqType::BigNumeric => (13, Vec::new()),
        BqType::Interval => (14, Vec::new()),
        BqType::Json => (15, Vec::new()),
        BqType::Range(_) => (16, Vec::new()),
        BqType::Struct(sf) => (
            4,
            sf.iter()
                .enumerate()
                .map(|(i, f)| {
                    let fname = f.name.clone().unwrap_or_else(|| format!("f{i}_"));
                    col_to_proto(&fname, &f.ty, true)
                })
                .collect(),
        ),
        BqType::String | BqType::UntypedNull | BqType::Array(_) => (1, Vec::new()),
    };
    TableFieldSchema {
        name: name.to_owned(),
        r#type: type_num,
        mode,
        fields: subfields,
    }
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct Timestamp {
    #[prost(int64, tag = "1")]
    pub seconds: i64,
    #[prost(int32, tag = "2")]
    pub nanos: i32,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct Int64Value {
    #[prost(int64, tag = "1")]
    pub value: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ArrowSchema {
    #[prost(bytes = "vec", tag = "1")]
    pub serialized_schema: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct AvroSchema {
    #[prost(string, tag = "1")]
    pub schema: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ArrowRecordBatch {
    #[prost(bytes = "vec", tag = "1")]
    pub serialized_record_batch: Vec<u8>,
    #[prost(int64, tag = "2")]
    pub row_count: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct AvroRows {
    #[prost(bytes = "vec", tag = "1")]
    pub serialized_binary_rows: Vec<u8>,
    #[prost(int64, tag = "2")]
    pub row_count: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ReadStream {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct TableReadOptions {
    #[prost(string, repeated, tag = "1")]
    pub selected_fields: Vec<String>,
    #[prost(string, tag = "2")]
    pub row_restriction: String,
    #[prost(double, optional, tag = "5")]
    pub sample_percentage: Option<f64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ReadSession {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, optional, tag = "2")]
    pub expire_time: Option<Timestamp>,
    #[prost(int32, tag = "3")]
    pub data_format: i32,
    #[prost(message, optional, tag = "4")]
    pub avro_schema: Option<AvroSchema>,
    #[prost(message, optional, tag = "5")]
    pub arrow_schema: Option<ArrowSchema>,
    #[prost(string, tag = "6")]
    pub table: String,
    #[prost(message, optional, tag = "8")]
    pub read_options: Option<TableReadOptions>,
    #[prost(message, repeated, tag = "10")]
    pub streams: Vec<ReadStream>,
    #[prost(int64, tag = "14")]
    pub estimated_row_count: i64,
}

#[derive(Clone, PartialEq, Message)]
pub struct CreateReadSessionRequest {
    #[prost(string, tag = "1")]
    pub parent: String,
    #[prost(message, optional, tag = "2")]
    pub read_session: Option<ReadSession>,
    #[prost(int32, tag = "3")]
    pub max_stream_count: i32,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ReadRowsRequest {
    #[prost(string, tag = "1")]
    pub read_stream: String,
    #[prost(int64, tag = "2")]
    pub offset: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ReadRowsResponse {
    #[prost(message, optional, tag = "3")]
    pub avro_rows: Option<AvroRows>,
    #[prost(message, optional, tag = "4")]
    pub arrow_record_batch: Option<ArrowRecordBatch>,
    #[prost(int64, tag = "6")]
    pub row_count: i64,
    #[prost(message, optional, tag = "7")]
    pub avro_schema: Option<AvroSchema>,
    #[prost(message, optional, tag = "8")]
    pub arrow_schema: Option<ArrowSchema>,
    #[prost(int64, tag = "10")]
    pub total_estimated_row_count: i64,
}

#[derive(Clone, PartialEq, Message)]
pub struct SplitReadStreamRequest {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(double, tag = "2")]
    pub fraction: f64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct SplitReadStreamResponse {
    #[prost(message, optional, tag = "1")]
    pub primary_stream: Option<ReadStream>,
    #[prost(message, optional, tag = "2")]
    pub remainder_stream: Option<ReadStream>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TableFieldSchema {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(int32, tag = "2")]
    pub r#type: i32,
    #[prost(int32, tag = "3")]
    pub mode: i32,
    #[prost(message, repeated, tag = "4")]
    pub fields: Vec<Self>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TableSchema {
    #[prost(message, repeated, tag = "1")]
    pub fields: Vec<TableFieldSchema>,
}

#[derive(Clone, PartialEq, Message)]
pub struct WriteStream {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(int32, tag = "2")]
    pub r#type: i32,
    #[prost(message, optional, tag = "3")]
    pub create_time: Option<Timestamp>,
    #[prost(message, optional, tag = "4")]
    pub commit_time: Option<Timestamp>,
    #[prost(message, optional, tag = "5")]
    pub table_schema: Option<TableSchema>,
    #[prost(string, tag = "8")]
    pub location: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct CreateWriteStreamRequest {
    #[prost(string, tag = "1")]
    pub parent: String,
    #[prost(message, optional, tag = "2")]
    pub write_stream: Option<WriteStream>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct GetWriteStreamRequest {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct FieldDescriptorProto {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(int32, optional, tag = "3")]
    pub number: Option<i32>,
    #[prost(int32, optional, tag = "4")]
    pub label: Option<i32>,
    #[prost(int32, optional, tag = "5")]
    pub r#type: Option<i32>,
    #[prost(string, optional, tag = "6")]
    pub type_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct DescriptorProto {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(message, repeated, tag = "2")]
    pub field: Vec<FieldDescriptorProto>,
    #[prost(message, repeated, tag = "3")]
    pub nested_type: Vec<Self>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ProtoSchema {
    #[prost(message, optional, tag = "1")]
    pub proto_descriptor: Option<DescriptorProto>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ProtoRows {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub serialized_rows: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ProtoData {
    #[prost(message, optional, tag = "1")]
    pub writer_schema: Option<ProtoSchema>,
    #[prost(message, optional, tag = "2")]
    pub rows: Option<ProtoRows>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct ArrowData {
    #[prost(message, optional, tag = "1")]
    pub writer_schema: Option<ArrowSchema>,
    #[prost(message, optional, tag = "2")]
    pub rows: Option<ArrowRecordBatch>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AppendRowsRequest {
    #[prost(string, tag = "1")]
    pub write_stream: String,
    #[prost(message, optional, tag = "2")]
    pub offset: Option<Int64Value>,
    #[prost(message, optional, tag = "4")]
    pub proto_rows: Option<ProtoData>,
    #[prost(message, optional, tag = "5")]
    pub arrow_rows: Option<ArrowData>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct AppendResult {
    #[prost(message, optional, tag = "1")]
    pub offset: Option<Int64Value>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct RpcStatus {
    #[prost(int32, tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct AppendRowsResponse {
    #[prost(message, optional, tag = "1")]
    pub append_result: Option<AppendResult>,
    #[prost(message, optional, tag = "2")]
    pub error: Option<RpcStatus>,
    #[prost(string, tag = "5")]
    pub write_stream: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct FinalizeWriteStreamRequest {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct FinalizeWriteStreamResponse {
    #[prost(int64, tag = "1")]
    pub row_count: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct FlushRowsRequest {
    #[prost(string, tag = "1")]
    pub write_stream: String,
    #[prost(message, optional, tag = "2")]
    pub offset: Option<Int64Value>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct FlushRowsResponse {
    #[prost(int64, tag = "1")]
    pub offset: i64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct BatchCommitWriteStreamsRequest {
    #[prost(string, tag = "1")]
    pub parent: String,
    #[prost(string, repeated, tag = "2")]
    pub write_streams: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct StorageError {
    #[prost(int32, tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub entity: String,
    #[prost(string, tag = "3")]
    pub error_message: String,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct BatchCommitWriteStreamsResponse {
    #[prost(message, optional, tag = "1")]
    pub commit_time: Option<Timestamp>,
    #[prost(message, repeated, tag = "2")]
    pub stream_errors: Vec<StorageError>,
}
