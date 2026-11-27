use bq_lite_core::{BqErrorKind, BqType, BqValue, Engine};

#[test]
fn issue_14_group_by_resolves_unique_select_column_when_ambiguous_in_from() {
    let eng = Engine::new();

    // Repro 1: a.k selected, ambiguous k in FROM
    let qr1 = eng
        .query(
            "WITH a AS (SELECT 1 AS id, 'x' AS k),
                  b AS (SELECT 'x' AS k, 2 AS v)
             SELECT a.k, COUNT(*) AS n
             FROM a JOIN b ON a.k = b.k
             GROUP BY k",
        )
        .unwrap();
    assert_eq!(
        qr1.rows,
        vec![vec![BqValue::String("x".to_owned()), BqValue::Int64(1)]]
    );

    // Repro 2: derived table + LEFT JOIN with same column name
    let qr2 = eng
        .query(
            "WITH docs AS (SELECT 'ok' AS status, 1 AS id)
             SELECT st.status, COUNT(d.id) AS n
             FROM (SELECT DISTINCT status FROM docs) st
             LEFT JOIN docs d ON d.status = st.status
             GROUP BY status",
        )
        .unwrap();
    assert_eq!(
        qr2.rows,
        vec![vec![BqValue::String("ok".to_owned()), BqValue::Int64(1)]]
    );

    // Ambiguous in BOTH FROM and SELECT still errors
    let err = eng
        .query(
            "WITH a AS (SELECT 'x' AS k), b AS (SELECT 'x' AS k)
             SELECT a.k, b.k FROM a JOIN b ON a.k = b.k GROUP BY k",
        )
        .unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Query);
}

#[test]
fn issue_15_date_part_in_grouped_aggregate_calls_and_tuple_unnest() {
    let eng = Engine::new();

    // Exact issue #15 repro
    let qr = eng
        .query(
            "WITH t AS (
               SELECT CAST(ts AS TIMESTAMP) AS at, g
               FROM UNNEST([
                 STRUCT('a' AS g, '2026-01-01 00:00:00' AS ts),
                 STRUCT('a' AS g, '2026-01-01 00:00:30' AS ts)
               ])
             )
             SELECT g, TIMESTAMP_DIFF(MAX(at), MIN(at), SECOND) AS s
             FROM t
             GROUP BY g",
        )
        .unwrap();
    assert_eq!(
        qr.rows,
        vec![vec![BqValue::String("a".to_owned()), BqValue::Int64(30)]]
    );

    // Side note from issue #15: tuple syntax for subsequent elements in UNNEST([...])
    let tuple_qr = eng
        .query(
            "SELECT g, TIMESTAMP_DIFF(MAX(ts), MIN(ts), SECOND) AS s
             FROM UNNEST([
               STRUCT('a' AS g, TIMESTAMP '2026-01-01 00:00:00 UTC' AS ts),
               ('a', TIMESTAMP '2026-01-01 00:00:45 UTC')
             ])
             GROUP BY g",
        )
        .unwrap();
    assert_eq!(
        tuple_qr.rows,
        vec![vec![BqValue::String("a".to_owned()), BqValue::Int64(45)]]
    );

    // DATE_TRUNC / DATE_DIFF wrapping aggregates also work
    let trunc_qr = eng
        .query(
            "SELECT DATE_TRUNC(MAX(d), MONTH) AS m, DATE_DIFF(MAX(d), MIN(d), DAY) AS days
             FROM UNNEST([DATE '2026-05-01', DATE '2026-05-15']) AS d",
        )
        .unwrap();
    assert_eq!(trunc_qr.rows[0][0].to_string(), "2026-05-01");
    assert_eq!(trunc_qr.rows[0][1], BqValue::Int64(14));
}

