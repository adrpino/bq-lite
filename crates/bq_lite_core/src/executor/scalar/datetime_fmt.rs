use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::error::{BqError, BqResult};
use crate::types::{validate_bq_date, BqInterval, BqTimezone, BqValue};

use super::datetime_calc::{add_interval_to_date, coerce_date_arg};

pub(super) fn eval_format_dt(name: &str, args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query(format!("{name} expects 2 or 3 arguments")));
    }
    let mut it = args.into_iter();
    let fmt_v = it.next().unwrap_or(BqValue::Null);
    let val = it.next().unwrap_or(BqValue::Null);
    let tz_v = it.next();
    if fmt_v.is_null() || val.is_null() || tz_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let BqValue::String(fmt) = fmt_v else {
        return Err(BqError::query(format!("{name} format must be STRING")));
    };
    let tz = match tz_v {
        Some(BqValue::String(tz_s)) => Some(BqTimezone::parse(&tz_s)?),
        Some(_) => return Err(BqError::query(format!("{name} timezone must be STRING"))),
        None => None,
    };
    let formatted = match (name, val) {
        ("FORMAT_DATE", BqValue::Date(d)) => d.format(&fmt).to_string(),
        ("FORMAT_DATE" | "FORMAT_DATETIME", BqValue::Datetime(dt)) => dt.format(&fmt).to_string(),
        ("FORMAT_TIME", BqValue::Time(t)) => t.format(&fmt).to_string(),
        ("FORMAT_DATETIME" | "FORMAT_TIMESTAMP", BqValue::Timestamp(ts)) => {
            tz.unwrap_or(BqTimezone::Utc).format_timestamp(ts, &fmt)
        }
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
    let tz_v = it.next();
    if fmt_v.is_null() || str_v.is_null() || tz_v.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(fmt), BqValue::String(s)) = (fmt_v, str_v) else {
        return Err(BqError::query(format!("{name} expects STRING arguments")));
    };
    let tz = match tz_v {
        Some(BqValue::String(tz_s)) => Some(BqTimezone::parse(&tz_s)?),
        Some(_) => return Err(BqError::query(format!("{name} timezone must be STRING"))),
        None => None,
    };
    match name {
        "PARSE_DATE" => parse_date_fmt(&s, &fmt)
            .and_then(validate_bq_date)
            .map(BqValue::Date),
        "PARSE_DATETIME" => NaiveDateTime::parse_from_str(&s, &fmt)
            .map(BqValue::Datetime)
            .map_err(|e| BqError::semantic(format!("Failed to parse DATETIME: {e}"))),
        "PARSE_TIME" => NaiveTime::parse_from_str(&s, &fmt)
            .map(BqValue::Time)
            .map_err(|e| BqError::semantic(format!("Failed to parse TIME: {e}"))),
        "PARSE_TIMESTAMP" => parse_timestamp_fmt(&s, &fmt, tz).map(BqValue::Timestamp),
        _ => Err(BqError::internal("unexpected parse fn")),
    }
}

fn parse_date_fmt(s: &str, fmt: &str) -> BqResult<NaiveDate> {
    if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
        return Ok(d);
    }
    // GoogleSQL defaults omitted weekday to Monday for %W and Sunday for %U.
    if fmt.contains("%W") && !fmt.contains("%w") && !fmt.contains("%u") && !fmt.contains("%a") {
        if let Ok(d) = NaiveDate::parse_from_str(&format!("{s} 1"), &format!("{fmt} %u")) {
            return Ok(d);
        }
    }
    if fmt.contains("%U") && !fmt.contains("%w") && !fmt.contains("%u") && !fmt.contains("%a") {
        if let Ok(d) = NaiveDate::parse_from_str(&format!("{s} 0"), &format!("{fmt} %w")) {
            return Ok(d);
        }
    }
    Err(BqError::semantic(format!(
        "Failed to parse DATE '{s}' with format '{fmt}'"
    )))
}

