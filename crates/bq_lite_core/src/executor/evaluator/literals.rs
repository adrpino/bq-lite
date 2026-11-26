use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::ast::Literal;
use crate::error::{BqError, BqResult};
use crate::types::{BqBigNumeric, BqNumeric, BqValue};

pub(super) fn eval_literal(lit: &Literal) -> BqResult<BqValue> {
    match lit {
        Literal::Null => Ok(BqValue::Null),
        Literal::Bool(b) => Ok(BqValue::Bool(*b)),
        Literal::Int(n) => Ok(BqValue::Int64(*n)),
        Literal::Float(f) => Ok(BqValue::Float64(*f)),
        Literal::Numeric(s) => s.parse::<BqNumeric>().map(BqValue::Numeric),
        Literal::BigNumeric(s) => s
            .parse::<BqBigNumeric>()
            .map(|v| BqValue::BigNumeric(Box::new(v))),
        Literal::String(s) => Ok(BqValue::String(s.clone())),
        Literal::Bytes(b) => Ok(BqValue::Bytes(b.clone())),
        Literal::Date(s) => NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map(BqValue::Date)
            .map_err(|_| BqError::query(format!("Invalid DATE literal: '{s}'"))),
        Literal::Datetime(s) => parse_datetime_lit(s).map(BqValue::Datetime),
        Literal::Time(s) => NaiveTime::parse_from_str(s.trim(), "%H:%M:%S%.f")
            .or_else(|_| NaiveTime::parse_from_str(s.trim(), "%H:%M:%S"))
            .map(BqValue::Time)
            .map_err(|_| BqError::query(format!("Invalid TIME literal: '{s}'"))),
        Literal::Timestamp(s) => parse_timestamp_lit(s).map(BqValue::Timestamp),
        Literal::Json(s) => serde_json::from_str::<serde_json::Value>(s)
            .map(|v| BqValue::Json(Box::new(v)))
            .map_err(|e| BqError::query(format!("Invalid JSON literal: {e}"))),
    }
}

fn parse_datetime_lit(s: &str) -> BqResult<NaiveDateTime> {
    let trimmed = s.trim();
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(dt);
        }
    }
    Err(BqError::query(format!("Invalid DATETIME literal: '{s}'")))
}

fn parse_timestamp_lit(s: &str) -> BqResult<DateTime<Utc>> {
    let trimmed = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Some(without_utc) = trimmed.strip_suffix(" UTC") {
        if let Ok(dt) = parse_datetime_lit(without_utc) {
            return Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
        }
    }
    if let Ok(dt) = parse_datetime_lit(trimmed) {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    }
    Err(BqError::query(format!("Invalid TIMESTAMP literal: '{s}'")))
}
