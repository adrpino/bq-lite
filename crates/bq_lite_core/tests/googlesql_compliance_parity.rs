use bq_lite_core::{BqValue, Engine};

#[test]
fn interval_and_civil_time_parity() {
    let e = Engine::new();
    let res = e
        .query(
            "SELECT
                CAST(CAST('P1Y2M3DT4H5M6.789S' AS INTERVAL) AS STRING),
                CAST(INTERVAL '-20 30' MONTH TO DAY AS STRING),
                CAST(INTERVAL 1 MONTH * 3.5 AS STRING),
                CAST(INTERVAL 2 HOUR / 4 AS STRING),
                INTERVAL 1 MONTH = INTERVAL 30 DAY,
                EXTRACT(YEAR FROM JUSTIFY_INTERVAL(INTERVAL '1-2 -12 -40:5:6.789' YEAR TO SECOND)),
                EXTRACT(HOUR FROM JUSTIFY_INTERVAL(INTERVAL '1-2 -12 -40:5:6.789' YEAR TO SECOND)),
                ADD_MONTHS(DATE '2025-02-28', 1),
                NEXT_DAY(DATE '2025-10-09', 'Thu'),
                LAST_DAY(DATE '2020-07-24', WEEK(MONDAY)),
                MONTHS_BETWEEN(DATE '2025-11-30', DATE '2024-10-31'),
                SAFE_CAST('bad' AS INTERVAL),
                ADD_MONTHS(CAST(NULL AS DATE), 1),
             ",
        )
        .unwrap();
    assert_eq!(
        res.rows[0][0],
        BqValue::String("1-2 3 4:5:6.789".to_owned())
    );
    assert_eq!(res.rows[0][1], BqValue::String("-1-8 30 0:0:0".to_owned()));
    assert_eq!(res.rows[0][2], BqValue::String("0-3 15 0:0:0".to_owned()));
    assert_eq!(res.rows[0][3], BqValue::String("0-0 0 0:30:0".to_owned()));
    assert_eq!(res.rows[0][4], BqValue::Bool(true));
    assert_eq!(res.rows[0][5], BqValue::Int64(1));
    assert_eq!(res.rows[0][6], BqValue::Int64(7));
    assert_eq!(
        res.rows[0][7],
        BqValue::Date(chrono::NaiveDate::from_ymd_opt(2025, 3, 31).unwrap())
    );
    assert_eq!(
        res.rows[0][8],
        BqValue::Date(chrono::NaiveDate::from_ymd_opt(2025, 10, 16).unwrap())
    );
    assert_eq!(
        res.rows[0][9],
        BqValue::Date(chrono::NaiveDate::from_ymd_opt(2020, 7, 26).unwrap())
    );
    assert_eq!(res.rows[0][10], BqValue::Float64(13.0));
    assert!(res.rows[0][11].is_null());
    assert!(res.rows[0][12].is_null());

    // Overflow & invalid errors
    assert!(e.query("SELECT ADD_MONTHS(DATE '9999-12-25', 1)").is_err());
    assert!(e
        .query("SELECT NEXT_DAY(DATE '2025-10-09', 'Moonday')")
        .is_err());
    assert!(e.query("SELECT CAST('P-' AS INTERVAL)").is_err());
}

#[test]
fn cast_format_json_and_range_parity() {
    let e = Engine::new();
    let res = e
        .query(
            "SELECT
                CAST(b'abc' AS STRING FORMAT 'base32'),
                CAST('MFRGG===' AS BYTES FORMAT 'base32'),
                CAST('-0x100' AS INT64),
                SAFE_CAST(123456 AS STRING(5)),
                CAST([1, NULL] AS JSON),
                JSON_QUERY(JSON '{\"a\":[{\"b\":1},[{\"b\":2}]]}', 'lax recursive $.a.b'),
                JSON_ARRAY_INSERT(JSON '{\"a\":[10,null]}', '$.a[1]', 'foo'),
                JSON_ARRAY_APPEND(JSON '{\"a\":[10]}', '$.a', 20),
                ARRAY_LENGTH(JSON_FLATTEN(JSON '[[1,[2]],3]')),
                RANGE_IS_START_UNBOUNDED(RANGE(CAST(NULL AS DATE), DATE '2025-01-01')),
                RANGE_IS_END_UNBOUNDED(RANGE(CAST(NULL AS DATE), CAST(NULL AS DATE))),
                SAFE.RANGE(DATE '2025-01-01', DATE '2025-01-01'),
                SAFE.RANGE_INTERSECT(RANGE(DATE '2025-01-01', DATE '2025-01-05'), RANGE(DATE '2025-01-10', DATE '2025-01-20'))",
        )
        .unwrap();
    assert_eq!(res.rows[0][0], BqValue::String("MFRGG===".to_owned()));
    assert_eq!(res.rows[0][1], BqValue::Bytes(b"abc".to_vec()));
    assert_eq!(res.rows[0][2], BqValue::Int64(-256));
    assert!(res.rows[0][3].is_null());
    assert_eq!(res.rows[0][4].to_string(), "[1,null]");
    assert_eq!(res.rows[0][5].to_string(), "[1,2]");
    assert_eq!(res.rows[0][6].to_string(), "{\"a\":[10,\"foo\",null]}");
    assert_eq!(res.rows[0][7].to_string(), "{\"a\":[10,20]}");
    assert_eq!(res.rows[0][8], BqValue::Int64(3));
    assert_eq!(res.rows[0][9], BqValue::Bool(true));
    assert_eq!(res.rows[0][10], BqValue::Bool(true));
    assert!(res.rows[0][11].is_null());
    assert!(res.rows[0][12].is_null());

    assert!(e.query("SELECT CAST(123456 AS STRING(5))").is_err());
    assert!(e
        .query("SELECT RANGE(DATE '2025-01-01', DATE '2025-01-01')")
        .is_err());
    assert!(e
        .query("SELECT RANGE_INTERSECT(RANGE(DATE '2025-01-01', DATE '2025-01-02'), RANGE(DATE '2025-01-05', DATE '2025-01-06'))")
        .is_err());
}

