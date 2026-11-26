use bq_lite_core::{BqValue, Engine};

#[test]
fn covers_all_datetime_functions_and_errors() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             CURRENT_DATE('UTC'), CURRENT_DATETIME(), CURRENT_TIME(), CURRENT_TIMESTAMP(), \
             DATE(2024, 5, 19), DATE(TIMESTAMP '2024-05-19 23:00:00 UTC', 'America/New_York'), \
             DATETIME(DATE '2024-05-19', TIME '10:20:30'), \
             DATETIME(TIMESTAMP '2024-05-19 10:20:30 UTC', 'UTC'), \
             TIME(10, 20, 30), \
             TIMESTAMP(DATETIME '2024-05-19 10:20:30', 'UTC'), \
             DATE_ADD(DATE '2024-01-31', INTERVAL 1 MONTH), \
             DATE_SUB(DATE '2024-03-01', INTERVAL 1 DAY), \
             DATE_DIFF(DATE '2024-03-01', DATE '2024-01-01', MONTH), \
             DATE_TRUNC(DATE '2024-05-19', QUARTER), \
             DATE_TRUNC(DATE '2024-05-19', YEAR), \
             DATETIME_ADD(DATETIME '2024-01-01 00:00:00', INTERVAL 1 HOUR), \
             DATETIME_SUB(DATETIME '2024-01-02 00:00:00', INTERVAL 1 DAY), \
             DATETIME_DIFF(DATETIME '2024-01-02 01:00:00', DATETIME '2024-01-01 00:00:00', HOUR), \
             DATETIME_TRUNC(DATETIME '2024-05-19 12:34:56', MINUTE), \
             TIMESTAMP_ADD(TIMESTAMP '2024-01-01 00:00:00 UTC', INTERVAL 30 SECOND), \
             TIMESTAMP_SUB(TIMESTAMP '2024-01-01 01:00:00 UTC', INTERVAL 10 MINUTE), \
             TIMESTAMP_DIFF(TIMESTAMP '2024-01-01 01:00:00 UTC', TIMESTAMP '2024-01-01 00:00:00 UTC', SECOND), \
             TIMESTAMP_TRUNC(TIMESTAMP '2024-05-19 12:34:56 UTC', HOUR), \
             TIMESTAMP_SECONDS(1700000000), TIMESTAMP_MILLIS(1700000000000), TIMESTAMP_MICROS(1700000000000000), \
             UNIX_SECONDS(TIMESTAMP '2023-11-14 22:13:20 UTC'), UNIX_MILLIS(TIMESTAMP '2023-11-14 22:13:20 UTC'), \
             UNIX_MICROS(TIMESTAMP '2023-11-14 22:13:20 UTC'), UNIX_DATE(DATE '1970-01-02'), DATE_FROM_UNIX_DATE(1), \
             LAST_DAY(DATE '2024-02-10'), ARRAY_LENGTH(GENERATE_DATE_ARRAY(DATE '2024-01-01', DATE '2024-01-05', INTERVAL 2 DAY)), \
             FORMAT_DATE('%Y-%m-%d', DATE '2024-05-19'), PARSE_DATE('%Y/%m/%d', '2024/05/19'), \
             FORMAT_DATETIME('%Y-%m-%d %H:%M', DATETIME '2024-05-19 10:20:00'), \
             FORMAT_TIME('%H:%M', TIME '10:20:00'), \
             FORMAT_TIMESTAMP('%Y-%m-%d', TIMESTAMP '2024-05-19 10:20:00 UTC'), \
             SAFE.DATE(2024, 2, 31), SAFE.PARSE_DATE('%Y-%m-%d', 'bad')",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][38], BqValue::Null);
    assert_eq!(qr.rows[0][39], BqValue::Null);
}

#[test]
fn covers_math_hash_regex_array_string_and_json_functions() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             ABS(-5), SIGN(-5), ROUND(3.14159, 2), TRUNC(3.9), CEIL(3.1), FLOOR(3.9), \
             MOD(10, 3), DIV(10, 3), IEEE_DIVIDE(1.0, 0.0), SAFE_DIVIDE(1, 0), \
             SAFE_ADD(1, 2), SAFE_SUBTRACT(5, 2), SAFE_MULTIPLY(3, 4), SAFE_NEGATE(5), \
             POW(2, 3), SQRT(16), CBRT(27), EXP(0), LN(1), LOG(100, 10), LOG10(1000), \
             GREATEST(1, 5, 3), LEAST(1, 5, 3), RAND() >= 0, RANGE_BUCKET(5, [0, 5, 10]), \
             TO_HEX(MD5('a')), TO_HEX(SHA1('a')), TO_HEX(SHA256('a')), TO_HEX(SHA512('a')), \
             TO_BASE64(b'abc'), FROM_BASE64('YWJj'), FROM_HEX('6162'), \
             LPAD('x', 3, '0'), RPAD('x', 3, '0'), TRANSLATE('abc', 'ab', 'xy'), INITCAP('hello world'), \
             REVERSE('abc'), REPEAT('a', 3), STRPOS('abc', 'b'), INSTR('abcabc', 'b', 1, 2), \
             BYTE_LENGTH('abc'), LEFT('hello', 2), RIGHT('hello', 2), FORMAT('%s=%d', 'n', 7), \
             REGEXP_EXTRACT_ALL('a1b2', r'\\d'), REGEXP_INSTR('abc123', r'\\d+'), \
             REGEXP_REPLACE('a1b2', r'\\d', 'X'), REGEXP_SUBSTR('abc123', r'\\d+'), \
             ARRAY_FIRST([10, 20]), ARRAY_LAST([10, 20]), ARRAY_SLICE([1, 2, 3, 4], 1, 2), \
             ARRAY_LENGTH(GENERATE_ARRAY(1, 5, 2)), \
             JSON_TYPE(PARSE_JSON('{\"a\":1}')), JSON_KEYS(PARSE_JSON('{\"a\":1,\"b\":2}')), \
             JSON_SET(PARSE_JSON('{\"a\":1}'), '$.b', 2), JSON_REMOVE(PARSE_JSON('{\"a\":1,\"b\":2}'), '$.b'), \
             INT64(PARSE_JSON('42')), FLOAT64(PARSE_JSON('3.5')), BOOL(PARSE_JSON('true')), STRING(PARSE_JSON('\"hi\"'))",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][9], BqValue::Null);
}

