use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};

#[test]
fn issue_1_timestamp_hour_only_tz_offsets() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               TIMESTAMP '2026-01-02 03:04:05+00' AS t1,
               TIMESTAMP '2026-01-02 03:04:05.123+00' AS t2,
               TIMESTAMP '2026-01-02 03:04:05-05' AS t3,
               TIMESTAMP '2026-01-02 03:04:05+5' AS t4,
               TIMESTAMP '2026-01-02 03:04:05 +00' AS t5,
               TIMESTAMP '2026-01-02 03:04:05 +05:30' AS t6,
               CAST('2026-01-02 03:04:05+00' AS TIMESTAMP) AS t7,
               SAFE_CAST('bad+00' AS TIMESTAMP) AS bad_t",
        )
        .unwrap();

    assert_eq!(qr.columns[0].ty, BqType::Timestamp);
    assert_eq!(qr.rows[0][0].to_string(), "2026-01-02 03:04:05 UTC");
    assert_eq!(qr.rows[0][1].to_string(), "2026-01-02 03:04:05.123 UTC");
    assert_eq!(qr.rows[0][2].to_string(), "2026-01-02 08:04:05 UTC");
    assert_eq!(qr.rows[0][3].to_string(), "2026-01-01 22:04:05 UTC");
    assert_eq!(qr.rows[0][4].to_string(), "2026-01-02 03:04:05 UTC");
    assert_eq!(qr.rows[0][5].to_string(), "2026-01-01 21:34:05 UTC");
    assert_eq!(qr.rows[0][6].to_string(), "2026-01-02 03:04:05 UTC");
    assert_eq!(qr.rows[0][7], BqValue::Null);

    let err = eng.query("SELECT TIMESTAMP 'bad+00'").unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Query);
}

#[test]
fn issue_2_extract_quarter_and_temporal_parts() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               EXTRACT(QUARTER FROM DATE '2026-05-10') AS q_date,
               EXTRACT(QUARTER FROM DATETIME '2026-10-01 12:00:00') AS q_dt,
               EXTRACT(QUARTER FROM TIMESTAMP '2026-01-15 00:00:00 UTC') AS q_ts,
               EXTRACT(QUARTER FROM CAST(NULL AS DATE)) AS q_null,
               EXTRACT(WEEK FROM DATE '2023-01-01') AS w_sun_jan1,
               EXTRACT(WEEK FROM DATE '2023-01-08') AS w_sun_jan8,
               EXTRACT(WEEK FROM DATE '2023-12-31') AS w_sun_dec31,
               EXTRACT(WEEK FROM DATE '2026-01-01') AS w_thu_jan1,
               EXTRACT(WEEK FROM DATE '2026-01-04') AS w_thu_jan4,
               EXTRACT(ISOWEEK FROM DATE '2026-01-01') AS isow,
               EXTRACT(DATE FROM TIMESTAMP '2026-05-10 14:30:00 UTC') AS d_part,
               EXTRACT(DATETIME FROM TIMESTAMP '2026-05-10 14:30:00 UTC') AS dt_part",
        )
        .unwrap();

    assert_eq!(qr.columns[0].ty, BqType::Int64);
    assert_eq!(qr.columns[3].ty, BqType::Int64);
    assert_eq!(qr.columns[10].ty, BqType::Date);
    assert_eq!(qr.columns[11].ty, BqType::Datetime);
    assert_eq!(qr.rows[0][0], BqValue::Int64(2));
    assert_eq!(qr.rows[0][1], BqValue::Int64(4));
    assert_eq!(qr.rows[0][2], BqValue::Int64(1));
    assert_eq!(qr.rows[0][3], BqValue::Null);
    assert_eq!(qr.rows[0][4], BqValue::Int64(1));
    assert_eq!(qr.rows[0][5], BqValue::Int64(2));
    assert_eq!(qr.rows[0][6], BqValue::Int64(53));
    assert_eq!(qr.rows[0][7], BqValue::Int64(0));
    assert_eq!(qr.rows[0][8], BqValue::Int64(1));
    assert_eq!(qr.rows[0][9], BqValue::Int64(1));
    assert_eq!(qr.rows[0][10].to_string(), "2026-05-10");
    assert_eq!(qr.rows[0][11].to_string(), "2026-05-10 14:30:00");

    let err = eng
        .query("SELECT EXTRACT(HOUR FROM DATE '2026-05-10')")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Query);

    let err_dt = eng
        .query("SELECT EXTRACT(DATETIME FROM DATETIME '2026-05-10 14:30:00')")
        .unwrap_err();
    assert_eq!(err_dt.kind, BqErrorKind::Query);
}