#[test]
fn strings_bytes_arrays_aggregates_and_tablesample_parity() {
    let e = Engine::new();
    let res = e
        .query(
            "SELECT
                SPLIT_SUBSTR('www.abc.com', '.', -2, 2),
                CHR(97),
                SOUNDEX('Robert'),
                BIT_COUNT(b'a'),
                'abc' '123',
                ARRAY_IS_DISTINCT([1, 2, 3]),
                ARRAY_IS_DISTINCT([1, NULL, NULL]),
                ARRAY_INCLUDES([1, 2, 3], x -> x > 2),
                ARRAY_INCLUDES_ALL([1, 2, 3], [1, 3]),
                ARRAY_LENGTH(ARRAY_FIRST_N([1, 2, 3, 4], 2)),
                ARRAY_OFFSET(['a', 'b', 'c'], 'b'),
                DEGREES(RADIANS(180)),
                SAFE.ASCII('\\xFF'),
                SAFE.TRANSLATE('apple', 'aa', 'bc'),
                SAFE.REGEXP_INSTR('abc', '(a)(b)'),
                SAFE.PARSE_NUMERIC(' - 1 2.34 ')",
        )
        .unwrap();
    assert_eq!(res.rows[0][0], BqValue::String("abc.com".to_owned()));
    assert_eq!(res.rows[0][1], BqValue::String("a".to_owned()));
    assert_eq!(res.rows[0][2], BqValue::String("R163".to_owned()));
    assert_eq!(res.rows[0][3], BqValue::Int64(3));
    assert_eq!(res.rows[0][4], BqValue::String("abc123".to_owned()));
    assert_eq!(res.rows[0][5], BqValue::Bool(true));
    assert_eq!(res.rows[0][6], BqValue::Bool(false));
    assert_eq!(res.rows[0][7], BqValue::Bool(true));
    assert_eq!(res.rows[0][8], BqValue::Bool(true));
    assert_eq!(res.rows[0][9], BqValue::Int64(2));
    assert_eq!(res.rows[0][10], BqValue::Int64(1));
    assert_eq!(res.rows[0][11], BqValue::Float64(180.0));
    assert!(res.rows[0][12].is_null());
    assert!(res.rows[0][13].is_null());
    assert!(res.rows[0][14].is_null());
    assert!(res.rows[0][15].is_null());
}

#[test]
fn aggregates_bytes_and_tablesample_parity() {
    let e = Engine::new();
    // BIT_AND/OR/XOR on BYTES, COUNTIF(DISTINCT), ARRAY_CONCAT_AGG, TABLESAMPLE
    let agg = e
        .query(
            "SELECT
                BIT_AND(b), BIT_OR(b), BIT_XOR(b),
                COUNTIF(DISTINCT b IS NOT NULL),
                STRING_AGG(CAST(b AS STRING)) IN ('ab,ac', 'ac,ab')
             FROM UNNEST([b'ab', b'ac']) AS b",
        )
        .unwrap();
    assert_eq!(agg.rows[0][0], BqValue::Bytes(vec![b'a', b'b' & b'c']));
    assert_eq!(agg.rows[0][1], BqValue::Bytes(vec![b'a', b'b' | b'c']));
    assert_eq!(agg.rows[0][2], BqValue::Bytes(vec![0, b'b' ^ b'c']));
    assert_eq!(agg.rows[0][3], BqValue::Int64(1));
    assert_eq!(agg.rows[0][4], BqValue::Bool(true));

    let ts = e
        .query("SELECT COUNT(*) FROM (SELECT * FROM UNNEST(GENERATE_ARRAY(1, 50))) TABLESAMPLE RESERVOIR(5 ROWS)")
        .unwrap();
    assert_eq!(ts.rows[0][0], BqValue::Int64(5));
}

