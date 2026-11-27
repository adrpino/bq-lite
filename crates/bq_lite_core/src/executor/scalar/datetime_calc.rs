use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::error::{BqError, BqResult};
use crate::types::coercion::cast_value;
use crate::types::{BqInterval, BqType, BqValue};

pub(super) fn coerce_date_arg(val: BqValue) -> BqResult<BqValue> {
    if matches!(val, BqValue::String(_)) {
        cast_value(val, &BqType::Date)
    } else {
        Ok(val)
    }
}

pub(super) fn coerce_datetime_arg(val: BqValue) -> BqResult<BqValue> {
    if matches!(val, BqValue::String(_)) {
        cast_value(val, &BqType::Datetime)
    } else {
        Ok(val)
    }
}

pub(super) fn coerce_timestamp_arg(val: BqValue) -> BqResult<BqValue> {
    if matches!(val, BqValue::String(_)) {
        cast_value(val, &BqType::Timestamp)
    } else {
        Ok(val)
    }
}

pub(super) fn coerce_time_arg(val: BqValue) -> BqResult<BqValue> {
    if matches!(val, BqValue::String(_)) {
        cast_value(val, &BqType::Time)
    } else {
        Ok(val)
    }
}

pub(in crate::executor) fn add_interval_to_date(
    d: NaiveDate,
    iv: BqInterval,
    add: bool,
) -> BqResult<NaiveDate> {
    let sign = if add { 1 } else { -1 };
    let total_months = iv.months * sign;
    let d1 = if total_months >= 0 {
        d.checked_add_months(Months::new(u32::try_from(total_months).unwrap_or(0)))
    } else {
        d.checked_sub_months(Months::new(u32::try_from(-total_months).unwrap_or(0)))
    }
    .ok_or_else(|| BqError::semantic("DATE overflow"))?;
    let total_days = i64::from(iv.days) * i64::from(sign);
    d1.checked_add_signed(Duration::days(total_days))
        .ok_or_else(|| BqError::semantic("DATE overflow"))
}

pub(super) fn eval_date_add_sub(args: Vec<BqValue>, add: bool) -> BqResult<BqValue> {
    let [d_val, iv_val]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("DATE_ADD/SUB expects 2 arguments"))?;
    if d_val.is_null() || iv_val.is_null() {
        return Ok(BqValue::Null);
    }
    let d_val = coerce_date_arg(d_val)?;
    let (BqValue::Date(d), BqValue::Interval(iv)) = (d_val, iv_val) else {
        return Err(BqError::query("DATE_ADD/SUB expects (DATE, INTERVAL)"));
    };
    add_interval_to_date(d, iv, add).map(BqValue::Date)
}

pub(super) fn eval_datetime_add_sub(args: Vec<BqValue>, add: bool) -> BqResult<BqValue> {
    let [dt_val, iv_val]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("DATETIME_ADD/SUB expects 2 arguments"))?;
    if dt_val.is_null() || iv_val.is_null() {
        return Ok(BqValue::Null);
    }
    let dt_val = coerce_datetime_arg(dt_val)?;
    let (BqValue::Datetime(dt), BqValue::Interval(iv)) = (dt_val, iv_val) else {
        return Err(BqError::query(
            "DATETIME_ADD/SUB expects (DATETIME, INTERVAL)",
        ));
    };
    let d = add_interval_to_date(dt.date(), iv, add)?;
    let base = NaiveDateTime::new(d, dt.time());
    let micros = if add { iv.micros } else { -iv.micros };
    Ok(BqValue::Datetime(base + Duration::microseconds(micros)))
}

pub(super) fn eval_timestamp_add_sub(args: Vec<BqValue>, add: bool) -> BqResult<BqValue> {
    let [ts_val, iv_val]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("TIMESTAMP_ADD/SUB expects 2 arguments"))?;
    if ts_val.is_null() || iv_val.is_null() {
        return Ok(BqValue::Null);
    }
    let ts_val = coerce_timestamp_arg(ts_val)?;
    let (BqValue::Timestamp(ts), BqValue::Interval(iv)) = (ts_val, iv_val) else {
        return Err(BqError::query(
            "TIMESTAMP_ADD/SUB expects (TIMESTAMP, INTERVAL)",
        ));
    };
    let sign = if add { 1_i64 } else { -1_i64 };
    let delta =
        Duration::days(i64::from(iv.days) * sign) + Duration::microseconds(iv.micros * sign);
    Ok(BqValue::Timestamp(ts + delta))
}

