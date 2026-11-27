use bq_lite_core::{BqType, BqValue, Engine};

#[cfg(feature = "js-udf")]
#[test]
fn milestone_k_boa_all_types_and_error_paths() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kjs2;
         CREATE FUNCTION kjs2.echo_bytes(b BYTES) RETURNS BYTES LANGUAGE js AS r\"\"\"
           return new Uint8Array([b[0] + 1, b[1] + 1]);
         \"\"\";
         CREATE FUNCTION kjs2.echo_json(j JSON) RETURNS JSON LANGUAGE js AS r\"\"\"
           j.added = true;
           return j;
         \"\"\";
         CREATE FUNCTION kjs2.echo_date(d DATE) RETURNS DATE LANGUAGE js AS r\"\"\"
           return d;
         \"\"\";
         CREATE FUNCTION kjs2.echo_ts(ts TIMESTAMP) RETURNS TIMESTAMP LANGUAGE js AS r\"\"\"
           return new Date(ts.getTime() + 1000);
         \"\"\";
         CREATE FUNCTION kjs2.echo_misc(
           n NUMERIC, bn BIGNUMERIC, dt DATETIME, tm TIME, iv INTERVAL,
           rg RANGE<DATE>, big INT64, arr ARRAY<STRING>
         ) RETURNS STRING LANGUAGE js AS r\"\"\"
           return [typeof n, typeof bn, dt, tm, iv, rg, big, arr.join('-')].join('|');
         \"\"\";
         CREATE FUNCTION kjs2.ret_arr() RETURNS ARRAY<INT64> LANGUAGE js AS r\"\"\"
           return [10, 20, null];
         \"\"\";
         CREATE FUNCTION kjs2.ret_bigint() RETURNS INT64 LANGUAGE js AS r\"\"\"
           return 42n;
         \"\"\";
         CREATE FUNCTION kjs2.ret_null() RETURNS INT64 LANGUAGE js AS r\"\"\"
           return undefined;
         \"\"\";
         CREATE FUNCTION kjs2.bad_syntax() RETURNS INT64 LANGUAGE js AS r\"\"\"
           return (;
         \"\"\";
         CREATE FUNCTION kjs2.bad_date() RETURNS TIMESTAMP LANGUAGE js AS r\"\"\"
           return new Date('invalid');
         \"\"\";",
    )
    .unwrap();

    let qr = eng
        .query(
            "SELECT
               kjs2.echo_bytes(b'AB'),
               kjs2.echo_json(JSON '{\"a\":1}'),
               kjs2.echo_date(DATE '2026-03-15'),
               kjs2.echo_ts(TIMESTAMP '2026-03-15 00:00:00 UTC'),
               kjs2.ret_arr(),
               kjs2.ret_bigint(),
               kjs2.ret_null(),
               kjs2.echo_misc(
                 NUMERIC '1.5', BIGNUMERIC '2.5',
                 DATETIME '2026-03-15 10:20:30', TIME '10:20:30',
                 INTERVAL 1 DAY, RANGE<DATE> '[2026-01-01, 2026-02-01)',
                 9007199254740999, ['x', 'y']
               )",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Bytes(b"BC".to_vec()));
    assert!(qr.rows[0][1].to_string().contains("\"added\":true"));
    assert_eq!(qr.rows[0][2].to_string(), "2026-03-15");
    assert_eq!(qr.rows[0][3].to_string(), "2026-03-15 00:00:01 UTC");
    assert_eq!(qr.rows[0][5], BqValue::Int64(42));
    assert_eq!(qr.rows[0][6], BqValue::Null);
    assert!(qr.rows[0][7].to_string().contains("9007199254740999|x-y"));

    assert!(eng.query("SELECT kjs2.bad_syntax()").is_err());
    assert!(eng.query("SELECT kjs2.bad_date()").is_err());
}

