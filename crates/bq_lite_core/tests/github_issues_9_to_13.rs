use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};

#[test]
fn issue_9_json_object_two_array_signature() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               TO_JSON_STRING(JSON_OBJECT(['b', 'a'], [1, 2])) AS arrays,
               TO_JSON_STRING(JSON_OBJECT(['a', 'a'], [1, 2])) AS repeated_key,
               TO_JSON_STRING(JSON_OBJECT(CAST([] AS ARRAY<STRING>), CAST([] AS ARRAY<INT64>))) AS empty_arrays,
               TO_JSON_STRING(JSON_OBJECT('items', [1, 2])) AS kv_array_val,
               TO_JSON_STRING(JSON_OBJECT('x', 10, 'x', 20)) AS kv_dup_key,
               JSON_OBJECT(CAST(NULL AS ARRAY<STRING>), [1]) AS null_keys,
               JSON_OBJECT(['a'], CAST(NULL AS ARRAY<INT64>)) AS null_vals",
        )
        .unwrap();

    assert_eq!(
        qr.rows[0][0],
        BqValue::String(r#"{"a":2,"b":1}"#.to_owned())
    );
    assert_eq!(qr.rows[0][1], BqValue::String(r#"{"a":1}"#.to_owned()));
    assert_eq!(qr.rows[0][2], BqValue::String("{}".to_owned()));
    assert_eq!(
        qr.rows[0][3],
        BqValue::String(r#"{"items":[1,2]}"#.to_owned())
    );
    assert_eq!(qr.rows[0][4], BqValue::String(r#"{"x":10}"#.to_owned()));
    assert_eq!(qr.rows[0][5], BqValue::Null);
    assert_eq!(qr.rows[0][6], BqValue::Null);
    assert_eq!(qr.columns[5].ty, BqType::Json);

    // Works with ARRAY_AGG (both non-empty and empty input)
    let agg_qr = eng
        .query(
            "SELECT TO_JSON_STRING(JSON_OBJECT(ARRAY_AGG(k ORDER BY x), ARRAY_AGG(v ORDER BY x))) AS obj
             FROM UNNEST([STRUCT('b' AS k, 1 AS v, 1 AS x), STRUCT('a', 2, 2)])",
        )
        .unwrap();
    assert_eq!(
        agg_qr.rows[0][0],
        BqValue::String(r#"{"a":2,"b":1}"#.to_owned())
    );

    let empty_agg = eng
        .query(
            "SELECT JSON_OBJECT(ARRAY_AGG(k), ARRAY_AGG(v)) AS obj
             FROM UNNEST(ARRAY<STRUCT<k STRING, v INT64>>[])",
        )
        .unwrap();
    assert_eq!(empty_agg.columns[0].ty, BqType::Json);
    assert_eq!(empty_agg.rows[0][0], BqValue::Null);

    // Mismatched lengths & NULL keys are Semantic errors (SAFE.-suppressible)
    let err_len = eng
        .query("SELECT JSON_OBJECT(['a', 'b'], [1])")
        .unwrap_err();
    assert_eq!(err_len.kind, BqErrorKind::Semantic);

    let err_null_key = eng
        .query("SELECT JSON_OBJECT([CAST(NULL AS STRING)], [1])")
        .unwrap_err();
    assert_eq!(err_null_key.kind, BqErrorKind::Semantic);

    let safe = eng
        .query(
            "SELECT
               SAFE.JSON_OBJECT(['a', 'b'], [1]) AS s1,
               SAFE.JSON_OBJECT([CAST(NULL AS STRING)], [1]) AS s2",
        )
        .unwrap();
    assert_eq!(safe.rows[0], vec![BqValue::Null, BqValue::Null]);
}

#[test]
fn issue_10_legacy_json_extract_bracket_quoted_keys() {
    let eng = Engine::new();
    let qr = eng
        .query(
            r#"SELECT
                 JSON_EXTRACT_SCALAR('{"_id":{"$oid":"abc"}}', "$._id['$oid']") AS on_string,
                 JSON_EXTRACT_SCALAR(JSON '{"_id":{"$oid":"abc"}}', "$._id['$oid']") AS on_json,
                 JSON_EXTRACT_SCALAR('{"a b":"x"}', "$['a b']") AS spaced_key,
                 JSON_EXTRACT('{"_id":{"$oid":"abc"}}', "$._id['$oid']") AS extract_raw,
                 JSON_EXTRACT_STRING_ARRAY('{"a.b":["x","y"]}', "$['a.b']") AS arr_str,
                 JSON_VALUE('{"_id":{"$oid":"abc"}}', '$._id."$oid"') AS std_quoted"#,
        )
        .unwrap();

    assert_eq!(qr.rows[0][0], BqValue::String("abc".to_owned()));
    assert_eq!(qr.rows[0][1], BqValue::String("abc".to_owned()));
    assert_eq!(qr.rows[0][2], BqValue::String("x".to_owned()));
    assert_eq!(qr.rows[0][3], BqValue::String(r#""abc""#.to_owned()));
    assert_eq!(
        qr.rows[0][4],
        BqValue::Array(vec![
            BqValue::String("x".to_owned()),
            BqValue::String("y".to_owned()),
        ])
    );
    assert_eq!(qr.rows[0][5], BqValue::String("abc".to_owned()));

    // Standard JSON_VALUE and JSON_QUERY still reject single-quoted bracket keys
    for std_sql in [
        r#"SELECT JSON_VALUE('{"_id":{"$oid":"abc"}}', "$._id['$oid']")"#,
        r#"SELECT JSON_QUERY('{"_id":{"$oid":"abc"}}', "$._id['$oid']")"#,
    ] {
        let err = eng.query(std_sql).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Semantic, "failed for: {std_sql}");
    }

    let safe = eng
        .query(r#"SELECT SAFE.JSON_VALUE('{"_id":{"$oid":"abc"}}', "$._id['$oid']")"#)
        .unwrap();
    assert_eq!(safe.rows[0][0], BqValue::Null);
}

#[test]
fn issue_11_round_and_scalar_null_return_types() {
    let mut eng = Engine::new();

    // Direct NULL FLOAT64 + UNION ALL
    let qr = eng
        .query(
            "SELECT ROUND(CAST(NULL AS FLOAT64), 2) AS v
             UNION ALL
             SELECT 1.5",
        )
        .unwrap();
    assert_eq!(qr.columns[0].ty, BqType::Float64);
    assert_eq!(qr.rows[0][0], BqValue::Null);
    assert_eq!(qr.rows[1][0], BqValue::Float64(1.5));

    // Empty input aggregate + ROUND + UNION ALL
    let empty_agg = eng
        .query(
            "SELECT ROUND(AVG(x), 2) AS v
             FROM UNNEST(CAST([] AS ARRAY<FLOAT64>)) AS x
             UNION ALL
             SELECT 1.5",
        )
        .unwrap();
    assert_eq!(empty_agg.columns[0].ty, BqType::Float64);
    assert_eq!(empty_agg.rows[0][0], BqValue::Null);
    assert_eq!(empty_agg.rows[1][0], BqValue::Float64(1.5));

    // NUMERIC and BIGNUMERIC preserve their exact types (both NULL and non-NULL)
    let num_qr = eng
        .query(
            "SELECT
               ROUND(CAST(NULL AS NUMERIC), 2) AS n_null,
               ROUND(NUMERIC '1.235', 2) AS n_val,
               ROUND(CAST(NULL AS BIGNUMERIC), 2) AS bn_null,
               ROUND(BIGNUMERIC '1.235', 2) AS bn_val,
               TRUNC(NUMERIC '1.239', 2) AS n_trunc,
               ABS(CAST(NULL AS INT64)) AS abs_i,
               CEIL(CAST(NULL AS FLOAT64)) AS ceil_f,
               FLOOR(CAST(NULL AS FLOAT64)) AS floor_f,
               SIGN(CAST(NULL AS FLOAT64)) AS sign_i",
        )
        .unwrap();
    assert_eq!(num_qr.columns[0].ty, BqType::Numeric);
    assert_eq!(num_qr.columns[1].ty, BqType::Numeric);
    assert_eq!(num_qr.rows[0][1].to_string(), "1.24");
    assert_eq!(num_qr.columns[2].ty, BqType::BigNumeric);
    assert_eq!(num_qr.columns[3].ty, BqType::BigNumeric);
    assert_eq!(num_qr.rows[0][3].to_string(), "1.24");
    assert_eq!(num_qr.rows[0][4].to_string(), "1.23");
    assert_eq!(num_qr.columns[5].ty, BqType::Int64);
    assert_eq!(num_qr.columns[6].ty, BqType::Float64);
    assert_eq!(num_qr.columns[7].ty, BqType::Float64);
    assert_eq!(num_qr.columns[8].ty, BqType::Int64);

    // CTAS on empty table writes exact FLOAT64 schema to INFORMATION_SCHEMA.COLUMNS
    eng.exec_script(
        "CREATE SCHEMA ds;
         CREATE TABLE ds.t (x FLOAT64);
         CREATE TABLE ds.a AS SELECT AVG(x) AS avg_x, ROUND(AVG(x), 2) AS round_avg FROM ds.t;",
    )
    .unwrap();
    let cols = eng
        .query(
            "SELECT column_name, data_type
             FROM ds.INFORMATION_SCHEMA.COLUMNS
             WHERE table_name = 'a'
             ORDER BY ordinal_position",
        )
        .unwrap();
    assert_eq!(
        cols.rows,
        vec![
            vec![
                BqValue::String("avg_x".to_owned()),
                BqValue::String("FLOAT64".to_owned()),
            ],
            vec![
                BqValue::String("round_avg".to_owned()),
                BqValue::String("FLOAT64".to_owned()),
            ],
        ]
    );
}

#[test]
fn issue_12_outer_join_using_unmatched_column_not_ambiguous() {
    let eng = Engine::new();

    // Exact issue repro: LEFT JOIN ... USING with UNNEST on left
    let qr = eng
        .query(
            "WITH
               a AS (SELECT k FROM UNNEST(['x', 'y']) AS k),
               b AS (SELECT 'x' AS k)
             SELECT k
             FROM a
             LEFT JOIN b USING (k)
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(
        qr.rows,
        vec![
            vec![BqValue::String("x".to_owned())],
            vec![BqValue::String("y".to_owned())],
        ]
    );

    // GROUP BY and ORDER BY on merged USING column
    let grouped = eng
        .query(
            "WITH
               a AS (SELECT k FROM UNNEST(['x', 'y', 'y']) AS k),
               b AS (SELECT 'x' AS k, 10 AS v)
             SELECT k, COUNT(*) AS cnt, MAX(v) AS mv
             FROM a
             LEFT JOIN b USING (k)
             GROUP BY k
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(
        grouped.rows,
        vec![
            vec![
                BqValue::String("x".to_owned()),
                BqValue::Int64(1),
                BqValue::Int64(10),
            ],
            vec![
                BqValue::String("y".to_owned()),
                BqValue::Int64(2),
                BqValue::Null,
            ],
        ]
    );

    // RIGHT JOIN and FULL OUTER JOIN USING preserve non-null right key without ambiguity
    let full = eng
        .query(
            "WITH
               a AS (SELECT k, 1 AS la FROM UNNEST(['x', 'y']) AS k),
               b AS (SELECT k, 2 AS rb FROM UNNEST(['x', 'z']) AS k)
             SELECT k, la, rb
             FROM a
             FULL OUTER JOIN b USING (k)
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(
        full.rows,
        vec![
            vec![
                BqValue::String("x".to_owned()),
                BqValue::Int64(1),
                BqValue::Int64(2),
            ],
            vec![
                BqValue::String("y".to_owned()),
                BqValue::Int64(1),
                BqValue::Null,
            ],
            vec![
                BqValue::String("z".to_owned()),
                BqValue::Null,
                BqValue::Int64(2),
            ],
        ]
    );
}

#[test]
fn issue_13_google_sheets_external_table_with_schema_creates_empty_table() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA s;
         CREATE EXTERNAL TABLE s.t (a STRING, b INT64)
         OPTIONS (
           format = 'GOOGLE_SHEETS',
           uris = ['https://docs.google.com/spreadsheets/d/EXAMPLE'],
           skip_leading_rows = 1
         );",
    )
    .unwrap();

    // Selecting returns 0 rows with the exact declared schema
    let qr = eng.query("SELECT a, b FROM s.t").unwrap();
    assert_eq!(qr.rows.len(), 0);
    assert_eq!(qr.columns.len(), 2);
    assert_eq!(qr.columns[0].name, "a");
    assert_eq!(qr.columns[0].ty, BqType::String);
    assert_eq!(qr.columns[1].name, "b");
    assert_eq!(qr.columns[1].ty, BqType::Int64);

    // Downstream models / views / UNION ALL succeed
    let downstream = eng
        .query(
            "SELECT a, b FROM s.t
             UNION ALL
             SELECT 'ok', 42",
        )
        .unwrap();
    assert_eq!(downstream.rows.len(), 1);
    assert_eq!(
        downstream.rows[0],
        vec![BqValue::String("ok".to_owned()), BqValue::Int64(42)]
    );

    // Appears in __TABLES__ with type = 3 (EXTERNAL) and 0 rows
    let meta = eng
        .query("SELECT table_id, row_count, type FROM s.__TABLES__")
        .unwrap();
    assert_eq!(
        meta.rows[0],
        vec![
            BqValue::String("t".to_owned()),
            BqValue::Int64(0),
            BqValue::Int64(3),
        ]
    );

    // Without explicit schema, GOOGLE_SHEETS still errors clearly
    let err = eng
        .exec(
            "CREATE EXTERNAL TABLE s.no_schema
             OPTIONS (format = 'GOOGLE_SHEETS', uris = ['https://docs.google.com/spreadsheets/d/EXAMPLE']);",
        )
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Unimplemented);
}