#[test]
fn set_ops_by_name_struct_accessors_with_expr_and_temporal_coercion_parity() {
    let e = Engine::new();

    // INNER / LEFT / FULL UNION ALL BY NAME [ON (...)]
    let full = e
        .query(
            "SELECT 1 AS b, 2 AS a, 5 AS z
             FULL UNION ALL BY NAME ON (B, z, y)
             SELECT 6 AS y, 3 AS b, 4 AS a
             ORDER BY B",
        )
        .unwrap();
    assert_eq!(
        full.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["B", "z", "y"]
    );
    assert_eq!(
        full.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Int64(5), BqValue::Null],
            vec![BqValue::Int64(3), BqValue::Null, BqValue::Int64(6)],
        ]
    );

    // STRUCT positional accessors, empty STRUCT<>(), WITH(...) expressions + expr.*
    let s = e
        .query(
            "SELECT
                STRUCT(10, 'hi')[OFFSET(0)],
                STRUCT(10, 'hi')[ORDINAL(2)],
                STRUCT(10, 'hi')[SAFE_OFFSET(9)],
                STRUCT<>() IS DISTINCT FROM CAST(NULL AS STRUCT<>),
                WITH(x AS ARRAY_AGG(v ORDER BY v), STRUCT(ARRAY_LENGTH(x) AS n, x[OFFSET(0)] AS first)).*
             FROM UNNEST([3, 1, 2]) AS v",
        )
        .unwrap();
    assert_eq!(s.rows[0][0], BqValue::Int64(10));
    assert_eq!(s.rows[0][1], BqValue::String("hi".to_owned()));
    assert!(s.rows[0][2].is_null());
    assert_eq!(s.rows[0][3], BqValue::Bool(true));
    assert_eq!(s.rows[0][4], BqValue::Int64(3));
    assert_eq!(s.rows[0][5], BqValue::Int64(1));
    assert!(e.query("SELECT STRUCT(1)[OFFSET(5)]").is_err());

    // IANA tz TIMESTAMP literals, temporal BETWEEN/IN with string literals, ARRAY<NUMERIC> float coercion
    let t = e
        .query(
            "SELECT
                TIMESTAMP '2013-07-22 16:24:00 Europe/London' > TIMESTAMP '2011-04-29 10:48:00 Europe/London',
                DATE '2020-05-01' BETWEEN '2020-01-01' AND '2020-12-31',
                DATE '2020-05-01' IN ('2020-05-01', '2020-06-01'),
                CAST(1.25 AS NUMERIC) IN UNNEST(ARRAY<NUMERIC>[1, 1.25, 2]),
                ROUND(NUMERIC '2.45', 1, CAST('ROUND_HALF_AWAY_FROM_ZERO' AS ROUNDING_MODE)),
                ST_X(ST_GEOGPOINT(-122.4, 37.8)),
                ST_Y(ST_GEOGPOINT(-122.4, 37.8))",
        )
        .unwrap();
    assert_eq!(t.rows[0][0], BqValue::Bool(true));
    assert_eq!(t.rows[0][1], BqValue::Bool(true));
    assert_eq!(t.rows[0][2], BqValue::Bool(true));
    assert_eq!(t.rows[0][3], BqValue::Bool(true));
    assert_eq!(t.rows[0][4].to_string(), "2.5");
    assert_eq!(t.rows[0][5], BqValue::Float64(-122.4));
    assert_eq!(t.rows[0][6], BqValue::Float64(37.8));
}