#[test]
fn milestone_k_scalar_format_json_array_datetime_geo_and_net() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               FORMAT('%d|%i|%u|%f|%g|%s|%t|%T|%p|%%', 42, -7, 9, 3.14, 1.5, 'hi', [1,2], 'q', 1),
               ARRAY_SLICE([10, 20, 30, 40], 1, 2),
               ARRAY_FIRST([10, 20]),
               ARRAY_LAST([10, 20]),
               GENERATE_ARRAY(1, 5, 2),
               GENERATE_DATE_ARRAY(DATE '2026-01-01', DATE '2026-01-03'),
               JSON_SET(JSON '{\"a\":1}', '$.b', 2),
               JSON_REMOVE(JSON '{\"a\":1,\"b\":2}', '$.a'),
               JSON_KEYS(JSON '{\"a\":null,\"b\":1}'),
               JSON_ARRAY(1, 'a', true),
               JSON_OBJECT('k', 10),
               INT64(JSON '7'),
               FLOAT64(JSON '7.5'),
               BOOL(JSON 'true'),
               STRING(JSON '\"ok\"'),
               JSON_TYPE(JSON '[1]'),
               JSON_VALUE_ARRAY(JSON '[\"x\",\"y\"]'),
               JSON_QUERY_ARRAY(JSON '[{\"a\":1}]'),
               TO_BASE64(b'abc'),
               FROM_BASE64('YWJj'),
               TO_HEX(b'ab'),
               FROM_HEX('6162')",
        )
        .unwrap();
    assert_eq!(
        qr.rows[0][0],
        BqValue::String("42|-7|9|3.140000|1.5|hi|[1, 2]|\"q\"|1|%".to_owned())
    );
    assert_eq!(qr.rows[0][18], BqValue::String("YWJj".to_owned()));
    assert_eq!(qr.rows[0][19], BqValue::Bytes(b"abc".to_vec()));
    assert_eq!(qr.rows[0][20], BqValue::String("6162".to_owned()));
    assert_eq!(qr.rows[0][21], BqValue::Bytes(b"ab".to_vec()));
    assert_eq!(
        eng.query("SELECT FORMAT('%o', 8)").unwrap().rows[0][0],
        BqValue::String("10".to_owned())
    );
    assert!(eng.query("SELECT FORMAT('%Z', 8)").is_err());
    assert!(eng.query("SELECT FORMAT('%', 8)").is_err());
    assert!(eng.query("SELECT FROM_BASE64('!!!')").is_err());
    assert!(eng.query("SELECT FROM_HEX('zz')").is_err());
    assert_eq!(qr.rows[0][2], BqValue::Int64(10));
    assert_eq!(qr.rows[0][3], BqValue::Int64(20));
    assert_eq!(qr.rows[0][11], BqValue::Int64(7));
    assert_eq!(qr.rows[0][13], BqValue::Bool(true));
    assert_eq!(qr.rows[0][15], BqValue::String("array".to_owned()));
}

#[test]
fn milestone_k_datetime_geo_and_net() {
    let eng = Engine::new();
    let geo_qr = eng
        .query(
            "SELECT
               DATE_TRUNC(DATE '2026-03-15', MONTH),
               DATETIME_TRUNC(DATETIME '2026-03-15 10:25:30', HOUR),
               TIMESTAMP_TRUNC(TIMESTAMP '2026-03-15 10:25:30 UTC', DAY),
               LAST_DAY(DATE '2026-02-10', MONTH),
               DATE_DIFF(DATE '2026-03-15', DATE '2026-03-01', DAY),
               DATETIME_DIFF(DATETIME '2026-03-15 12:00:00', DATETIME '2026-03-15 10:30:00', MINUTE),
               TIMESTAMP_DIFF(TIMESTAMP '2026-03-15 12:00:00 UTC', TIMESTAMP '2026-03-15 10:30:00 UTC', MINUTE),
               RANGE_OVERLAPS(RANGE<DATE> '[2026-01-01, 2026-01-10)', RANGE<DATE> '[2026-01-05, 2026-01-20)'),
               RANGE_INTERSECT(RANGE<DATE> '[2026-01-01, 2026-01-10)', RANGE<DATE> '[2026-01-05, 2026-01-20)'),
               NET.HOST('https://example.com:8080/a'),
               NET.REG_DOMAIN('sub.example.co.uk'),
               NET.PUBLIC_SUFFIX('sub.example.co.uk'),
               NET.IP_FROM_STRING('127.0.0.1'),
               NET.IP_TO_STRING(NET.IP_FROM_STRING('127.0.0.1')),
               ST_DISTANCE(ST_GEOGPOINT(0, 0), ST_GEOGPOINT(0, 1)),
               ST_CONTAINS(ST_GEOGFROMTEXT('POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))'), ST_GEOGPOINT(1, 1)),
               ST_WITHIN(ST_GEOGPOINT(1, 1), ST_GEOGFROMTEXT('POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))')),
               ST_ASTEXT(ST_GEOGPOINT(1, 2)),
               SEARCH('hello world', 'hello')",
        )
        .unwrap();
    assert_eq!(geo_qr.rows[0][3].to_string(), "2026-02-28");
    assert_eq!(geo_qr.rows[0][4], BqValue::Int64(14));
    assert_eq!(geo_qr.rows[0][5], BqValue::Int64(90));
    assert_eq!(geo_qr.rows[0][6], BqValue::Int64(90));
    assert_eq!(geo_qr.rows[0][7], BqValue::Bool(true));
    assert_eq!(geo_qr.rows[0][9], BqValue::String("example.com".to_owned()));
    assert_eq!(geo_qr.rows[0][13], BqValue::String("127.0.0.1".to_owned()));
    assert_eq!(geo_qr.rows[0][15], BqValue::Bool(true));
    assert_eq!(geo_qr.rows[0][16], BqValue::Bool(true));
    assert_eq!(geo_qr.rows[0][18], BqValue::Bool(true));
}

