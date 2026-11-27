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