#[test]
fn issue_16_string_literal_coercion_in_generate_date_array_and_datetime_fns() {
    let eng = Engine::new();

    let qr = eng
        .query(
            "SELECT
               ARRAY_LENGTH(GENERATE_DATE_ARRAY('2023-01-01', DATE '2023-01-05', INTERVAL 1 DAY)) AS n1,
               ARRAY_LENGTH(GENERATE_DATE_ARRAY('2023-01-01', '2023-01-03')) AS n2,
               DATE_ADD('2023-01-01', INTERVAL 2 DAY) AS d_add,
               DATE_DIFF('2023-01-10', '2023-01-01', DAY) AS d_diff,
               DATE_TRUNC('2023-05-19', MONTH) AS d_trunc,
               LAST_DAY('2024-02-10') AS ld",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Int64(5));
    assert_eq!(qr.rows[0][1], BqValue::Int64(3));
    assert_eq!(qr.rows[0][2].to_string(), "2023-01-03");
    assert_eq!(qr.rows[0][3], BqValue::Int64(9));
    assert_eq!(qr.rows[0][4].to_string(), "2023-05-01");
    assert_eq!(qr.rows[0][5].to_string(), "2024-02-29");
}

#[test]
fn issue_17_to_json_string_rfc3339_temporal_and_struct_field_order() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               TO_JSON_STRING(STRUCT(
                 TIMESTAMP '2026-11-24 12:34:56.789 UTC' AS ts,
                 DATETIME '2026-11-24 12:34:56' AS dt
               )) AS j,
               TO_JSON_STRING(TIMESTAMP '2026-11-24 12:34:56 UTC') AS ts_alone,
               TO_JSON_STRING(STRUCT(1 AS z, 2 AS a, 3 AS m)) AS ordered_struct,
               TO_JSON_STRING(JSON_OBJECT('z', 1, 'a', 2)) AS sorted_json_obj",
        )
        .unwrap();

    assert_eq!(
        qr.rows[0][0],
        BqValue::String(
            r#"{"ts":"2026-11-24T12:34:56.789Z","dt":"2026-11-24T12:34:56"}"#.to_owned()
        )
    );
    assert_eq!(
        qr.rows[0][1],
        BqValue::String(r#""2026-11-24T12:34:56Z""#.to_owned())
    );
    assert_eq!(
        qr.rows[0][2],
        BqValue::String(r#"{"z":1,"a":2,"m":3}"#.to_owned())
    );
    assert_eq!(
        qr.rows[0][3],
        BqValue::String(r#"{"a":2,"z":1}"#.to_owned())
    );
}

#[test]
fn tier1_extract_at_time_zone_intervals_buckets_and_time_fns() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               EXTRACT(HOUR FROM TIMESTAMP '2026-01-15 02:00:00 UTC' AT TIME ZONE 'America/New_York') AS ny_hr,
               EXTRACT(DATE FROM TIMESTAMP '2026-01-15 02:00:00 UTC' AT TIME ZONE '-05:00') AS off_date,
               INTERVAL '1-2 3 04:05:06' YEAR TO SECOND AS iv1,
               MAKE_INTERVAL(year => 1, day => 5) AS iv2,
               JUSTIFY_DAYS(INTERVAL 35 DAY) AS jd,
               JUSTIFY_HOURS(INTERVAL 26 HOUR) AS jh,
               DATE_BUCKET(DATE '2026-01-10', INTERVAL 7 DAY, DATE '2026-01-01') AS db,
               TIMESTAMP_BUCKET(TIMESTAMP '2026-01-01 00:35:00 UTC', INTERVAL 15 MINUTE) AS tb,
               TIME_ADD(TIME '10:30:00', INTERVAL 45 MINUTE) AS t_add,
               TIME_TRUNC(TIME '10:35:42', MINUTE) AS t_trunc,
               TIME_DIFF(TIME '12:30:00', TIME '10:00:00', MINUTE) AS t_diff",
        )
        .unwrap();

    assert_eq!(qr.rows[0][0], BqValue::Int64(21));
    assert_eq!(qr.rows[0][1].to_string(), "2026-01-14");
    assert_eq!(qr.columns[2].ty, BqType::Interval);
    assert_eq!(qr.columns[3].ty, BqType::Interval);
    assert_eq!(qr.rows[0][6].to_string(), "2026-01-08");
    assert_eq!(qr.rows[0][7].to_string(), "2026-01-01 00:30:00 UTC");
    assert_eq!(qr.rows[0][8].to_string(), "11:15:00");
    assert_eq!(qr.rows[0][9].to_string(), "10:35:00");
    assert_eq!(qr.rows[0][10], BqValue::Int64(150));

    let err_tz = eng
        .query("SELECT EXTRACT(HOUR FROM TIMESTAMP '2026-01-15 02:00:00 UTC' AT TIME ZONE 'Not/AZone')")
        .unwrap_err();
    assert_eq!(err_tz.kind, BqErrorKind::Semantic);
}