fn parse_timestamp_fmt(s: &str, fmt: &str, tz: Option<BqTimezone>) -> BqResult<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
        return tz.unwrap_or(BqTimezone::Utc).local_to_utc(ndt);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
        let ndt = d.and_hms_opt(0, 0, 0).unwrap_or_default();
        return tz.unwrap_or(BqTimezone::Utc).local_to_utc(ndt);
    }
    Err(BqError::semantic(format!(
        "Failed to parse TIMESTAMP '{s}' with format '{fmt}'"
    )))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TIMESTAMP_* expects INT64 or NULL"
)]
pub(super) fn eval_ts_epoch(args: Vec<BqValue>, mult_micros: i64) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("TIMESTAMP_* expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Int64(n) => {
            let micros = n
                .checked_mul(mult_micros)
                .ok_or_else(|| BqError::semantic("TIMESTAMP overflow"))?;
            DateTime::from_timestamp_micros(micros)
                .map(BqValue::Timestamp)
                .ok_or_else(|| BqError::semantic("TIMESTAMP out of range"))
        }
        _ => Err(BqError::query("TIMESTAMP_* expects INT64")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNIX_* expects TIMESTAMP or NULL"
)]
pub(super) fn eval_unix_epoch(args: Vec<BqValue>, div_micros: i64) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("UNIX_* expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Timestamp(ts) => Ok(BqValue::Int64(ts.timestamp_micros() / div_micros)),
        _ => Err(BqError::query("UNIX_* expects TIMESTAMP")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNIX_DATE expects DATE or NULL"
)]
pub(super) fn eval_unix_date(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("UNIX_DATE expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let coerced = coerce_date_arg(arg)?;
    match coerced {
        BqValue::Date(d) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or(d);
            Ok(BqValue::Int64((d - epoch).num_days()))
        }
        _ => Err(BqError::query("UNIX_DATE expects DATE")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "DATE_FROM_UNIX_DATE expects INT64 or NULL"
)]
pub(super) fn eval_date_from_unix(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("DATE_FROM_UNIX_DATE expects 1 argument"))?;
    match arg {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Int64(days) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
            epoch
                .checked_add_signed(Duration::days(days))
                .and_then(|d| validate_bq_date(d).ok())
                .map(BqValue::Date)
                .ok_or_else(|| BqError::semantic("DATE_FROM_UNIX_DATE out of range"))
        }
        _ => Err(BqError::query("DATE_FROM_UNIX_DATE expects INT64")),
    }
}

pub(super) fn eval_generate_date_array(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query(
            "GENERATE_DATE_ARRAY expects 2 or 3 arguments",
        ));
    }
    let mut it = args.into_iter();
    let start_v = it.next().unwrap_or(BqValue::Null);
    let end_v = it.next().unwrap_or(BqValue::Null);
    let step_v = it.next();
    if start_v.is_null() || end_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (start_v, end_v) = (coerce_date_arg(start_v)?, coerce_date_arg(end_v)?);
    let (BqValue::Date(start), BqValue::Date(end)) = (start_v, end_v) else {
        return Err(BqError::query("GENERATE_DATE_ARRAY expects DATE bounds"));
    };
    let step = match step_v {
        Some(BqValue::Interval(iv)) => iv,
        None => BqInterval::new(0, 1, 0),
        _ => return Err(BqError::query("GENERATE_DATE_ARRAY step must be INTERVAL")),
    };
    if step.months == 0 && step.days == 0 {
        return Err(BqError::semantic("GENERATE_DATE_ARRAY step cannot be zero"));
    }
    let forward = step.months > 0 || (step.months == 0 && step.days > 0);
    let mut out = Vec::new();
    let mut cur = start;
    while (if forward { cur <= end } else { cur >= end }) && out.len() < 100_000 {
        out.push(BqValue::Date(cur));
        cur = add_interval_to_date(cur, step, true)?;
    }
    Ok(BqValue::Array(out))
}