#[test]
fn milestone_k_subqueries_and_coercion() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA pds2;
         CREATE TABLE pds2.items (id INT64, tag STRING, amt NUMERIC);
         INSERT INTO pds2.items VALUES (1, 'a', NUMERIC '1.25'), (2, 'b', NUMERIC '2.75');
         CREATE TABLE pds2.empty_items (id INT64, x BOOL, d DATE, ts TIMESTAMP);
         CREATE VIEW pds2.v_items AS SELECT id, tag FROM pds2.items;",
    )
    .unwrap();

    let sub = eng
        .query(
            "SELECT
               id,
               EXISTS(SELECT 1 FROM pds2.items i2 WHERE i2.id = items.id AND i2.tag = 'a') AS ex,
               id IN (SELECT id FROM pds2.items WHERE amt > 2) AS in_sub,
               (SELECT tag FROM pds2.items i3 WHERE i3.id = items.id) AS scal,
               CAST(amt AS FLOAT64) AS f_amt,
               CAST(amt AS BIGNUMERIC) AS bn_amt,
               CAST(DATE '2026-01-01' AS DATETIME) AS dt_cast,
               CAST(DATETIME '2026-01-01 01:02:03' AS TIME) AS tm_cast,
               CAST(b'abc' AS STRING) AS bs
             FROM pds2.items
             ORDER BY id",
        )
        .unwrap();
    assert_eq!(sub.rows[0][1], BqValue::Bool(true));
    assert_eq!(sub.rows[0][2], BqValue::Bool(false));
    assert_eq!(sub.rows[1][2], BqValue::Bool(true));
    assert_eq!(sub.rows[0][8], BqValue::String("abc".to_owned()));

    let z = eng
        .query(
            "SELECT a.id, a.x AND TRUE, a.d BETWEEN DATE '2026-01-01' AND DATE '2026-01-02',
                    CAST(a.id AS STRING) LIKE '1%', a.id IN (1, 2), COALESCE(a.ts, CURRENT_TIMESTAMP())
             FROM pds2.empty_items a LEFT JOIN pds2.items b ON a.id = b.id",
        )
        .unwrap();
    assert_eq!(z.rows.len(), 0);
    assert_eq!(z.columns[1].ty, BqType::Bool);
    assert_eq!(z.columns[2].ty, BqType::Bool);
    assert_eq!(z.columns[5].ty, BqType::Timestamp);
}

#[cfg(feature = "persistence")]
#[test]
fn milestone_k_persistence_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.redb");
    let mut cache = bq_lite_core::storage::persist::PersistCache::default();
    {
        let mut eng = Engine::open_persistent(&db_path, "local").unwrap();
        eng.exec_script(
            "CREATE SCHEMA pds2;
             CREATE TABLE pds2.items (id INT64, tag STRING, amt NUMERIC);
             INSERT INTO pds2.items VALUES (1, 'a', NUMERIC '1.25'), (2, 'b', NUMERIC '2.75');
             CREATE VIEW pds2.v_items AS SELECT id, tag FROM pds2.items;",
        )
        .unwrap();
        eng.save_persistent_incremental(&db_path, &mut cache)
            .unwrap();
        eng.exec_script(
            "TRUNCATE TABLE pds2.items; CREATE TABLE pds2.temp_t (k INT64); DROP TABLE pds2.temp_t;",
        )
        .unwrap();
        eng.save_persistent_incremental(&db_path, &mut cache)
            .unwrap();
    }
    let reopened = Engine::open_persistent(&db_path, "local").unwrap();
    let after = reopened.query("SELECT COUNT(*) FROM pds2.items").unwrap();
    assert_eq!(after.rows[0][0], BqValue::Int64(0));
    assert!(reopened.query("SELECT * FROM pds2.temp_t").is_err());
    assert!(reopened.query("SELECT * FROM pds2.v_items").is_ok());
}