#[test]
fn tier1_bytes_parity() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               CONCAT(b'ab', b'cd') AS c_b,
               SUBSTR(b'abcdef', 2, 3) AS sub_b,
               LEFT(b'abcdef', 2) AS l_b,
               RIGHT(b'abcdef', 2) AS r_b,
               REVERSE(b'abc') AS rev_b,
               STARTS_WITH(b'hello', b'he') AS sw_b,
               ENDS_WITH(b'hello', b'lo') AS ew_b,
               STRPOS(b'hello', b'll') AS sp_b,
               REPLACE(b'aabaa', b'a', b'x') AS rep_b,
               TRIM(b'xxabxx', b'x') AS tr_b,
               LPAD(b'ab', 4, b'0') AS lp_b,
               REPEAT(b'ab', 2) AS rpt_b,
               ARRAY_LENGTH(SPLIT(b'a,b,c', b',')) AS spl_b",
        )
        .unwrap();

    assert_eq!(
        qr.rows[0],
        vec![
            BqValue::Bytes(b"abcd".to_vec()),
            BqValue::Bytes(b"bcd".to_vec()),
            BqValue::Bytes(b"ab".to_vec()),
            BqValue::Bytes(b"ef".to_vec()),
            BqValue::Bytes(b"cba".to_vec()),
            BqValue::Bool(true),
            BqValue::Bool(true),
            BqValue::Int64(3),
            BqValue::Bytes(b"xxbxx".to_vec()),
            BqValue::Bytes(b"ab".to_vec()),
            BqValue::Bytes(b"00ab".to_vec()),
            BqValue::Bytes(b"abab".to_vec()),
            BqValue::Int64(3),
        ]
    );
}

#[test]
fn tier1_code_point_functions() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               OCTET_LENGTH('hé') AS oct_s,
               ASCII('A') AS asc_a,
               UNICODE('é') AS uni_e,
               TO_CODE_POINTS('AB') AS cps,
               CODE_POINTS_TO_STRING([65, 66]) AS cp_s,
               CODE_POINTS_TO_BYTES([65, 66]) AS cp_b",
        )
        .unwrap();

    assert_eq!(
        qr.rows[0],
        vec![
            BqValue::Int64(3),
            BqValue::Int64(65),
            BqValue::Int64(233),
            BqValue::Array(vec![BqValue::Int64(65), BqValue::Int64(66)]),
            BqValue::String("AB".to_owned()),
            BqValue::Bytes(b"AB".to_vec()),
        ]
    );

    let safe_bad_cp = eng
        .query("SELECT SAFE.CODE_POINTS_TO_BYTES([300]) AS b")
        .unwrap();
    assert_eq!(safe_bad_cp.rows[0][0], BqValue::Null);
}

