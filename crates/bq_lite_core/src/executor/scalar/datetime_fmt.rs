use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub(super) fn eval_format_dt(name: &str, args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query(format!("{name} expects 2 or 3 arguments")));
    }
    let mut it = args.into_iter();
    let fmt_v = it.next().unwrap_or(BqValue::Null);
    let val = it.next().unwrap_or(BqValue::Null);
    if fmt_v.is_null() || val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(fmt) = fmt_v else {
        return Err(BqError::query(format!("{name} format must be STRING")));
    };
    let formatted = match (name, val) {
        ("FORMAT_DATE", BqValue::Date(d)) => d.format(&fmt).to_string(),
        ("FORMAT_DATETIME", BqValue::Datetime(dt)) => dt.format(&fmt).to_string(),
        ("FORMAT_TIME", BqValue::Time(t)) => t.format(&fmt).to_string(),
        ("FORMAT_TIMESTAMP", BqValue::Timestamp(ts)) => ts.format(&fmt).to_string(),
        _ => return Err(BqError::query(format!("Invalid argument type for {name}"))),
    };
    Ok(BqValue::String(formatted))
}

pub(super) fn eval_parse_dt(name: &str, args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query(format!("{name} expects 2 or 3 arguments")));
    }
    let mut it = args.into_iter();
    let fmt_v = it.next().unwrap_or(BqValue::Null);
    let str_v = it.next().unwrap_or(BqValue::Null);
    if fmt_v.is_null() || str_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(fmt), BqValue::String(s)) = (fmt_v, str_v) else {
        return Err(BqError::query(format!("{name} expects STRING arguments")));
    };
    match name {
        "PARSE_DATE" => NaiveDate::parse_from_str(&s, &fmt)
            .map(BqValue::Date)
            .map_err(|e| BqError::semantic(format!("Failed to parse DATE: {e}"))),
        "PARSE_DATETIME" => NaiveDateTime::parse_from_str(&s, &fmt)
            .map(BqValue::Datetime)
            .map_err(|e| BqError::semantic(format!("Failed to parse DATETIME: {e}"))),
        "PARSE_TIME" => NaiveTime::parse_from_str(&s, &fmt)
            .map(BqValue::Time)
            .map_err(|e| BqError::semantic(format!("Failed to parse TIME: {e}"))),
        "PARSE_TIMESTAMP" => DateTime::parse_from_str(&s, &fmt)
            .map(|dt| BqValue::Timestamp(dt.with_timezone(&Utc)))
            .or_else(|_| {
                NaiveDateTime::parse_from_str(&s, &fmt).map(|dt| {
                    BqValue::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
                })
            })
            .map_err(|e| BqError::semantic(format!("Failed to parse TIMESTAMP: {e}"))),
        _ => Err(BqError::internal("unexpected parse fn")),
    }
}