#[test]
fn issue_3_safe_convert_bytes_to_string() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               SAFE_CONVERT_BYTES_TO_STRING(b'abc') AS ok,
               SAFE_CONVERT_BYTES_TO_STRING(b'\\xff\\xfeZ') AS lossy,
               SAFE_CONVERT_BYTES_TO_STRING(b'') AS empty_b,
               SAFE_CONVERT_BYTES_TO_STRING(CAST(NULL AS BYTES)) AS null_b",
        )
        .unwrap();

    assert_eq!(qr.columns[0].ty, BqType::String);
    assert_eq!(qr.columns[3].ty, BqType::String);
    assert_eq!(qr.rows[0][0], BqValue::String("abc".to_owned()));
    assert_eq!(
        qr.rows[0][1],
        BqValue::String("\u{FFFD}\u{FFFD}Z".to_owned())
    );
    assert_eq!(qr.rows[0][2], BqValue::String(String::new()));
    assert_eq!(qr.rows[0][3], BqValue::Null);

    let err = eng
        .query("SELECT SAFE_CONVERT_BYTES_TO_STRING(123)")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Query);
}

#[test]
fn issue_4_table_function_with_table_parameter() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA ds;
         CREATE TABLE ds.src (a INT64, label STRING);
         INSERT INTO ds.src VALUES (1, 'x'), (5, 'y'), (10, 'z');
         CREATE TABLE ds.empty_src (a INT64, label STRING);
         CREATE TABLE FUNCTION ds.tf(input TABLE<a INT64>) AS (
           SELECT a, a * 2 AS doubled FROM input WHERE a >= 5
         );
         CREATE TABLE FUNCTION ds.tf_named(input TABLE<a FLOAT64>, min_a INT64) AS (
           SELECT a FROM input WHERE a >= min_a
         );
         CREATE TABLE FUNCTION ds.tf_any(input ANY TABLE) AS (
           SELECT COUNT(*) AS n FROM input
         );",
    )
    .unwrap();

    let qr = eng
        .query("SELECT a, doubled FROM ds.tf(TABLE ds.src) ORDER BY a")
        .unwrap();
    assert_eq!(qr.rows.len(), 2);
    assert_eq!(qr.rows[0], vec![BqValue::Int64(5), BqValue::Int64(10)]);
    assert_eq!(qr.rows[1], vec![BqValue::Int64(10), BqValue::Int64(20)]);

    // Named args in reversed order + implicit INT64 -> FLOAT64 coercion on TABLE column
    let named_qr = eng
        .query("SELECT a FROM ds.tf_named(min_a => 5, input => TABLE ds.src) ORDER BY a")
        .unwrap();
    assert_eq!(named_qr.columns[0].ty, BqType::Float64);
    assert_eq!(
        named_qr.rows,
        vec![vec![BqValue::Float64(5.0)], vec![BqValue::Float64(10.0)]]
    );

    // ANY TABLE parameter
    let any_qr = eng.query("SELECT n FROM ds.tf_any(TABLE ds.src)").unwrap();
    assert_eq!(any_qr.rows[0][0], BqValue::Int64(3));

    // Subquery and 0-row schema preservation
    let sub_qr = eng
        .query("SELECT a, doubled FROM ds.tf((SELECT a FROM ds.src WHERE a = 1))")
        .unwrap();
    assert_eq!(sub_qr.rows.len(), 0);
    assert_eq!(sub_qr.columns[0].ty, BqType::Int64);

    let empty_qr = eng
        .query("SELECT a, doubled FROM ds.tf(TABLE ds.empty_src)")
        .unwrap();
    assert_eq!(empty_qr.rows.len(), 0);
    assert_eq!(empty_qr.columns.len(), 2);
    assert_eq!(empty_qr.columns[0].ty, BqType::Int64);

    // Validation errors: missing column, incompatible column type, arity, scalar UDF TABLE param
    eng.exec_script(
        "CREATE TABLE ds.bad_col (b INT64);
         CREATE TABLE ds.bad_type (a STRING);",
    )
    .unwrap();
    assert_eq!(
        eng.query("SELECT * FROM ds.tf(TABLE ds.bad_col)")
            .unwrap_err()
            .kind,
        BqErrorKind::Query
    );
    assert_eq!(
        eng.query("SELECT * FROM ds.tf(TABLE ds.bad_type)")
            .unwrap_err()
            .kind,
        BqErrorKind::Query
    );
    assert_eq!(
        eng.query("SELECT * FROM ds.tf()").unwrap_err().kind,
        BqErrorKind::Query
    );
    assert_eq!(
        eng.exec("CREATE FUNCTION ds.bad_scalar(t TABLE<a INT64>) RETURNS INT64 AS (1);")
            .unwrap_err()
            .kind,
        BqErrorKind::Query
    );
}