pub(super) fn eval_date_diff(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b, part]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("DATE_DIFF expects 3 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let (a, b) = (coerce_date_arg(a)?, coerce_date_arg(b)?);
    let (BqValue::Date(da), BqValue::Date(db), BqValue::String(p)) = (a, b, part) else {
        return Err(BqError::query("DATE_DIFF expects (DATE, DATE, PART)"));
    };
    let diff = match p.to_ascii_uppercase().as_str() {
        "DAY" => (da - db).num_days(),
        "WEEK" => (da - db).num_weeks(),
        "MONTH" => i64::from(
            (da.year() - db.year()) * 12
                + (i32::try_from(da.month()).unwrap_or(0) - i32::try_from(db.month()).unwrap_or(0)),
        ),
        "QUARTER" => i64::from(
            (da.year() - db.year()) * 4
                + (i32::try_from((da.month() - 1) / 3).unwrap_or(0)
                    - i32::try_from((db.month() - 1) / 3).unwrap_or(0)),
        ),
        "YEAR" => i64::from(da.year() - db.year()),
        _ => return Err(BqError::query(format!("Unsupported DATE_DIFF part '{p}'"))),
    };
    Ok(BqValue::Int64(diff))
}

pub(super) fn eval_datetime_diff(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b, part]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("DATETIME_DIFF expects 3 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let (a, b) = (coerce_datetime_arg(a)?, coerce_datetime_arg(b)?);
    let (BqValue::Datetime(da), BqValue::Datetime(db), BqValue::String(p)) = (a, b, part) else {
        return Err(BqError::query(
            "DATETIME_DIFF expects (DATETIME, DATETIME, PART)",
        ));
    };
    diff_duration(da - db, &p)
}

pub(super) fn eval_timestamp_diff(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a, b, part]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("TIMESTAMP_DIFF expects 3 arguments"))?;
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let (a, b) = (coerce_timestamp_arg(a)?, coerce_timestamp_arg(b)?);
    let (BqValue::Timestamp(ta), BqValue::Timestamp(tb), BqValue::String(p)) = (a, b, part) else {
        return Err(BqError::query(
            "TIMESTAMP_DIFF expects (TIMESTAMP, TIMESTAMP, PART)",
        ));
    };
    diff_duration(ta - tb, &p)
}

fn diff_duration(dur: Duration, part: &str) -> BqResult<BqValue> {
    let val = match part.to_ascii_uppercase().as_str() {
        "DAY" => dur.num_days(),
        "HOUR" => dur.num_hours(),
        "MINUTE" => dur.num_minutes(),
        "SECOND" => dur.num_seconds(),
        "MILLISECOND" => dur.num_milliseconds(),
        "MICROSECOND" => dur.num_microseconds().unwrap_or(i64::MAX),
        _ => return Err(BqError::query(format!("Unsupported DIFF part '{part}'"))),
    };
    Ok(BqValue::Int64(val))
}

pub(super) fn eval_date_trunc(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [d_val, part]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("DATE_TRUNC expects 2 arguments"))?;
    if d_val.is_null() {
        return Ok(BqValue::Null);
    }
    let d_val = coerce_date_arg(d_val)?;
    let (BqValue::Date(d), BqValue::String(p)) = (d_val, part) else {
        return Err(BqError::query("DATE_TRUNC expects (DATE, PART)"));
    };
    let out = match p.to_ascii_uppercase().as_str() {
        "DAY" => d,
        "WEEK" => d - Duration::days(i64::from(d.weekday().num_days_from_sunday())),
        "ISOWEEK" => d - Duration::days(i64::from(d.weekday().num_days_from_monday())),
        "MONTH" => NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d),
        "QUARTER" => {
            let q_month = ((d.month() - 1) / 3) * 3 + 1;
            NaiveDate::from_ymd_opt(d.year(), q_month, 1).unwrap_or(d)
        }
        "YEAR" => NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap_or(d),
        _ => return Err(BqError::query(format!("Unsupported DATE_TRUNC part '{p}'"))),
    };
    Ok(BqValue::Date(out))
}