#[test]
fn milestone_k_scalar_null_edge_and_error_branches() {
    let eng = Engine::new();
    let nulls = eng
        .query(
            "SELECT
               ARRAY_LENGTH(CAST(NULL AS ARRAY<INT64>)),
               ARRAY_TO_STRING(CAST(NULL AS ARRAY<STRING>), ','),
               ARRAY_CONCAT(CAST(NULL AS ARRAY<INT64>), [1]),
               ARRAY_REVERSE(CAST(NULL AS ARRAY<INT64>)),
               ARRAY_FIRST(CAST(NULL AS ARRAY<INT64>)),
               ARRAY_LAST(CAST(NULL AS ARRAY<INT64>)),
               ARRAY_SLICE(CAST(NULL AS ARRAY<INT64>), 0, 1),
               GENERATE_ARRAY(CAST(NULL AS INT64), 5, 1),
               GENERATE_DATE_ARRAY(CAST(NULL AS DATE), DATE '2026-01-02'),
               TO_BASE64(CAST(NULL AS BYTES)),
               FROM_BASE64(CAST(NULL AS STRING)),
               TO_HEX(CAST(NULL AS BYTES)),
               FROM_HEX(CAST(NULL AS STRING)),
               FORMAT(CAST(NULL AS STRING), 1),
               PARSE_JSON(CAST(NULL AS STRING)),
               JSON_QUERY(CAST(NULL AS JSON), '$.a'),
               JSON_VALUE(CAST(NULL AS JSON), '$.a'),
               JSON_TYPE(CAST(NULL AS JSON)),
               JSON_KEYS(CAST(NULL AS JSON)),
               JSON_SET(CAST(NULL AS JSON), '$.a', 1),
               DATE_TRUNC(CAST(NULL AS DATE), MONTH),
               DATETIME_TRUNC(CAST(NULL AS DATETIME), DAY),
               TIMESTAMP_TRUNC(CAST(NULL AS TIMESTAMP), DAY),
               LAST_DAY(CAST(NULL AS DATE), MONTH),
               RANGE_OVERLAPS(CAST(NULL AS RANGE<DATE>), RANGE<DATE> '[2026-01-01, 2026-01-02)'),
               RANGE_INTERSECT(CAST(NULL AS RANGE<DATE>), RANGE<DATE> '[2026-01-01, 2026-01-02)'),
               NET.HOST(CAST(NULL AS STRING)),
               NET.IP_FROM_STRING(CAST(NULL AS STRING)),
               ST_GEOGPOINT(CAST(NULL AS FLOAT64), 0.0),
               ST_DISTANCE(CAST(NULL AS GEOGRAPHY), ST_GEOGPOINT(0, 0)),
               ST_CONTAINS(CAST(NULL AS GEOGRAPHY), ST_GEOGPOINT(0, 0)),
               REGEXP_CONTAINS(CAST(NULL AS STRING), 'a'),
               REGEXP_EXTRACT(CAST(NULL AS STRING), 'a'),
               REGEXP_REPLACE(CAST(NULL AS STRING), 'a', 'b'),
               SPLIT(CAST(NULL AS STRING), ','),
               REPLACE(CAST(NULL AS STRING), 'a', 'b'),
               SUBSTR(CAST(NULL AS STRING), 1, 2)",
        )
        .unwrap();
    for (i, v) in nulls.rows[0].iter().enumerate() {
        assert!(v.is_null(), "col {i} was {v:?}");
    }
    assert_eq!(
        eng.query("SELECT SEARCH(CAST(NULL AS STRING), 'a')")
            .unwrap()
            .rows[0][0],
        BqValue::Null
    );
    assert_eq!(
        eng.query("SELECT TO_JSON(CAST(NULL AS INT64)), TO_JSON_STRING(CAST(NULL AS INT64))")
            .unwrap()
            .rows[0][0]
            .to_string(),
        "null"
    );

    let bad_sqls = [
        "SELECT ARRAY_LENGTH(1)",
        "SELECT ARRAY_FIRST([])",
        "SELECT ARRAY_LAST([])",
        "SELECT GENERATE_ARRAY(1, 5, 0)",
        "SELECT TO_BASE64(1)",
        "SELECT FROM_BASE64(1)",
        "SELECT TO_HEX(1)",
        "SELECT FROM_HEX(1)",
        "SELECT FORMAT(1)",
        "SELECT FORMAT('%d')",
        "SELECT PARSE_JSON('{bad')",
        "SELECT JSON_KEYS(1)",
        "SELECT REGEXP_CONTAINS('a', '[invalid')",
        "SELECT NET.IP_FROM_STRING('999.999.999.999')",
        "SELECT ST_GEOGFROMTEXT(123)",
        "SELECT ERROR('boom')",
    ];
    for sql in bad_sqls {
        assert!(eng.query(sql).is_err(), "expected error for: {sql}");
    }
}

#[test]
fn milestone_k_ops_coercion_and_empty_schema_paths() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kops;
         CREATE TABLE kops.e (i INT64, f FLOAT64, n NUMERIC, b BOOL, s STRING, d DATE, dt DATETIME, t TIME, ts TIMESTAMP, iv INTERVAL);",
    )
    .unwrap();

    let ops = eng
        .query(
            "SELECT
               1 + 2, 5 - 3, 2 * 4, 9 / 2, MOD(10, 3),
               (1 << 3), (8 >> 2), (5 & 3), (5 | 2), (5 ^ 3), (~0),
               (1 IS DISTINCT FROM 2), (1 IS NOT DISTINCT FROM NULL),
               (TRUE IS TRUE), (FALSE IS NOT TRUE), (FALSE IS FALSE), (TRUE IS NOT FALSE),
               (NULL IS UNKNOWN), (TRUE IS NOT UNKNOWN),
               DATE '2026-01-01' + INTERVAL 1 MONTH,
               DATE '2026-01-10' - INTERVAL 5 DAY,
               TIMESTAMP '2026-01-01 00:00:00 UTC' + INTERVAL 1 HOUR,
               TIMESTAMP '2026-01-01 01:00:00 UTC' - INTERVAL 1 HOUR,
               NUMERIC '10.5' + NUMERIC '2.5', NUMERIC '10.5' - NUMERIC '2.5',
               NUMERIC '2.5' * NUMERIC '4.0', NUMERIC '10.0' / NUMERIC '4.0',
               SAFE_CAST('not_int' AS INT64), SAFE_CAST('not_num' AS NUMERIC),
               SAFE_CAST('not_date' AS DATE), SAFE_CAST('not_ts' AS TIMESTAMP)",
        )
        .unwrap();
    assert_eq!(ops.rows[0][0], BqValue::Int64(3));
    assert_eq!(ops.rows[0][5], BqValue::Int64(8));
    assert_eq!(ops.rows[0][11], BqValue::Bool(true));
    assert_eq!(ops.rows[0][12], BqValue::Bool(false));
    assert!(ops.rows[0][27].is_null());
    assert!(ops.rows[0][28].is_null());

    // Zero-row schema inference over UNION, INTERSECT, PIVOT, WINDOW, QUALIFY
    let z = eng
        .query(
            "SELECT i, f, n, b, s, d, dt, t, ts, iv,
                    ROW_NUMBER() OVER (PARTITION BY i ORDER BY d) AS rn,
                    SUM(i) OVER () AS tot
             FROM kops.e
             QUALIFY rn = 1",
        )
        .unwrap();
    assert_eq!(z.rows.len(), 0);
    assert_eq!(z.columns[10].ty, BqType::Int64);
}

