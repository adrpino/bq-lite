use bq_lite_core::{BqErrorKind, BqValue, Engine};

#[test]
fn string_and_regex_functions_match_bigquery() {
    let engine = Engine::new();
    let res = engine
        .query(
            "SELECT CONCAT('a', 'b'), SUBSTR('abcdef', 2, 3), LENGTH('héllo'), BYTE_LENGTH('héllo'), STRPOS('banana', 'na'), INITCAP('hello WORLD'), LPAD('7', 3, '0'), REGEXP_EXTRACT('user-42', r'-(\\d+)'), REGEXP_EXTRACT_ALL('a1 b22', r'\\d+'), REGEXP_REPLACE('foo 123', r'\\d+', 'X')",
        )
        .unwrap();
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::String("ab".to_owned()),
            BqValue::String("bcd".to_owned()),
            BqValue::Int64(5),
            BqValue::Int64(6),
            BqValue::Int64(3),
            BqValue::String("Hello World".to_owned()),
            BqValue::String("007".to_owned()),
            BqValue::String("42".to_owned()),
            BqValue::Array(vec![
                BqValue::String("1".to_owned()),
                BqValue::String("22".to_owned()),
            ]),
            BqValue::String("foo X".to_owned()),
        ]
    );
}

#[test]
fn datetime_interval_and_timezone_functions() {
    let engine = Engine::new();
    let res = engine
        .query(
            "SELECT DATE_ADD(DATE '2026-01-15', INTERVAL 1 MONTH), DATE '2026-03-10' - INTERVAL 10 DAY, DATE_DIFF(DATE '2026-02-01', DATE '2026-01-01', 'DAY'), LAST_DAY(DATE '2024-02-10'), FORMAT_DATE('%Y/%m/%d', PARSE_DATE('%Y-%m-%d', '2026-05-09')), SAFE.PARSE_DATE('%Y-%m-%d', 'bad'), ARRAY_LENGTH(GENERATE_DATE_ARRAY(DATE '2026-01-01', DATE '2026-01-05'))",
        )
        .unwrap();
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::Date(chrono::NaiveDate::from_ymd_opt(2026, 2, 15).unwrap()),
            BqValue::Date(chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()),
            BqValue::Int64(31),
            BqValue::Date(chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
            BqValue::String("2026/05/09".to_owned()),
            BqValue::Null,
            BqValue::Int64(5),
        ]
    );
}

#[test]
fn math_hash_and_farm_fingerprint_golden_values() {
    let engine = Engine::new();
    let res = engine
        .query(
            "SELECT ABS(-42), ROUND(2.71828, 2), MOD(10, 3), DIV(10, 3), GREATEST(1, 5, 3), RANGE_BUCKET(25, [0, 10, 20, 30]), TO_HEX(SHA256('abc')), FARM_FINGERPRINT(''), FARM_FINGERPRINT('hello'), SAFE.ERROR('ignored')",
        )
        .unwrap();
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::Int64(42),
            BqValue::Float64(2.72),
            BqValue::Int64(1),
            BqValue::Int64(3),
            BqValue::Int64(5),
            BqValue::Int64(3),
            BqValue::String(
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_owned()
            ),
            BqValue::Int64(-7_286_425_919_675_154_353),
            BqValue::Int64(-5_436_999_610_281_751_320),
            BqValue::Null,
        ]
    );

    let err = engine.query("SELECT ERROR('boom')").unwrap_err();
    assert_eq!(err.kind, BqErrorKind::Semantic);
}

#[test]
fn json_and_array_functions() {
    let engine = Engine::new();
    let res = engine
        .query(
            "SELECT JSON_VALUE(PARSE_JSON('{\"user\":{\"name\":\"ada\"}}'), '$.user.name'), INT64(PARSE_JSON('{\"n\":42}').n), PARSE_JSON('{\"arr\":[10,20]}').arr[1], JSON_TYPE(PARSE_JSON('[1,2]')), ARRAY_TO_STRING(ARRAY_REVERSE([1, 2, 3]), '-'), GENERATE_ARRAY(1, 7, 2), ARRAY_SLICE([10, 20, 30, 40], 1, 2)",
        )
        .unwrap();
    assert_eq!(
        res.rows[0],
        vec![
            BqValue::String("ada".to_owned()),
            BqValue::Int64(42),
            BqValue::Json(Box::new(serde_json::json!(20))),
            BqValue::String("array".to_owned()),
            BqValue::String("3-2-1".to_owned()),
            BqValue::Array(vec![
                BqValue::Int64(1),
                BqValue::Int64(3),
                BqValue::Int64(5),
                BqValue::Int64(7),
            ]),
            BqValue::Array(vec![BqValue::Int64(20), BqValue::Int64(30)]),
        ]
    );
}
