use bq_lite_core::parser::{parse_expr, parse_query, parse_script};
use bq_lite_core::types::coercion::{can_implicitly_coerce, cast_value, common_supertype};
use bq_lite_core::{
    BqBigNumeric, BqError, BqErrorKind, BqInterval, BqNumeric, BqRange, BqStructFieldValue, BqType,
    BqValue, RangeElementType, SourceSpan, StructField,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

#[test]
fn covers_all_bq_type_formatting_and_rest_names() {
    let tys = vec![
        BqType::UntypedNull,
        BqType::Bool,
        BqType::Int64,
        BqType::Float64,
        BqType::Numeric,
        BqType::BigNumeric,
        BqType::String,
        BqType::Bytes,
        BqType::Date,
        BqType::Datetime,
        BqType::Time,
        BqType::Timestamp,
        BqType::Interval,
        BqType::Json,
        BqType::Geography,
        BqType::Range(RangeElementType::Date),
        BqType::Range(RangeElementType::Datetime),
        BqType::Range(RangeElementType::Timestamp),
        BqType::array(BqType::Int64).unwrap(),
        BqType::structure(vec![
            StructField::named("x", BqType::Int64),
            StructField::anonymous(BqType::String),
        ]),
    ];
    for t in &tys {
        assert_ne!(t.to_string(), "");
        assert_ne!(t.rest_type_name(), "");
    }
    assert!(BqType::Int64.is_numeric());
    assert!(BqType::Float64.is_numeric());
    assert!(!BqType::String.is_numeric());
}

#[test]
fn covers_bq_value_display_and_infer_type() {
    let d = NaiveDate::from_ymd_opt(2024, 3, 14).unwrap();
    let t = NaiveTime::from_hms_micro_opt(9, 26, 53, 123_456).unwrap();
    let dt = NaiveDateTime::new(d, t);
    let ts = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
    let vals = vec![
        BqValue::Null,
        BqValue::Bool(true),
        BqValue::Int64(-7),
        BqValue::Float64(f64::NAN),
        BqValue::Float64(f64::INFINITY),
        BqValue::Float64(f64::NEG_INFINITY),
        BqValue::Float64(1.25),
        BqValue::Numeric("12.34".parse().unwrap()),
        BqValue::BigNumeric(Box::new("999.125".parse().unwrap())),
        BqValue::String("hi".to_owned()),
        BqValue::Bytes(vec![1, 2, 3]),
        BqValue::Date(d),
        BqValue::Datetime(dt),
        BqValue::Time(t),
        BqValue::Timestamp(ts),
        BqValue::Interval(BqInterval::new(14, 3, 1_500_000)),
        BqValue::Json(Box::new(serde_json::json!({"k": 1}))),
        BqValue::Geography("POINT(1 2)".to_owned()),
        BqValue::Range(Box::new(BqRange {
            elem_type: RangeElementType::Date,
            start: Some(BqValue::Date(d)),
            end: None,
        })),
        BqValue::Array(vec![]),
        BqValue::Struct(vec![BqStructFieldValue {
            name: None,
            value: BqValue::Int64(5),
        }]),
    ];
    for v in &vals {
        assert_ne!(v.to_string(), "");
        assert!(v.infer_type().is_ok());
    }
}

#[test]
fn covers_coercion_supertype_and_bool_int_casts() {
    assert!(can_implicitly_coerce(&BqType::Date, &BqType::Datetime));
    assert!(can_implicitly_coerce(
        &BqType::array(BqType::Int64).unwrap(),
        &BqType::array(BqType::Float64).unwrap()
    ));
    assert!(!can_implicitly_coerce(&BqType::String, &BqType::Int64));
    assert_eq!(
        common_supertype(&[BqType::Date, BqType::Datetime]).unwrap(),
        BqType::Datetime
    );
    assert!(common_supertype(&[BqType::String, BqType::Int64]).is_err());

    assert_eq!(
        cast_value(BqValue::String("true".to_owned()), &BqType::Bool).unwrap(),
        BqValue::Bool(true)
    );
    assert_eq!(
        cast_value(BqValue::String("FALSE".to_owned()), &BqType::Bool).unwrap(),
        BqValue::Bool(false)
    );
    assert!(cast_value(BqValue::String("bad".to_owned()), &BqType::Bool).is_err());
    assert!(cast_value(BqValue::Float64(f64::NAN), &BqType::Int64).is_err());
    assert!(cast_value(BqValue::Float64(1e30), &BqType::Int64).is_err());
}

#[test]
fn covers_cast_errors_and_datetime_array_casts() {
    for (bad, ty) in [
        ("bad", BqType::Float64),
        ("bad", BqType::Numeric),
        ("bad", BqType::Date),
        ("bad", BqType::Time),
        ("bad", BqType::Datetime),
        ("bad", BqType::Timestamp),
        ("{bad", BqType::Json),
    ] {
        assert!(cast_value(BqValue::String(bad.to_owned()), &ty).is_err());
    }
    assert!(cast_value(
        BqValue::String("2024-01-02 03:04:05+00:00".to_owned()),
        &BqType::Timestamp
    )
    .is_ok());
    assert!(cast_value(
        BqValue::String("2024-01-02 03:04:05".to_owned()),
        &BqType::Timestamp
    )
    .is_ok());
    assert_eq!(
        cast_value(
            BqValue::Array(vec![BqValue::Int64(1), BqValue::Int64(2)]),
            &BqType::array(BqType::String).unwrap()
        )
        .unwrap(),
        BqValue::Array(vec![
            BqValue::String("1".to_owned()),
            BqValue::String("2".to_owned()),
        ])
    );
}

#[test]
fn covers_numeric_bignumeric_and_error_kinds() {
    let a: BqNumeric = "10.50".parse().unwrap();
    let b: BqNumeric = "2.25".parse().unwrap();
    assert_eq!(a.checked_add(b).unwrap().to_string(), "12.75");
    assert_eq!(a.checked_sub(b).unwrap().to_string(), "8.25");
    assert_eq!(a.checked_mul(b).unwrap().to_string(), "23.625");
    assert!(a.checked_div(b).is_ok());
    assert!(a.checked_div("0".parse().unwrap()).is_err());
    assert_eq!(a.checked_neg().unwrap().to_string(), "-10.5");

    let ba: BqBigNumeric = "100.5".parse().unwrap();
    let bb: BqBigNumeric = "2".parse().unwrap();
    assert_eq!(ba.checked_add(&bb).unwrap().to_string(), "102.5");
    assert_eq!(ba.checked_sub(&bb).unwrap().to_string(), "98.5");
    assert_eq!(ba.checked_mul(&bb).unwrap().to_string(), "201");
    assert_eq!(ba.checked_div(&bb).unwrap().to_string(), "50.25");
    assert!(ba.checked_div(&"0".parse().unwrap()).is_err());

    for err in [
        BqError::syntax("s", SourceSpan::new(0, 1, 1, 1)),
        BqError::query("q"),
        BqError::semantic("sem"),
        BqError::not_found("nf"),
        BqError::already_exists("ae"),
        BqError::invalid_argument("ia"),
        BqError::permission_denied("pd"),
        BqError::unimplemented("un"),
        BqError::internal("int"),
    ] {
        assert_ne!(err.api_status(), "");
        assert_ne!(err.api_reason(), "");
        assert_eq!(
            err.is_safe_suppressible(),
            err.kind == BqErrorKind::Semantic
        );
    }
}

#[test]
fn covers_parser_types_and_string_literals() {
    let q = parse_query(
        "SELECT CAST(1 AS INT64), CAST(1 AS SMALLINT), CAST(1 AS DECIMAL(10, 2)), \
         CAST(1 AS BIGNUMERIC(20, 4)), CAST('x' AS STRING(10)), CAST(b'\\x41\\n' AS BYTES(4)), \
         r'''raw''s''', rb'bytes', \"double\\t\\\"quoted\\\"\" \
         FROM t",
    )
    .unwrap();
    assert!(matches!(q.body, _));

    assert!(parse_expr("CAST(NULL AS RANGE<DATE>)").is_ok());
    assert!(parse_expr("CAST(NULL AS RANGE<DATETIME>)").is_ok());
    assert!(parse_expr("CAST(NULL AS RANGE<TIMESTAMP>)").is_ok());
    assert!(parse_expr("CAST(NULL AS STRUCT<a INT64, b ARRAY<STRING>>)").is_ok());
    assert!(parse_script("CREATE TABLE t (a INT, b BOOLEAN, c FLOAT64, d TIME, e DATETIME, f JSON, g GEOGRAPHY, h INTERVAL);").is_ok());
    assert!(parse_expr("r'''unterminated").is_err());
    assert!(parse_expr("b'unterminated").is_err());
}

#[test]
fn covers_numeric_and_bytes_scalar_casts() {
    assert_eq!(
        cast_value(BqValue::Int64(42), &BqType::Numeric).unwrap(),
        BqValue::Numeric("42".parse().unwrap())
    );
    assert!(cast_value(BqValue::Float64(1.25), &BqType::Numeric).is_ok());
    assert!(cast_value(BqValue::Float64(1.25), &BqType::BigNumeric).is_ok());
    assert_eq!(
        cast_value(BqValue::Int64(5), &BqType::BigNumeric).unwrap(),
        BqValue::BigNumeric(Box::new(BqBigNumeric::from_i64(5)))
    );
    assert_eq!(
        cast_value(BqValue::Numeric("5".parse().unwrap()), &BqType::BigNumeric).unwrap(),
        BqValue::BigNumeric(Box::new(BqBigNumeric::from_i64(5)))
    );
    assert_eq!(
        cast_value(BqValue::String("abc".to_owned()), &BqType::Bytes).unwrap(),
        BqValue::Bytes(b"abc".to_vec())
    );
    assert!(cast_value(BqValue::Bool(true), &BqType::Int64).is_ok());
    assert!(cast_value(BqValue::Int64(1), &BqType::Bool).is_ok());
    assert!(cast_value(BqValue::Numeric("1.5".parse().unwrap()), &BqType::Float64).is_ok());
    assert!(cast_value(
        BqValue::BigNumeric(Box::new("1.5".parse().unwrap())),
        &BqType::Float64
    )
    .is_ok());
    assert!(BqNumeric::from_f64(f64::NAN).is_err());
    assert!(BqBigNumeric::from_f64(f64::INFINITY).is_err());
}

#[test]
fn covers_temporal_cross_casts() {
    let d = NaiveDate::from_ymd_opt(2024, 5, 19).unwrap();
    let t = NaiveTime::from_hms_opt(10, 20, 30).unwrap();
    let dt = NaiveDateTime::new(d, t);
    let ts = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);

    assert_eq!(
        cast_value(BqValue::Datetime(dt), &BqType::Date).unwrap(),
        BqValue::Date(d)
    );
    assert_eq!(
        cast_value(BqValue::Timestamp(ts), &BqType::Date).unwrap(),
        BqValue::Date(d)
    );
    assert_eq!(
        cast_value(BqValue::Date(d), &BqType::Datetime).unwrap(),
        BqValue::Datetime(NaiveDateTime::new(d, NaiveTime::default()))
    );
    assert_eq!(
        cast_value(BqValue::Timestamp(ts), &BqType::Datetime).unwrap(),
        BqValue::Datetime(dt)
    );
    assert_eq!(
        cast_value(BqValue::Datetime(dt), &BqType::Time).unwrap(),
        BqValue::Time(t)
    );
    assert_eq!(
        cast_value(BqValue::Timestamp(ts), &BqType::Time).unwrap(),
        BqValue::Time(t)
    );
    assert_eq!(
        cast_value(BqValue::Datetime(dt), &BqType::Timestamp).unwrap(),
        BqValue::Timestamp(ts)
    );
    assert!(cast_value(BqValue::Date(d), &BqType::Timestamp).is_ok());
}