#[test]
fn covers_set_ops_pipe_subqueries_and_bitwise_ops() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT (6 & 3), (6 | 1), (6 ^ 3), (1 << 3), (8 >> 2), (~0), \
             (1 IS DISTINCT FROM NULL), (1 IS NOT DISTINCT FROM 1), \
             (NULL IS UNKNOWN), (TRUE IS NOT UNKNOWN), \
             (2 IN UNNEST([1, 2, 3])), \
             ARRAY(SELECT x * 2 FROM UNNEST([1, 2]) AS x ORDER BY x)",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Int64(2));
    assert_eq!(qr.rows[0][10], BqValue::Bool(true));

    let set_qr = eng
        .query(
            "(SELECT 1 AS a, 'x' AS b UNION ALL SELECT 2, 'y' UNION DISTINCT SELECT 1, 'x') \
             INTERSECT DISTINCT SELECT 1 AS a, 'x' AS b",
        )
        .unwrap();
    assert_eq!(set_qr.rows.len(), 1);

    let exc_qr = eng
        .query("SELECT 1 AS a UNION ALL SELECT 2 AS a EXCEPT DISTINCT SELECT 1 AS a")
        .unwrap();
    assert_eq!(exc_qr.rows.len(), 1);
    assert_eq!(exc_qr.rows[0][0], BqValue::Int64(2));

    let pipe_qr = eng
        .query(
            "FROM (SELECT 1 AS a, 10 AS b UNION ALL SELECT 2, 20) \
             |> WHERE a >= 1 \
             |> EXTEND a + b AS total \
             |> SET a = a * 10 \
             |> RENAME b AS score \
             |> DROP total \
             |> ORDER BY a DESC \
             |> LIMIT 1",
        )
        .unwrap();
    assert_eq!(pipe_qr.rows[0][0], BqValue::Int64(20));
}

#[test]
fn covers_json_array_datetime_fmt_and_ops_edge_paths() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT \
             JSON_QUERY(PARSE_JSON('{\"a\":{\"b\":1}}'), '$.a'), \
             JSON_VALUE(PARSE_JSON('{\"a\":\"hi\"}'), '$.a'), \
             JSON_QUERY_ARRAY(PARSE_JSON('{\"a\":[1,2]}'), '$.a'), \
             JSON_VALUE_ARRAY(PARSE_JSON('{\"a\":[\"x\",\"y\"]}'), '$.a'), \
             JSON_EXTRACT('{\"a\":1}', '$.a'), JSON_EXTRACT_SCALAR('{\"a\":\"ok\"}', '$.a'), \
             JSON_EXTRACT_ARRAY('{\"a\":[1,2]}', '$.a'), JSON_EXTRACT_STRING_ARRAY('{\"a\":[\"x\"]}', '$.a'), \
             JSON_OBJECT('k', 1), JSON_ARRAY(1, 'two', TRUE), \
             TO_JSON(STRUCT(1 AS a)), TO_JSON_STRING(STRUCT(1 AS a)), \
             SAFE.INT64(PARSE_JSON('\"bad\"')), SAFE.FLOAT64(PARSE_JSON('\"bad\"')), \
             ARRAY_TO_STRING(['a', 'b'], '-'), ARRAY_CONCAT([1], [2, 3]), ARRAY_REVERSE([1, 2, 3]), \
             PARSE_DATETIME('%Y-%m-%d %H:%M:%S', '2024-05-19 10:20:30'), \
             PARSE_TIME('%H:%M:%S', '10:20:30'), \
             PARSE_TIMESTAMP('%Y-%m-%d %H:%M:%S', '2024-05-19 10:20:30'), \
             FORMAT('%i %f %t %T', 7, 1.5, 'x', 'y'), \
             (NUMERIC '10.5' + NUMERIC '1.5' - NUMERIC '2' * NUMERIC '1'), \
             (DATE '2024-01-01' + INTERVAL 2 DAY - INTERVAL 1 DAY), \
             (5 NOT BETWEEN 10 AND 20), ('abc' NOT LIKE 'z%'), (9 NOT IN (1, 2, 3))",
        )
        .unwrap();
    assert_eq!(qr.rows.len(), 1);
    assert_eq!(qr.rows[0][14], BqValue::String("a-b".to_owned()));
    assert_eq!(qr.rows[0][23], BqValue::Bool(true));
}
