use std::sync::Arc;

use arrow_array::{
    ArrayRef, Date64Array, Decimal128Array, Decimal256Array, Float32Array, Int16Array, Int32Array,
    Int8Array, LargeBinaryArray, LargeListArray, LargeStringArray, RecordBatch,
    Time32MillisecondArray, Time32SecondArray, Time64NanosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array,
    UInt8Array,
};
use arrow_buffer::{i256, OffsetBuffer};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bq_lite_core::io::arrow_codec::record_batches_to_query_result;
use bq_lite_core::{BqType, BqValue, Engine};

#[test]
fn milestone_k_zero_row_type_inference_and_grouped_eval() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kds;
         CREATE TABLE kds.empty_t (
             id INT64, f FLOAT64, b BOOL, s STRING, d DATE, ts TIMESTAMP,
             arr ARRAY<INT64>, rec STRUCT<x INT64, y STRING>
         );
         CREATE TABLE kds.grp_t (k INT64, v INT64, rec STRUCT<tag STRING>);
         INSERT INTO kds.grp_t VALUES (1, 10, STRUCT('a')), (1, 20, STRUCT('b')), (2, 0, STRUCT('c'));",
    )
    .unwrap();

    // Exercise 0-row static schema inference across all Expr variants & set ops & pipe
    let zero_qr = eng
        .query(
            "WITH cte AS (SELECT id, rec FROM kds.empty_t) \
             SELECT \
               id + 1 AS plus1, id / 2 AS divf, NOT b AS nb, \
               CASE WHEN b THEN 1 ELSE 2.5 END AS cval, \
               CAST(id AS STRING) AS sid, \
               [1, 2] AS alit, STRUCT(1 AS a, 'z' AS b) AS slit, \
               rec.x AS rx, arr[OFFSET(0)] AS a0, \
               EXTRACT(YEAR FROM d) AS yr, INTERVAL 1 DAY AS iv, \
               RANGE<DATE> '[2024-01-01, 2024-02-01)' AS rng, \
               IF(b, 1, 2) AS ifv, COALESCE(id, 0) AS cv, ARRAY_AGG(id) AS agg_a, \
               (SELECT COUNT(*) FROM kds.empty_t) AS subq, \
               ARRAY(SELECT id FROM kds.empty_t) AS asubq \
             FROM cte \
             GROUP BY id, f, b, s, d, ts, arr, rec \
             UNION ALL \
             SELECT 1, 1.0, TRUE, 1.0, '1', [1], STRUCT(1, 'z'), 1, 1, 2024, INTERVAL 1 DAY, \
                    RANGE<DATE> '[2024-01-01, 2024-02-01)', 1, 1, [1], 1, [1] \
             WHERE FALSE",
        )
        .unwrap();
    assert_eq!(zero_qr.rows.len(), 0);
    assert_eq!(zero_qr.columns.len(), 17);
    assert_eq!(zero_qr.columns[0].ty, BqType::Int64);
    assert_eq!(zero_qr.columns[1].ty, BqType::Float64);
    assert_eq!(zero_qr.columns[2].ty, BqType::Bool);

    let pipe_empty = eng
        .query("FROM kds.empty_t |> SELECT id |> EXTEND id + 1 AS next_id |> SET id = 0")
        .unwrap();
    assert_eq!(pipe_empty.columns.len(), 2);
    assert_eq!(pipe_empty.rows.len(), 0);

    // Exercise agg_eval branches (grouped AND/OR short-circuit, CASE, CAST, Unary, FieldAccess, GROUPING)
    let grp_qr = eng
        .query(
            "SELECT k, GROUPING(k) AS gk, \
               CASE k WHEN 1 THEN 'one' ELSE 'other' END AS lbl, \
               -k AS negk, CAST(k AS STRING) AS ks, \
               (k = 1 AND TRUE) AS and_ok, (k = 99 OR FALSE) AS or_ok, \
               ANY_VALUE(rec).tag AS rtag \
             FROM kds.grp_t GROUP BY ROLLUP(k) HAVING k IS NOT NULL ORDER BY k",
        )
        .unwrap();
    assert_eq!(grp_qr.rows.len(), 2);
    assert_eq!(grp_qr.rows[0][2], BqValue::String("one".to_owned()));
}