#[test]
fn milestone_k_ddl_script_agg_builtins_and_coercions() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kfull;
         CREATE TABLE kfull.part (id INT64, d DATE, s STRING)
           PARTITION BY RANGE_BUCKET(id, GENERATE_ARRAY(0, 100, 10))
           CLUSTER BY s
           OPTIONS (description = 'test');
         ALTER TABLE kfull.part ADD COLUMN extra INT64 DEFAULT 5;
         ALTER TABLE kfull.part ALTER COLUMN extra SET OPTIONS (description = 'col');
         ALTER TABLE kfull.part RENAME COLUMN extra TO extra2;
         ALTER TABLE kfull.part DROP COLUMN extra2;
         INSERT INTO kfull.part VALUES (1, DATE '2026-01-01', 'a'), (2, DATE '2026-01-02', 'b'), (2, DATE '2026-01-03', NULL);
         CREATE SNAPSHOT TABLE kfull.part_snap CLONE kfull.part;
         CREATE OR REPLACE TABLE kfull.part_clone CLONE kfull.part;
         DROP SNAPSHOT TABLE kfull.part_snap;",
    )
    .unwrap();

    let aggs = eng
        .query(
            "SELECT
               COUNTIF(id > 1), LOGICAL_AND(id > 0), LOGICAL_OR(id = 2),
               BIT_AND(id), BIT_OR(id), BIT_XOR(id),
               STDDEV_POP(id), STDDEV_SAMP(id), VAR_POP(id), VAR_SAMP(id), VARIANCE(id),
               ARRAY_AGG(s IGNORE NULLS ORDER BY s DESC LIMIT 1),
               STRING_AGG(DISTINCT s, ',' ORDER BY s)
             FROM kfull.part",
        )
        .unwrap();
    assert_eq!(aggs.rows[0][0], BqValue::Int64(2));
    assert_eq!(aggs.rows[0][1], BqValue::Bool(true));

    let script_qr = eng
        .exec_script(
            "DECLARE i INT64 DEFAULT 0;
             DECLARE acc INT64 DEFAULT 0;
             DECLARE err_msg STRING DEFAULT '';
             LOOP
               SET i = i + 1;
               IF i = 2 THEN ITERATE; END IF;
               SET acc = acc + i;
               IF i >= 4 THEN LEAVE; END IF;
             END LOOP;
             CASE acc
               WHEN 8 THEN SET acc = acc + 1;
               ELSE SET acc = 0;
             END CASE;
             BEGIN
               RAISE USING MESSAGE = 'handled';
             EXCEPTION WHEN ERROR THEN
               SET err_msg = @@error.message;
             END;
             ASSERT acc = 9 AS 'acc check';
             SELECT acc, err_msg;",
        )
        .unwrap();
    assert_eq!(script_qr.rows[0][0], BqValue::Int64(9));
    assert!(script_qr.rows[0][1].to_string().contains("handled"));
}

#[test]
fn milestone_k_scalar_strings_math_and_casts() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               LPAD('a', 3, '0'), RPAD('a', 3, '0'), LTRIM('  x'), RTRIM('x  '),
               INITCAP('hello world'), REPEAT('ab', 2), REVERSE('abc'),
               CHAR_LENGTH('hé'), BYTE_LENGTH('hé'), TRANSLATE('abc', 'ab', 'xy'),
               LEFT('hello', 2), RIGHT('hello', 2), STARTS_WITH('hello', 'he'), ENDS_WITH('hello', 'lo'),
               STRPOS('hello', 'll'), INSTR('hello', 'l', 1, 2),
               LOG(100, 10), LOG10(100), LN(1), EXP(0), POW(2, 5),
               ROUND(3.456, 2), TRUNC(3.456, 1), SIGN(-5), FLOOR(2.9), CEIL(2.1),
               SQRT(9), CBRT(27), IEEE_DIVIDE(1, 0), SAFE_DIVIDE(1, 0),
               SAFE_MULTIPLY(2, 3), SAFE_ADD(2, 3), SAFE_SUBTRACT(5, 3),
               GREATEST(1, 5, 3), LEAST(1, 5, 3),
               MD5('hi'), SHA1('hi'), SHA256('hi'), SHA512('hi'), FARM_FINGERPRINT('hi'),
               CAST(TRUE AS INT64), CAST(1 AS BOOL), CAST('true' AS BOOL),
               CAST(1.5 AS INT64), CAST('2026-01-01' AS DATE), CAST('12:30:00' AS TIME),
               CAST('2026-01-01 12:30:00' AS DATETIME), CAST(TIME '12:30:00' AS STRING)",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::String("00a".to_owned()));
    assert_eq!(qr.rows[0][1], BqValue::String("a00".to_owned()));
    assert_eq!(qr.rows[0][6], BqValue::String("cba".to_owned()));
    assert!(qr.rows[0][29].is_null());
    assert_eq!(qr.rows[0][33], BqValue::Int64(5));
    assert_eq!(qr.rows[0][34], BqValue::Int64(1));
    assert_eq!(qr.rows[0][40], BqValue::Int64(1));
    assert_eq!(qr.rows[0][41], BqValue::Bool(true));
}