#[test]
fn issue_5_dataset_dunder_tables_meta_table() {
    let mut eng = Engine::new();
    let csv_path = std::env::temp_dir().join("bq_lite_issue_5_ext.csv");
    std::fs::write(&csv_path, "1\n").unwrap();
    let create_ext = format!(
        "CREATE EXTERNAL TABLE ds.ext1 (a INT64) OPTIONS (format = 'CSV', uris = ['{}']);",
        csv_path.display()
    );
    eng.exec_script(&format!(
        "CREATE SCHEMA ds;
         CREATE SCHEMA empty_ds;
         CREATE TABLE ds.t1 (a INT64);
         INSERT INTO ds.t1 VALUES (1), (2), (3);
         CREATE VIEW ds.v1 AS SELECT a FROM ds.t1;
         {create_ext}"
    ))
    .unwrap();
    let _ = std::fs::remove_file(&csv_path);

    let qr = eng
        .query(
            "SELECT project_id, dataset_id, table_id, row_count, type
             FROM ds.__TABLES__
             ORDER BY table_id",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 3);
    assert_eq!(
        qr.rows[0],
        vec![
            BqValue::String("local".to_owned()),
            BqValue::String("ds".to_owned()),
            BqValue::String("ext1".to_owned()),
            BqValue::Int64(0),
            BqValue::Int64(3),
        ]
    );
    assert_eq!(
        qr.rows[1],
        vec![
            BqValue::String("local".to_owned()),
            BqValue::String("ds".to_owned()),
            BqValue::String("t1".to_owned()),
            BqValue::Int64(3),
            BqValue::Int64(1),
        ]
    );
    assert_eq!(
        qr.rows[2],
        vec![
            BqValue::String("local".to_owned()),
            BqValue::String("ds".to_owned()),
            BqValue::String("v1".to_owned()),
            BqValue::Int64(0),
            BqValue::Int64(2),
        ]
    );

    // __TABLES_SUMMARY__ exposes exact 5 columns
    let summary = eng
        .query("SELECT * FROM ds.__TABLES_SUMMARY__ ORDER BY table_id")
        .unwrap();
    assert_eq!(summary.columns.len(), 5);
    assert_eq!(summary.columns[4].name, "type");

    // Empty dataset preserves typed schema for both __TABLES__ and __TABLES_SUMMARY__
    let empty_meta = eng
        .query(
            "SELECT table_id, row_count FROM empty_ds.__TABLES__
             UNION ALL
             SELECT 'sentinel', 0",
        )
        .unwrap();
    assert_eq!(empty_meta.columns[0].ty, BqType::String);
    assert_eq!(empty_meta.columns[1].ty, BqType::Int64);
    assert_eq!(empty_meta.rows.len(), 1);

    let err = eng
        .query("SELECT * FROM missing_ds.__TABLES__")
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::NotFound);
}

#[test]
fn issue_6_having_references_select_aliases() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT MAX(x) AS last_value
             FROM UNNEST([1, 2, 3]) AS x
             HAVING last_value > 1 AND last_value BETWEEN 2 AND 4 AND last_value IN (3, 5)",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][0], BqValue::Int64(3));

    let grouped = eng
        .query(
            "SELECT k AS grp, COUNT(*) AS cnt, MAX(TIMESTAMP '2026-05-01 00:00:00 UTC') AS max_ts
             FROM UNNEST(['alpha', 'alpha', 'beta']) AS k
             GROUP BY k
             HAVING cnt > 1 AND grp LIKE 'al%' AND EXTRACT(YEAR FROM max_ts) = 2026",
        )
        .unwrap();
    assert_eq!(grouped.rows.len(), 1);
    assert_eq!(grouped.rows[0][0], BqValue::String("alpha".to_owned()));
    assert_eq!(grouped.rows[0][1], BqValue::Int64(2));

    let filtered_out = eng
        .query(
            "SELECT MAX(x) AS last_value
             FROM UNNEST([1, 2, 3]) AS x
             HAVING last_value > 10",
        )
        .unwrap();
    assert_eq!(filtered_out.rows.len(), 0);
}