pub(super) fn eval_datetime_trunc(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [dt_val, part]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("DATETIME_TRUNC expects 2 arguments"))?;
    if dt_val.is_null() {
        return Ok(BqValue::Null);
    }
    let dt_val = coerce_datetime_arg(dt_val)?;
    let (BqValue::Datetime(dt), BqValue::String(p)) = (dt_val, part) else {
        return Err(BqError::query("DATETIME_TRUNC expects (DATETIME, PART)"));
    };
    let t = trunc_naive_datetime(dt, &p)?;
    Ok(BqValue::Datetime(t))
}

pub(super) fn eval_timestamp_trunc(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("TIMESTAMP_TRUNC expects 2 or 3 arguments"));
    }
    let mut it = args.into_iter();
    let ts_val = it.next().unwrap_or(BqValue::Null);
    let part = it.next().unwrap_or(BqValue::Null);
    if ts_val.is_null() {
        return Ok(BqValue::Null);
    }
    let ts_val = coerce_timestamp_arg(ts_val)?;
    let (BqValue::Timestamp(ts), BqValue::String(p)) = (ts_val, part) else {
        return Err(BqError::query("TIMESTAMP_TRUNC expects (TIMESTAMP, PART)"));
    };
    let dt = trunc_naive_datetime(ts.naive_utc(), &p)?;
    Ok(BqValue::Timestamp(
        DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc),
    ))
}

fn trunc_naive_datetime(dt: NaiveDateTime, part: &str) -> BqResult<NaiveDateTime> {
    use chrono::Timelike;
    let zero_time = NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
    match part.to_ascii_uppercase().as_str() {
        "YEAR" => Ok(NaiveDateTime::new(
            NaiveDate::from_ymd_opt(dt.year(), 1, 1).unwrap_or_else(|| dt.date()),
            zero_time,
        )),
        "MONTH" => Ok(NaiveDateTime::new(
            NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1).unwrap_or_else(|| dt.date()),
            zero_time,
        )),
        "DAY" => Ok(NaiveDateTime::new(dt.date(), zero_time)),
        "HOUR" => Ok(NaiveDateTime::new(
            dt.date(),
            NaiveTime::from_hms_opt(dt.hour(), 0, 0).unwrap_or(zero_time),
        )),
        "MINUTE" => Ok(NaiveDateTime::new(
            dt.date(),
            NaiveTime::from_hms_opt(dt.hour(), dt.minute(), 0).unwrap_or(zero_time),
        )),
        "SECOND" => Ok(NaiveDateTime::new(
            dt.date(),
            NaiveTime::from_hms_opt(dt.hour(), dt.minute(), dt.second()).unwrap_or(zero_time),
        )),
        _ => Err(BqError::query(format!("Unsupported TRUNC part '{part}'"))),
    }
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
    match arg {
        BqValue::Null => Ok(BqValue::Null),
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
                .map(BqValue::Date)
                .ok_or_else(|| BqError::semantic("DATE_FROM_UNIX_DATE out of range"))
        }
        _ => Err(BqError::query("DATE_FROM_UNIX_DATE expects INT64")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "LAST_DAY expects DATE or DATETIME"
)]
pub(super) fn eval_last_day(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("LAST_DAY expects 1 or 2 arguments"));
    };
    if first.is_null() {
        return Ok(BqValue::Null);
    }
    let coerced = coerce_date_arg(first.clone())?;
    let d = match coerced {
        BqValue::Date(d) => d,
        BqValue::Datetime(dt) => dt.date(),
        _ => return Err(BqError::query("LAST_DAY expects DATE or DATETIME")),
    };
    let next_month = d
        .with_day(1)
        .and_then(|start| start.checked_add_months(Months::new(1)))
        .and_then(|nm| nm.pred_opt())
        .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?;
    Ok(BqValue::Date(next_month))
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