#[test]
fn milestone_k_widened_arrow_and_parquet_extractors() {
    let schema = Schema::new(vec![
        Field::new("i8", DataType::Int8, true),
        Field::new("i16", DataType::Int16, true),
        Field::new("i32", DataType::Int32, true),
        Field::new("u8", DataType::UInt8, true),
        Field::new("u16", DataType::UInt16, true),
        Field::new("u32", DataType::UInt32, true),
        Field::new("u64", DataType::UInt64, true),
        Field::new("f32", DataType::Float32, true),
        Field::new("lu", DataType::LargeUtf8, true),
        Field::new("lb", DataType::LargeBinary, true),
        Field::new("d64", DataType::Date64, true),
        Field::new("t32s", DataType::Time32(TimeUnit::Second), true),
        Field::new("t32ms", DataType::Time32(TimeUnit::Millisecond), true),
        Field::new("t64ns", DataType::Time64(TimeUnit::Nanosecond), true),
        Field::new(
            "tss",
            DataType::Timestamp(TimeUnit::Second, Some("UTC".into())),
            true,
        ),
        Field::new(
            "tsms",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new(
            "tsns",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        Field::new("dec128", DataType::Decimal128(10, 2), true),
        Field::new("dec256", DataType::Decimal256(50, 10), true),
        Field::new(
            "llist",
            DataType::LargeList(Arc::new(Field::new("item", DataType::Int32, true))),
            true,
        ),
    ]);
    let batch = build_widened_record_batch(&schema).unwrap();
    let qr = record_batches_to_query_result(&schema, &[batch]).unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][0], BqValue::Int64(1));
    assert_eq!(qr.rows[0][6], BqValue::Int64(7));
    assert_eq!(qr.rows[0][8], BqValue::String("wide".to_owned()));
    assert_eq!(qr.rows[0][17], BqValue::Numeric("12.34".parse().unwrap()));
    assert!(matches!(qr.rows[0][18], BqValue::BigNumeric(_)));
}

fn build_widened_record_batch(schema: &Schema) -> Result<RecordBatch, Box<dyn std::error::Error>> {
    let dec128 = Decimal128Array::from(vec![1234_i128]).with_precision_and_scale(10, 2)?;
    let dec256 = Decimal256Array::from(vec![i256::from_i128(12_345_678_901_i128)])
        .with_precision_and_scale(50, 10)?;
    let llist = LargeListArray::new(
        Arc::new(Field::new("item", DataType::Int32, true)),
        OffsetBuffer::new(vec![0_i64, 2_i64].into()),
        Arc::new(Int32Array::from(vec![9, 10])) as ArrayRef,
        None,
    );
    Ok(RecordBatch::try_new(
        Arc::new(schema.clone()),
        vec![
            Arc::new(Int8Array::from(vec![1])),
            Arc::new(Int16Array::from(vec![2])),
            Arc::new(Int32Array::from(vec![3])),
            Arc::new(UInt8Array::from(vec![4])),
            Arc::new(UInt16Array::from(vec![5])),
            Arc::new(UInt32Array::from(vec![6])),
            Arc::new(UInt64Array::from(vec![7])),
            Arc::new(Float32Array::from(vec![1.5_f32])),
            Arc::new(LargeStringArray::from(vec!["wide"])),
            Arc::new(LargeBinaryArray::from(vec![b"bin".as_slice()])),
            Arc::new(Date64Array::from(vec![86_400_000_i64])),
            Arc::new(Time32SecondArray::from(vec![3600])),
            Arc::new(Time32MillisecondArray::from(vec![3_600_500])),
            Arc::new(Time64NanosecondArray::from(vec![3_600_000_000_000_i64])),
            Arc::new(TimestampSecondArray::from(vec![1_700_000_000]).with_timezone("UTC")),
            Arc::new(TimestampMillisecondArray::from(vec![1_700_000_000_000]).with_timezone("UTC")),
            Arc::new(
                TimestampNanosecondArray::from(vec![1_700_000_000_000_000_000])
                    .with_timezone("UTC"),
            ),
            Arc::new(dec128),
            Arc::new(dec256),
            Arc::new(llist),
        ],
    )?)
}