#[test]
fn milestone_k_datetime_extract_unix_range_net_and_subscripts() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               DATE_ADD(DATE '2026-01-01', INTERVAL 10 DAY),
               DATE_SUB(DATE '2026-01-11', INTERVAL 10 DAY),
               DATETIME_ADD(DATETIME '2026-01-01 00:00:00', INTERVAL 2 HOUR),
               DATETIME_SUB(DATETIME '2026-01-01 02:00:00', INTERVAL 2 HOUR),
               TIMESTAMP_ADD(TIMESTAMP '2026-01-01 00:00:00 UTC', INTERVAL 30 MINUTE),
               TIMESTAMP_SUB(TIMESTAMP '2026-01-01 00:30:00 UTC', INTERVAL 30 MINUTE),
               TIMESTAMP_SECONDS(0), TIMESTAMP_MILLIS(1000), TIMESTAMP_MICROS(1000000),
               UNIX_SECONDS(TIMESTAMP '1970-01-01 00:00:10 UTC'),
               UNIX_MILLIS(TIMESTAMP '1970-01-01 00:00:01 UTC'),
               UNIX_MICROS(TIMESTAMP '1970-01-01 00:00:01 UTC'),
               UNIX_DATE(DATE '1970-01-02'), DATE_FROM_UNIX_DATE(1),
               FORMAT_DATE('%Y/%m/%d', DATE '2026-03-15'),
               PARSE_DATE('%Y/%m/%d', '2026/03/15'),
               FORMAT_TIMESTAMP('%Y-%m-%d', TIMESTAMP '2026-03-15 00:00:00 UTC'),
               PARSE_TIMESTAMP('%Y-%m-%d %H:%M:%S', '2026-03-15 00:00:00'),
               EXTRACT(MONTH FROM DATE '2026-05-01'),
               EXTRACT(DAYOFWEEK FROM DATE '2026-05-01'),
               EXTRACT(DAYOFYEAR FROM DATE '2026-02-01'),
               EXTRACT(HOUR FROM TIMESTAMP '2026-05-01 14:05:06 UTC'),
               EXTRACT(MINUTE FROM TIMESTAMP '2026-05-01 14:05:06 UTC'),
               EXTRACT(SECOND FROM TIMESTAMP '2026-05-01 14:05:06 UTC'),
               RANGE_START(RANGE(DATE '2026-01-01', DATE '2026-02-01')),
               RANGE_END(RANGE<DATE> '[2026-01-01, 2026-02-01)'),
               RANGE_CONTAINS(RANGE<DATE> '[2026-01-01, 2026-02-01)', DATE '2026-01-15'),
               NET.SAFE_IP_FROM_STRING('bad_ip'),
               NET.IP_TO_STRING(NET.IP_FROM_STRING('::1')),
               [10, 20, 30][OFFSET(1)], [10, 20, 30][ORDINAL(2)],
               [10, 20][SAFE_OFFSET(9)], [10, 20][SAFE_ORDINAL(9)],
               ('abc' LIKE 'a_c'), ('abc' NOT LIKE 'z%'),
               (5 NOT BETWEEN 1 AND 3), (5 NOT IN (1, 2, 3))",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0].to_string(), "2026-01-11");
    assert_eq!(qr.rows[0][9], BqValue::Int64(10));
    assert_eq!(qr.rows[0][12], BqValue::Int64(1));
    assert_eq!(qr.rows[0][18], BqValue::Int64(5));
    assert_eq!(qr.rows[0][26], BqValue::Bool(true));
    assert!(qr.rows[0][27].is_null());
    assert_eq!(qr.rows[0][28], BqValue::String("::1".to_owned()));
    assert_eq!(qr.rows[0][29], BqValue::Int64(20));
    assert_eq!(qr.rows[0][30], BqValue::Int64(20));
    assert!(qr.rows[0][31].is_null());
    assert!(qr.rows[0][32].is_null());
    assert_eq!(qr.rows[0][33], BqValue::Bool(true));
    assert_eq!(qr.rows[0][36], BqValue::Bool(true));
}