#[test]
fn issue_7_approx_quantiles_and_empty_input_type() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               APPROX_QUANTILES(x, 2) AS q,
               APPROX_COUNT_DISTINCT(x) AS cd
             FROM UNNEST([1.0, 2.0, 2.0, 3.0]) AS x",
        )
        .unwrap();
    assert_eq!(qr.columns[0].ty, BqType::array(BqType::Float64).unwrap());
    assert_eq!(qr.columns[1].ty, BqType::Int64);
    assert_eq!(
        qr.rows[0][0],
        BqValue::Array(vec![
            BqValue::Float64(1.0),
            BqValue::Float64(2.0),
            BqValue::Float64(3.0),
        ])
    );
    assert_eq!(qr.rows[0][1], BqValue::Int64(3));

    let empty = eng
        .query(
            "SELECT APPROX_QUANTILES(x, 2)[OFFSET(1)] AS med
             FROM UNNEST(ARRAY<FLOAT64>[]) AS x
             UNION ALL
             SELECT 1.5 AS med",
        )
        .unwrap();
    assert_eq!(empty.columns[0].ty, BqType::Float64);
    assert_eq!(empty.rows.len(), 2);
    assert_eq!(empty.rows[0][0], BqValue::Null);
    assert_eq!(empty.rows[1][0], BqValue::Float64(1.5));

    // NaN values sort stably without panic
    let with_nan = eng
        .query(
            "SELECT APPROX_QUANTILES(x, 2) AS q
             FROM UNNEST([CAST('NaN' AS FLOAT64), 1.0, 5.0]) AS x",
        )
        .unwrap();
    let BqValue::Array(ref arr) = with_nan.rows[0][0] else {
        panic!("expected array");
    };
    assert_eq!(arr.len(), 3);
    assert!(matches!(arr[0], BqValue::Float64(f) if f.is_nan()));
    assert_eq!(arr[2], BqValue::Float64(5.0));

    // Invalid / NULL / excessive k return Semantic errors and are SAFE.-suppressible
    for bad in [
        "SELECT APPROX_QUANTILES(x, 0) FROM UNNEST([1, 2]) AS x",
        "SELECT APPROX_QUANTILES(x, CAST(NULL AS INT64)) FROM UNNEST([1, 2]) AS x",
        "SELECT APPROX_QUANTILES(x, 1000000000000) FROM UNNEST([1, 2]) AS x",
    ] {
        let err = eng.query(bad).unwrap_err();
        assert_eq!(err.kind, BqErrorKind::Semantic, "failed for: {bad}");
    }

    let safe = eng
        .query(
            "SELECT
               SAFE.APPROX_QUANTILES(x, 0) AS q0,
               SAFE.APPROX_QUANTILES(x, CAST(NULL AS INT64)) AS qnull,
               SAFE.APPROX_QUANTILES(x, 1000000000000) AS qhuge
             FROM UNNEST([1, 2]) AS x",
        )
        .unwrap();
    assert_eq!(
        safe.rows[0],
        vec![BqValue::Null, BqValue::Null, BqValue::Null]
    );
}

#[test]
fn issue_8_google_sheets_external_table_clear_error() {
    let mut eng = Engine::new();
    eng.exec("CREATE SCHEMA ds;").unwrap();
    let err = eng
        .exec(
            "CREATE EXTERNAL TABLE ds.sheet (a STRING)
             OPTIONS (format = 'GOOGLE_SHEETS', uris = ['https://docs.google.com/spreadsheets/d/EXAMPLE']);",
        )
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Unimplemented);
    assert!(
        err.message.contains("GOOGLE_SHEETS"),
        "unexpected message: {}",
        err.message
    );

    let err_http = eng
        .exec(
            "CREATE EXTERNAL TABLE ds.remote_csv (a STRING)
             OPTIONS (format = 'CSV', uris = ['HTTPS://example.com/data.csv']);",
        )
        .unwrap_err();
    assert_eq!(err_http.kind, BqErrorKind::Unimplemented);
    assert!(
        err_http.message.contains("HTTP/HTTPS"),
        "unexpected message: {}",
        err_http.message
    );
}