#[test]
fn milestone_k_external_load_ndjson_parquet_and_alter_options() {
    let dir = tempfile::tempdir().unwrap();
    let nd_path = dir.path().join("items.ndjson");
    std::fs::write(
        &nd_path,
        "{\"id\":1,\"tag\":\"a\"}\n{\"id\":2,\"tag\":\"b\"}\n",
    )
    .unwrap();
    let pq_path = dir.path().join("items.parquet");

    let mut eng = Engine::new();
    let seed = format!(
        "CREATE SCHEMA kext;
         CREATE EXTERNAL TABLE kext.ext_nd OPTIONS(format='NEWLINE_DELIMITED_JSON', uris=['{}']);
         EXPORT DATA OPTIONS(uri='{}', format='PARQUET') AS SELECT * FROM kext.ext_nd;
         CREATE TABLE kext.from_pq (id INT64, tag STRING);
         LOAD DATA INTO kext.from_pq FROM FILES(format='PARQUET', uris=['{}']);
         ALTER TABLE kext.from_pq ALTER COLUMN tag SET OPTIONS(policy_tag='pii', description='user tag');
         CREATE VIEW kext.v AS SELECT id FROM kext.from_pq;
         ALTER VIEW kext.v SET OPTIONS(description='vdesc');
         DROP EXTERNAL TABLE kext.ext_nd;",
        nd_path.display(),
        pq_path.display(),
        pq_path.display()
    );
    eng.exec_script(&seed).unwrap();

    let qr = eng.query("SELECT COUNT(*) FROM kext.from_pq").unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Int64(2));
}

#[cfg(feature = "js-udf")]
#[test]
fn milestone_k_js_udf_full_expressions_and_math_json() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kjs;
         CREATE FUNCTION kjs.complex(a INT64, s STRING) RETURNS STRING LANGUAGE js AS r\"\"\"
           let n = Math.abs(-a) + Math.floor(2.9) + Math.ceil(1.1) + Math.round(1.6);
           let p = Math.pow(2, 3);
           if (n > 10 && s !== 'skip') {
             return s.toLowerCase().split(',')[0] + ':' + (n + p);
           } else if (false || s === 'skip') {
             return 'skipped';
           } else {
             let j = JSON.parse('{\"k\":1}');
             return JSON.stringify(j) + ':' + [1, 2].length;
           }
         \"\"\";",
    )
    .unwrap();
    let qr = eng
        .query("SELECT kjs.complex(10, 'HI,THERE'), kjs.complex(1, 'skip'), kjs.complex(1, 'x')")
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::String("hi:24".to_owned()));
    assert_eq!(qr.rows[0][1], BqValue::String("skipped".to_owned()));
    assert!(qr.rows[0][2].to_string().ends_with(":2"));
}

#[cfg(feature = "js-udf")]
#[test]
fn milestone_k_boa_js_udf_full_es_features_and_sandboxing() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kboa;
         CREATE FUNCTION kboa.sum_even(nums ARRAY<INT64>) RETURNS INT64 LANGUAGE js AS r\"\"\"
           let total = 0;
           for (const n of nums.filter(x => x % 2 === 0)) {
             total += n;
           }
           return total;
         \"\"\";
         CREATE FUNCTION kboa.parse_struct(obj STRUCT<name STRING, score INT64>)
           RETURNS STRUCT<upper_name STRING, passed BOOL> LANGUAGE js AS r\"\"\"
           return { upper_name: obj.name.trim().toUpperCase(), passed: obj.score >= 50 };
         \"\"\";
         CREATE FUNCTION kboa.regex_date(txt STRING, ts TIMESTAMP) RETURNS STRING LANGUAGE js AS r\"\"\"
           const m = txt.match(/id=(\\d+)/);
           return (m ? m[1] : 'none') + '@' + ts.getUTCFullYear();
         \"\"\";
         CREATE FUNCTION kboa.spin() RETURNS INT64 LANGUAGE js AS r\"\"\"
           while (true) {}
         \"\"\";",
    )
    .unwrap();

    let qr = eng
        .query(
            "SELECT
               kboa.sum_even([1, 2, 3, 4, 6]),
               kboa.parse_struct(STRUCT(' alice ' AS name, 80 AS score)),
               kboa.regex_date('user id=42 ok', TIMESTAMP '2026-05-01 00:00:00 UTC')",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Int64(12));
    assert_eq!(
        qr.rows[0][1].to_string(),
        "{upper_name: ALICE, passed: true}"
    );
    assert_eq!(qr.rows[0][2], BqValue::String("42@2026".to_owned()));

    let err = eng.query("SELECT kboa.spin()").unwrap_err();
    assert!(err.to_string().contains("JavaScript UDF runtime error"));
}