#[test]
fn milestone_k_zero_row_joins_unnest_pivot_and_json_paths() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kz;
         CREATE TABLE kz.a (id INT64, tag STRING, arr ARRAY<INT64>, rec STRUCT<k INT64, v STRING>);
         CREATE TABLE kz.b (id INT64, grp STRING, val INT64);",
    )
    .unwrap();

    let z1 = eng
        .query(
            "SELECT * EXCEPT(tag) REPLACE(id + 1 AS id)
             FROM kz.a
             FULL OUTER JOIN kz.b USING (id)
             ORDER BY id",
        )
        .unwrap();
    assert_eq!(z1.rows.len(), 0);

    let z2 = eng
        .query(
            "SELECT id, x, off
             FROM kz.a, UNNEST(arr) AS x WITH OFFSET AS off
             UNION DISTINCT
             SELECT id, val, 0 FROM kz.b",
        )
        .unwrap();
    assert_eq!(z2.rows.len(), 0);
    assert_eq!(z2.columns.len(), 3);

    let j = eng
        .query(
            "SELECT
               JSON_QUERY(JSON '{\"a\":[10,20],\"b\":{\"c\":\"hi\"}}', '$.a[1]'),
               JSON_VALUE(JSON '{\"a\":[10,20],\"b\":{\"c\":\"hi\"}}', '$.b.c'),
               JSON_VALUE(JSON '{\"a\":1}', '$.missing'),
               JSON_QUERY(JSON '{\"a\":1}', '$.missing'),
               TO_JSON(STRUCT(1 AS a, [2, 3] AS b, DATE '2026-01-01' AS d)),
               ARRAY_TRANSFORM([1, 2, 3], x -> x * 10),
               ARRAY_FILTER([1, 2, 3, 4], x -> x > 2),
               ARRAY_CONCAT([1], [2, 3]),
               ARRAY_TO_STRING(['a', NULL, 'b'], '-', 'N/A'),
               IF(TRUE, 10, 20), IFNULL(NULL, 5), NULLIF(5, 5), COALESCE(NULL, NULL, 9)",
        )
        .unwrap();
    assert_eq!(j.rows[0][0].to_string(), "20");
    assert_eq!(j.rows[0][1], BqValue::String("hi".to_owned()));
    assert!(j.rows[0][2].is_null());
    assert!(j.rows[0][3].is_null());
    assert_eq!(j.rows[0][8], BqValue::String("a-N/A-b".to_owned()));
    assert_eq!(j.rows[0][9], BqValue::Int64(10));
    assert_eq!(j.rows[0][10], BqValue::Int64(5));
    assert!(j.rows[0][11].is_null());
    assert_eq!(j.rows[0][12], BqValue::Int64(9));
}

#[test]
fn milestone_k_coercion_trig_math_and_datetime_ctors() {
    let eng = Engine::new();
    let qr = eng
        .query(
            "SELECT
               ABS(-5), DIV(10, 3), POWER(2, 4), CEILING(2.1), SAFE_NEGATE(5),
               RANGE_BUCKET(15, [0, 10, 20, 30]), RAND() >= 0.0,
               SAFE.SQRT(-1.0), SAFE.LN(0.0), SAFE.DIV(1, 0),
               CURRENT_DATE(), CURRENT_DATETIME(), CURRENT_TIME(), CURRENT_TIMESTAMP(),
               DATE(2026, 3, 15), DATETIME(DATE '2026-03-15', TIME '10:20:30'), TIME(10, 20, 30),
               FORMAT_DATETIME('%Y-%m-%d %H:%M', DATETIME '2026-03-15 10:20:30'),
               PARSE_DATETIME('%Y-%m-%d %H:%M:%S', '2026-03-15 10:20:30'),
               FORMAT_TIME('%H:%M', TIME '10:20:30'),
               PARSE_TIME('%H:%M:%S', '10:20:30'),
               TIMESTAMP('2026-03-15 10:20:30 UTC'),
               CAST(1.5 AS NUMERIC), CAST(BIGNUMERIC '2.5' AS NUMERIC),
               CAST(1.5 AS BIGNUMERIC), CAST(TIMESTAMP '2026-01-01 12:30:00 UTC' AS TIME),
               CAST(DATE '2026-01-01' AS TIMESTAMP), CAST(DATETIME '2026-01-01 12:00:00' AS TIMESTAMP),
               CAST('{\"a\":1}' AS JSON), CAST([1, 2] AS ARRAY<FLOAT64>),
               SAFE_CAST('NaN' AS INT64), SAFE_CAST(CAST('NaN' AS FLOAT64) AS JSON)",
        )
        .unwrap();
    assert_eq!(qr.rows[0][0], BqValue::Int64(5));
    assert_eq!(qr.rows[0][1], BqValue::Int64(3));
    assert_eq!(qr.rows[0][5], BqValue::Int64(2));
    assert_eq!(qr.rows[0][6], BqValue::Bool(true));
    assert!(qr.rows[0][7].is_null());
    assert!(qr.rows[0][8].is_null());
    assert!(qr.rows[0][9].is_null());
    assert!(qr.rows[0][30].is_null());
    assert!(qr.rows[0][31].is_null());

    let bad_casts = [
        "SELECT CAST(TRUE AS DATE)",
        "SELECT CAST(TRUE AS FLOAT64)",
        "SELECT CAST(TRUE AS NUMERIC)",
        "SELECT CAST(TRUE AS BIGNUMERIC)",
        "SELECT CAST(TRUE AS BYTES)",
        "SELECT CAST(TRUE AS DATETIME)",
        "SELECT CAST(TRUE AS TIME)",
        "SELECT CAST(TRUE AS TIMESTAMP)",
        "SELECT CAST(CAST('NaN' AS FLOAT64) AS JSON)",
        "SELECT CAST(1 AS ARRAY<INT64>)",
    ];
    for sql in bad_casts {
        assert!(eng.query(sql).is_err(), "expected cast error for: {sql}");
    }
}