#[test]
fn grouping_sets_recursive_depth_pivot_constants_and_pk_dml_parity() {
    let mut e = Engine::new();

    // GROUPING SETS expression match + QUALIFY GROUPING(a) = 0
    let gs = e
        .query(
            "WITH t AS (SELECT 10 AS a, 'x' AS b UNION ALL SELECT 20, 'y')
             SELECT a, a + 1, COUNT(*) OVER ()
             FROM t
             GROUP BY GROUPING SETS(a, a + 1)
             QUALIFY GROUPING(a) = 0
             ORDER BY a",
        )
        .unwrap();
    assert_eq!(
        gs.rows,
        vec![
            vec![BqValue::Int64(10), BqValue::Int64(11), BqValue::Int64(4)],
            vec![BqValue::Int64(20), BqValue::Int64(21), BqValue::Int64(4)],
        ]
    );

    // WITH RECURSIVE WITH DEPTH AS depth BETWEEN 1 AND 3 + pipe set op recursion
    let rec = e
        .query(
            "WITH RECURSIVE t AS (
                SELECT 0 AS n
                UNION ALL
                SELECT MOD(n + 1, 3) AS n FROM t
             ) WITH DEPTH AS depth BETWEEN 1 AND 3
             SELECT n, depth FROM t ORDER BY depth",
        )
        .unwrap();
    assert_eq!(
        rec.rows,
        vec![
            vec![BqValue::Int64(1), BqValue::Int64(1)],
            vec![BqValue::Int64(2), BqValue::Int64(2)],
            vec![BqValue::Int64(0), BqValue::Int64(3)],
        ]
    );
    let pipe_rec = e
        .query(
            "WITH RECURSIVE t AS (
                (SELECT 1 AS a, 2 AS b)
                |> UNION DISTINCT BY NAME
                (SELECT b, a FROM t)
             )
             SELECT a, b FROM t",
        )
        .unwrap();
    assert_eq!(
        pipe_rec.rows,
        vec![vec![BqValue::Int64(1), BqValue::Int64(2)]]
    );

    // CREATE TEMP CONSTANT + PIVOT FOR (expr) + empty input + UNPIVOT qualified names
    e.exec("CREATE TEMP CONSTANT C10 = 10").unwrap();
    let piv = e
        .query(
            "SELECT _10 FROM (SELECT 10 AS k, 5 AS v WHERE FALSE)
             PIVOT(ANY_VALUE(v) FOR (k) IN (C10))",
        )
        .unwrap();
    assert_eq!(piv.rows, vec![vec![BqValue::Null]]);
    let unpiv = e
        .query(
            "SELECT lbl, val FROM (SELECT 1 AS a, 2 AS b) AS t
             UNPIVOT(val FOR lbl IN (t.a, t.b))
             ORDER BY lbl",
        )
        .unwrap();
    assert_eq!(
        unpiv.rows,
        vec![
            vec![BqValue::String("a".to_owned()), BqValue::Int64(1)],
            vec![BqValue::String("b".to_owned()), BqValue::Int64(2)],
        ]
    );
}

#[test]
fn primary_key_dml_modes_and_safe_analytic_parity() {
    let mut e = Engine::new();

    // PRIMARY KEY + INSERT OR IGNORE / REPLACE / UPDATE + THEN RETURN WITH ACTION + UPDATE PK error
    e.exec("CREATE SCHEMA pkds").unwrap();
    e.exec("CREATE TABLE pkds.kv (id INT64, v INT64, extra INT64, PRIMARY KEY (id))")
        .unwrap();
    e.exec("INSERT INTO pkds.kv VALUES (1, 10, 100), (2, 20, 200)")
        .unwrap();
    assert!(e.exec("INSERT INTO pkds.kv VALUES (1, 99, 999)").is_err());

    let ign = e
        .exec("INSERT OR IGNORE INTO pkds.kv VALUES (1, 99, 999), (3, 30, 300) THEN RETURN WITH ACTION *")
        .unwrap();
    assert_eq!(ign.rows_affected, 1);
    assert_eq!(ign.rows[0][3], BqValue::String("INSERT".to_owned()));

    let upd = e
        .exec("INSERT OR UPDATE INTO pkds.kv (id, v) VALUES (1, 55) THEN RETURN WITH ACTION *")
        .unwrap();
    assert_eq!(
        upd.rows[0],
        vec![
            BqValue::Int64(1),
            BqValue::Int64(55),
            BqValue::Int64(100),
            BqValue::String("UPDATE".to_owned()),
        ]
    );

    let rep = e
        .exec("INSERT OR REPLACE INTO pkds.kv (id, v) VALUES (2, 77) THEN RETURN WITH ACTION *")
        .unwrap();
    assert_eq!(
        rep.rows[0],
        vec![
            BqValue::Int64(2),
            BqValue::Int64(77),
            BqValue::Null,
            BqValue::String("REPLACE".to_owned()),
        ]
    );

    // UPDATE causing duplicate PK must fail
    assert!(e.exec("UPDATE pkds.kv SET id = 2 WHERE id = 1").is_err());

    // SAFE.SUM analytic suppresses INT64 overflow, but propagates arg division-by-zero
    let safe_win = e
        .query(
            "SELECT SAFE.SUM(v) OVER (ORDER BY k)
             FROM (SELECT 1 AS k, 9223372036854775807 AS v UNION ALL SELECT 2, 1)
             ORDER BY k",
        )
        .unwrap();
    assert_eq!(safe_win.rows[0][0], BqValue::Int64(i64::MAX));
    assert!(safe_win.rows[1][0].is_null());
    assert!(e
        .query("SELECT SAFE.SUM(1 / 0) OVER () FROM (SELECT 1 AS x)")
        .is_err());
}
