use bq_lite_core::io::arrow_codec::{
    arrow_storage_parts_to_query_result, query_result_to_arrow_storage_parts,
};
use bq_lite_core::io::parquet_codec::{export_parquet, load_parquet};
use bq_lite_core::{BqType, BqValue, Engine, QueryColumn, QueryResult};

#[test]
fn parquet_export_and_load_roundtrips_all_supported_types() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT 42 AS id, 'ada' AS name, TRUE AS ok, 3.5 AS score, \
             DATE '2024-05-19' AS dt, TIMESTAMP '2024-05-19 10:20:30 UTC' AS ts, \
             b'hi' AS raw, ['a', 'b'] AS tags \
             UNION ALL \
             SELECT 7, NULL, FALSE, -1.25, DATE '2020-01-01', TIMESTAMP '2020-01-01 00:00:00 UTC', b'', []",
        )
        .unwrap();

    let pq_bytes = export_parquet(&qr).unwrap();
    let (cols, rows) = load_parquet(&pq_bytes).unwrap();
    assert_eq!(cols.len(), 8);
    assert_eq!(cols[0].ty, BqType::Int64);
    assert_eq!(cols[7].ty, BqType::array(BqType::String).unwrap());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], BqValue::Int64(42));
    assert_eq!(rows[1][1], BqValue::Null);
}

#[test]
fn bare_arrow_storage_parts_roundtrip_without_prepended_stream_header() {
    let qr = QueryResult::new(
        vec![
            QueryColumn::new("id", BqType::Int64),
            QueryColumn::new("tags", BqType::array(BqType::String).unwrap()),
        ],
        vec![vec![
            BqValue::Int64(99),
            BqValue::Array(vec![BqValue::String("x".to_owned())]),
        ]],
    );
    let (schema_msg, batch_msg) = query_result_to_arrow_storage_parts(&qr).unwrap();
    assert_ne!(schema_msg.len(), 0);
    assert_ne!(batch_msg.len(), 0);
    let decoded = arrow_storage_parts_to_query_result(&schema_msg, &batch_msg).unwrap();
    assert_eq!(decoded.rows, qr.rows);
}

#[test]
fn parquet_and_arrow_lossless_nested_arrays_structs_decimals_and_temporals() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
               [1.5, 2.5] AS floats, \
               [TRUE, FALSE] AS bools, \
               [DATE '2024-01-01', DATE '2024-01-02'] AS dates, \
               [TIMESTAMP '2024-01-01 00:00:00 UTC'] AS tss, \
               STRUCT(7 AS id, 'ok' AS tag) AS rec, \
               [STRUCT(1 AS a), STRUCT(2 AS a)] AS rec_arr, \
               NUMERIC '123.456789' AS num, \
               BIGNUMERIC '99.123456789012345678' AS bignum, \
               TIME '12:34:56.789000' AS tm, \
               DATETIME '2024-05-19 10:20:30.123456' AS dt",
        )
        .unwrap();

    let pq = export_parquet(&qr).unwrap();
    let (cols, rows) = load_parquet(&pq).unwrap();
    assert_eq!(cols.len(), 10);
    assert_eq!(cols[0].ty, BqType::array(BqType::Float64).unwrap());
    assert_eq!(cols[1].ty, BqType::array(BqType::Bool).unwrap());
    assert_eq!(cols[2].ty, BqType::array(BqType::Date).unwrap());
    assert_eq!(cols[3].ty, BqType::array(BqType::Timestamp).unwrap());
    assert!(matches!(cols[4].ty, BqType::Struct(_)));
    assert_eq!(cols[6].ty, BqType::Numeric);
    assert_eq!(cols[7].ty, BqType::BigNumeric);
    assert_eq!(cols[8].ty, BqType::Time);
    assert_eq!(cols[9].ty, BqType::Datetime);
    assert_eq!(rows, qr.rows);

    let (schema_msg, batch_msg) = query_result_to_arrow_storage_parts(&qr).unwrap();
    let decoded = arrow_storage_parts_to_query_result(&schema_msg, &batch_msg).unwrap();
    assert_eq!(decoded.rows, qr.rows);
}