#[test]
fn milestone_k_zero_row_set_ops_pivot_and_governance_branches() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kgov;
         CREATE TABLE kgov.t (id INT64, dept STRING, secret STRING OPTIONS (policy_tag = 'pii'));
         CREATE ROW ACCESS POLICY p_eng ON kgov.t GRANT TO ('user:alice@example.com') FILTER USING (dept = 'eng');",
    )
    .unwrap();
    eng.set_enforce_column_masking(true);
    eng.set_masking_policy("pii", None);
    eng.set_session_user("user:alice@example.com");

    let z = eng
        .query("SELECT * FROM kgov.t INTERSECT DISTINCT SELECT * FROM kgov.t EXCEPT DISTINCT SELECT * FROM kgov.t")
        .unwrap();
    assert_eq!(z.rows.len(), 0);
    assert_eq!(z.columns.len(), 3);

    eng.exec("INSERT INTO kgov.t VALUES (1, 'eng', 'ssn-1'), (2, 'sales', 'ssn-2');")
        .unwrap();
    let masked = eng.query("SELECT id, secret FROM kgov.t").unwrap();
    assert_eq!(masked.rows.len(), 1);
    assert!(masked.rows[0][1].is_null());

    eng.set_masking_policy("pii", Some(BqValue::String("***".to_owned())));
    let redacted = eng.query("SELECT secret FROM kgov.t").unwrap();
    assert_eq!(redacted.rows[0][0], BqValue::String("***".to_owned()));
}

#[test]
fn milestone_k_extra_datetime_json_and_zero_row_pivot_coverage() {
    let mut eng = Engine::new();
    eng.exec_script(
        "CREATE SCHEMA kex;
         CREATE TABLE kex.sales (yr INT64, qtr STRING, amt INT64);
         CREATE TABLE kex.wide (id INT64, q1 INT64, q2 INT64);",
    )
    .unwrap();

    // Zero-row PIVOT + UNPIVOT schema inference
    let zp = eng
        .query("SELECT * FROM (SELECT yr, qtr, amt FROM kex.sales) PIVOT(SUM(amt) FOR qtr IN ('Q1', 'Q2'))")
        .unwrap();
    assert_eq!(zp.rows.len(), 0);
    let zu = eng
        .query("SELECT * FROM kex.wide UNPIVOT(val FOR qtr IN (q1, q2))")
        .unwrap();
    assert_eq!(zu.rows.len(), 0);

    // Date/Timestamp trunc & diff parts + Date/Interval math + JSON edge errors
    let dt = eng
        .query(
            "SELECT
               DATE_TRUNC(DATE '2026-05-15', YEAR), DATE_TRUNC(DATE '2026-05-15', QUARTER),
               DATETIME_TRUNC(DATETIME '2026-05-15 10:20:30', YEAR),
               DATETIME_TRUNC(DATETIME '2026-05-15 10:20:30', MONTH),
               DATETIME_TRUNC(DATETIME '2026-05-15 10:20:30', DAY),
               DATETIME_TRUNC(DATETIME '2026-05-15 10:20:30', MINUTE),
               TIMESTAMP_TRUNC(TIMESTAMP '2026-05-15 10:20:30 UTC', YEAR),
               TIMESTAMP_TRUNC(TIMESTAMP '2026-05-15 10:20:30 UTC', MONTH),
               TIMESTAMP_TRUNC(TIMESTAMP '2026-05-15 10:20:30 UTC', HOUR),
               TIMESTAMP_TRUNC(TIMESTAMP '2026-05-15 10:20:30 UTC', MINUTE),
               DATE_DIFF(DATE '2026-05-15', DATE '2025-01-10', YEAR),
               DATE_DIFF(DATE '2026-05-15', DATE '2025-01-10', MONTH),
               TIMESTAMP_DIFF(TIMESTAMP '2026-05-15 12:00:00 UTC', TIMESTAMP '2026-05-15 10:00:00 UTC', HOUR),
               TIMESTAMP_DIFF(TIMESTAMP '2026-05-15 12:00:00 UTC', TIMESTAMP '2026-05-15 11:59:00 UTC', SECOND),
               JSON_EXTRACT_STRING_ARRAY(JSON '[\"a\", \"b\"]'),
               JSON_EXTRACT_ARRAY(JSON '[1, 2]'),
               JSON_TYPE(JSON 'true'), JSON_TYPE(JSON '12'), JSON_TYPE(JSON '\"s\"'), JSON_TYPE(JSON '{}')",
        )
        .unwrap();
    assert_eq!(dt.rows[0][0].to_string(), "2026-01-01");
    assert_eq!(dt.rows[0][1].to_string(), "2026-04-01");
    assert_eq!(dt.rows[0][10], BqValue::Int64(1));
    assert_eq!(dt.rows[0][12], BqValue::Int64(2));
    assert_eq!(dt.rows[0][13], BqValue::Int64(60));
    assert_eq!(dt.rows[0][16], BqValue::String("boolean".to_owned()));
    assert_eq!(dt.rows[0][17], BqValue::String("number".to_owned()));
    assert_eq!(dt.rows[0][18], BqValue::String("string".to_owned()));
    assert_eq!(dt.rows[0][19], BqValue::String("object".to_owned()));
}