#[test]
fn tier1_round_modes_order_by_nan_lag_default_having_agg_and_dml_default() {
    let mut eng = Engine::new();

    // ROUND rounding_mode (ROUND_HALF_EVEN vs ROUND_HALF_AWAY_FROM_ZERO)
    let r = eng
        .query(
            "SELECT
               ROUND(2.5, 0, 'ROUND_HALF_EVEN') AS fe2,
               ROUND(3.5, 0, 'ROUND_HALF_EVEN') AS fe3,
               ROUND(NUMERIC '2.5', 0, 'ROUND_HALF_EVEN') AS ne2,
               ROUND(NUMERIC '2.5', 0, 'ROUND_HALF_AWAY_FROM_ZERO') AS na2",
        )
        .unwrap();
    assert_eq!(r.rows[0][0], BqValue::Float64(2.0));
    assert_eq!(r.rows[0][1], BqValue::Float64(4.0));
    assert_eq!(r.rows[0][2].to_string(), "2");
    assert_eq!(r.rows[0][3].to_string(), "3");

    // ORDER BY NaN sorts deterministically after NULL and before -inf
    let nan_ord = eng
        .query(
            "SELECT x FROM UNNEST([1.0, CAST('NaN' AS FLOAT64), -1.0, CAST(NULL AS FLOAT64)]) AS x
             ORDER BY x ASC",
        )
        .unwrap();
    assert_eq!(nan_ord.rows[0][0], BqValue::Null);
    assert!(matches!(nan_ord.rows[1][0], BqValue::Float64(f) if f.is_nan()));
    assert_eq!(nan_ord.rows[2][0], BqValue::Float64(-1.0));
    assert_eq!(nan_ord.rows[3][0], BqValue::Float64(1.0));

    // LAG constant default & offset=0
    let lag_qr = eng
        .query(
            "SELECT x, LAG(x, 1, 99) OVER (ORDER BY x) AS l1, LAG(x, 0) OVER (ORDER BY x) AS l0
             FROM UNNEST([1, 2]) AS x ORDER BY x",
        )
        .unwrap();
    assert_eq!(
        lag_qr.rows[0],
        vec![BqValue::Int64(1), BqValue::Int64(99), BqValue::Int64(1)]
    );
    assert_eq!(
        lag_qr.rows[1],
        vec![BqValue::Int64(2), BqValue::Int64(1), BqValue::Int64(2)]
    );

    // General HAVING MAX on SUM / ARRAY_AGG + tuple IN + CORRESPONDING
    let hav = eng
        .query(
            "SELECT SUM(v HAVING MAX ts) AS s
             FROM UNNEST([STRUCT(10 AS v, 1 AS ts), (20, 2), (30, 2)])",
        )
        .unwrap();
    assert_eq!(hav.rows[0][0], BqValue::Int64(50));

    let tin = eng
        .query("SELECT (1, 'a') IN ((1, 'a'), (2, 'b')) AS hit")
        .unwrap();
    assert_eq!(tin.rows[0][0], BqValue::Bool(true));

    let corr = eng
        .query("SELECT 1 AS a, 2 AS b UNION ALL CORRESPONDING SELECT 20 AS b, 10 AS a")
        .unwrap();
    assert_eq!(
        corr.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Int64(2)],
            vec![BqValue::Int64(10), BqValue::Int64(20)],
        ]
    );

    // INSERT / UPDATE DEFAULT keyword
    eng.exec_script(
        "CREATE SCHEMA ds;
         CREATE TABLE ds.defs (id INT64, label STRING DEFAULT 'init');
         INSERT INTO ds.defs VALUES (1, DEFAULT);
         UPDATE ds.defs SET label = 'changed' WHERE id = 1;
         UPDATE ds.defs SET label = DEFAULT WHERE id = 1;",
    )
    .unwrap();
    let def_qr = eng.query("SELECT id, label FROM ds.defs").unwrap();
    assert_eq!(
        def_qr.rows,
        vec![vec![BqValue::Int64(1), BqValue::String("init".to_owned())]]
    );
}
