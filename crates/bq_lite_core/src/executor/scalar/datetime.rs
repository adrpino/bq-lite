use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::error::{BqError, BqResult};
use crate::executor::context::ExecContext;
use crate::types::{parse_timestamp_str, validate_bq_date, BqTimezone, BqValue};

use super::datetime_calc::{
    eval_date_add_sub, eval_date_diff, eval_date_trunc, eval_datetime_add_sub, eval_datetime_diff,
    eval_datetime_trunc, eval_last_day, eval_timestamp_add_sub, eval_timestamp_diff,
    eval_timestamp_trunc,
};
use super::datetime_fmt::{
    eval_date_from_unix, eval_format_dt, eval_generate_date_array, eval_parse_dt, eval_ts_epoch,
    eval_unix_date, eval_unix_epoch,
};

pub(super) const DATETIME_FUNCTIONS: &[&str] = &[
    "CURRENT_DATE",
    "CURRENT_DATETIME",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATE",
    "DATE_ADD",
    "DATE_DIFF",
    "DATE_FROM_UNIX_DATE",
    "DATE_SUB",
    "DATE_TRUNC",
    "DATETIME",
    "DATETIME_ADD",
    "DATETIME_DIFF",
    "DATETIME_SUB",
    "DATETIME_TRUNC",
    "FORMAT_DATE",
    "FORMAT_DATETIME",
    "FORMAT_TIME",
    "FORMAT_TIMESTAMP",
    "GENERATE_DATE_ARRAY",
    "LAST_DAY",
    "PARSE_DATE",
    "PARSE_DATETIME",
    "PARSE_TIME",
    "PARSE_TIMESTAMP",
    "TIME",
    "TIMESTAMP",
    "TIMESTAMP_ADD",
    "TIMESTAMP_DIFF",
    "TIMESTAMP_MICROS",
    "TIMESTAMP_MILLIS",
    "TIMESTAMP_SECONDS",
    "TIMESTAMP_SUB",
    "TIMESTAMP_TRUNC",
    "UNIX_DATE",
    "UNIX_MICROS",
    "UNIX_MILLIS",
    "UNIX_SECONDS",
];

pub(super) fn eval_datetime_fn(
    name: &str,
    args: Vec<BqValue>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    match name {
        "CURRENT_TIMESTAMP" => Ok(Some(BqValue::Timestamp(ctx.now))),
        "CURRENT_DATE" => eval_current_date(&args, ctx).map(Some),
        "CURRENT_DATETIME" => eval_current_datetime(&args, ctx).map(Some),
        "CURRENT_TIME" => eval_current_time(&args, ctx).map(Some),
        "DATE" => eval_date_ctor(args).map(Some),
        "DATETIME" => eval_datetime_ctor(&args).map(Some),
        "TIME" => eval_time_ctor(args).map(Some),
        "TIMESTAMP" => eval_timestamp_ctor(&args).map(Some),
        "DATE_ADD" => eval_date_add_sub(args, true).map(Some),
        "DATE_SUB" => eval_date_add_sub(args, false).map(Some),
        "DATE_DIFF" => eval_date_diff(args).map(Some),
        "DATE_TRUNC" => eval_date_trunc(args).map(Some),
        "DATETIME_ADD" => eval_datetime_add_sub(args, true).map(Some),
        "DATETIME_SUB" => eval_datetime_add_sub(args, false).map(Some),
        "DATETIME_DIFF" => eval_datetime_diff(args).map(Some),
        "DATETIME_TRUNC" => eval_datetime_trunc(args).map(Some),
        "TIMESTAMP_ADD" => eval_timestamp_add_sub(args, true).map(Some),
        "TIMESTAMP_SUB" => eval_timestamp_add_sub(args, false).map(Some),
        "TIMESTAMP_DIFF" => eval_timestamp_diff(args).map(Some),
        "TIMESTAMP_TRUNC" => eval_timestamp_trunc(args).map(Some),
        "TIMESTAMP_SECONDS" => eval_ts_epoch(args, 1_000_000).map(Some),
        "TIMESTAMP_MILLIS" => eval_ts_epoch(args, 1_000).map(Some),
        "TIMESTAMP_MICROS" => eval_ts_epoch(args, 1).map(Some),
        "UNIX_SECONDS" => eval_unix_epoch(args, 1_000_000).map(Some),
        "UNIX_MILLIS" => eval_unix_epoch(args, 1_000).map(Some),
        "UNIX_MICROS" => eval_unix_epoch(args, 1).map(Some),
        "UNIX_DATE" => eval_unix_date(args).map(Some),
        "DATE_FROM_UNIX_DATE" => eval_date_from_unix(args).map(Some),
        "LAST_DAY" => eval_last_day(&args).map(Some),
        "GENERATE_DATE_ARRAY" => eval_generate_date_array(args).map(Some),
        "FORMAT_DATE" | "FORMAT_DATETIME" | "FORMAT_TIME" | "FORMAT_TIMESTAMP" => {
            eval_format_dt(name, args).map(Some)
        }
        "PARSE_DATE" | "PARSE_DATETIME" | "PARSE_TIME" | "PARSE_TIMESTAMP" => {
            eval_parse_dt(name, args).map(Some)
        }
        _ => Ok(None),
    }
}

fn eval_current_date(args: &[BqValue], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    match args.first() {
        None => Ok(BqValue::Date(ctx.now.date_naive())),
        Some(BqValue::Null) => Ok(BqValue::Null),
        Some(BqValue::String(tz_name)) => {
            let tz = BqTimezone::parse(tz_name)?;
            Ok(BqValue::Date(tz.utc_to_local(ctx.now).date()))
        }
        Some(_) => Err(BqError::query("CURRENT_DATE timezone must be STRING")),
    }
}

fn eval_current_datetime(args: &[BqValue], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    match args.first() {
        None => Ok(BqValue::Datetime(ctx.now.naive_utc())),
        Some(BqValue::Null) => Ok(BqValue::Null),
        Some(BqValue::String(tz_name)) => {
            let tz = BqTimezone::parse(tz_name)?;
            Ok(BqValue::Datetime(tz.utc_to_local(ctx.now)))
        }
        Some(_) => Err(BqError::query("CURRENT_DATETIME timezone must be STRING")),
    }
}

fn eval_current_time(args: &[BqValue], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    match args.first() {
        None => Ok(BqValue::Time(ctx.now.time())),
        Some(BqValue::Null) => Ok(BqValue::Null),
        Some(BqValue::String(tz_name)) => {
            let tz = BqTimezone::parse(tz_name)?;
            Ok(BqValue::Time(tz.utc_to_local(ctx.now).time()))
        }
        Some(_) => Err(BqError::query("CURRENT_TIME timezone must be STRING")),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining BqValue variants fall back to DATE cast"
)]
fn eval_date_ctor(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.len() == 3 {
        if args.iter().any(BqValue::is_null) {
            return Ok(BqValue::Null);
        }
        let [BqValue::Int64(y), BqValue::Int64(m), BqValue::Int64(d)]: [BqValue; 3] = args
            .try_into()
            .map_err(|_| BqError::query("DATE expects 3 INT64 args"))?
        else {
            return Err(BqError::query("DATE(y, m, d) expects INT64"));
        };
        let date = NaiveDate::from_ymd_opt(
            i32::try_from(y).unwrap_or(0),
            u32::try_from(m).unwrap_or(0),
            u32::try_from(d).unwrap_or(0),
        )
        .ok_or_else(|| BqError::semantic("Invalid DATE components"))?;
        return validate_bq_date(date).map(BqValue::Date);
    }
    let Some(first) = args.first() else {
        return Err(BqError::query("DATE expects 1 to 3 arguments"));
    };
    if first.is_null() || args.get(1).is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    match first {
        BqValue::Date(d) => Ok(BqValue::Date(*d)),
        BqValue::Datetime(dt) => Ok(BqValue::Date(dt.date())),
        BqValue::Timestamp(ts) => {
            if let Some(BqValue::String(tz_s)) = args.get(1) {
                let tz = BqTimezone::parse(tz_s)?;
                Ok(BqValue::Date(tz.utc_to_local(*ts).date()))
            } else {
                Ok(BqValue::Date(ts.date_naive()))
            }
        }
        BqValue::String(s) => {
            if let Ok(v) =
                crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Date)
            {
                return Ok(v);
            }
            let ts = parse_timestamp_str(s)?;
            if let Some(BqValue::String(tz_s)) = args.get(1) {
                let tz = BqTimezone::parse(tz_s)?;
                Ok(BqValue::Date(tz.utc_to_local(ts).date()))
            } else {
                Ok(BqValue::Date(ts.date_naive()))
            }
        }
        _ => crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Date),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "DATETIME(y,m,d,h,min,s) requires INT64 arguments"
)]
fn eval_datetime_ctor(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("DATETIME expects at least 1 argument"));
    };
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if args.len() == 6 {
        let vals: Vec<i64> = args
            .iter()
            .map(|v| match v {
                BqValue::Int64(n) => Ok(*n),
                _ => Err(BqError::query("DATETIME(y,m,d,h,min,s) expects INT64")),
            })
            .collect::<BqResult<_>>()?;
        let d = NaiveDate::from_ymd_opt(
            i32::try_from(vals.first().copied().unwrap_or(0)).unwrap_or(0),
            u32::try_from(vals.get(1).copied().unwrap_or(0)).unwrap_or(0),
            u32::try_from(vals.get(2).copied().unwrap_or(0)).unwrap_or(0),
        )
        .ok_or_else(|| BqError::semantic("Invalid DATETIME date components"))?;
        let d = validate_bq_date(d)?;
        let t = NaiveTime::from_hms_opt(
            u32::try_from(vals.get(3).copied().unwrap_or(99)).unwrap_or(99),
            u32::try_from(vals.get(4).copied().unwrap_or(99)).unwrap_or(99),
            u32::try_from(vals.get(5).copied().unwrap_or(99)).unwrap_or(99),
        )
        .ok_or_else(|| BqError::semantic("Invalid DATETIME time components"))?;
        return Ok(BqValue::Datetime(NaiveDateTime::new(d, t)));
    }
    if let (Some(BqValue::Date(d)), Some(BqValue::Time(t))) = (args.first(), args.get(1)) {
        return Ok(BqValue::Datetime(NaiveDateTime::new(*d, *t)));
    }
    if let BqValue::Timestamp(ts) = first {
        if let Some(BqValue::String(tz_s)) = args.get(1) {
            let tz = BqTimezone::parse(tz_s)?;
            return Ok(BqValue::Datetime(tz.utc_to_local(*ts)));
        }
        return Ok(BqValue::Datetime(ts.naive_utc()));
    }
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Datetime)
}

fn eval_time_ctor(args: Vec<BqValue>) -> BqResult<BqValue> {
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if args.len() == 3 {
        let [BqValue::Int64(h), BqValue::Int64(m), BqValue::Int64(s)]: [BqValue; 3] = args
            .try_into()
            .map_err(|_| BqError::query("TIME expects 3 INT64 args"))?
        else {
            return Err(BqError::query("TIME(h, m, s) expects INT64"));
        };
        let t = NaiveTime::from_hms_opt(
            u32::try_from(h).unwrap_or(99),
            u32::try_from(m).unwrap_or(99),
            u32::try_from(s).unwrap_or(99),
        )
        .ok_or_else(|| BqError::semantic("Invalid TIME components"))?;
        return Ok(BqValue::Time(t));
    }
    let Some(first) = args.first() else {
        return Err(BqError::query("TIME expects 1 to 3 arguments"));
    };
    if let BqValue::Timestamp(ts) = first {
        if let Some(BqValue::String(tz_s)) = args.get(1) {
            let tz = BqTimezone::parse(tz_s)?;
            return Ok(BqValue::Time(tz.utc_to_local(*ts).time()));
        }
        return Ok(BqValue::Time(ts.time()));
    }
    if let BqValue::String(s) = first {
        if let Ok(v) =
            crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Time)
        {
            return Ok(v);
        }
        let ts = parse_timestamp_str(s)?;
        if let Some(BqValue::String(tz_s)) = args.get(1) {
            let tz = BqTimezone::parse(tz_s)?;
            return Ok(BqValue::Time(tz.utc_to_local(ts).time()));
        }
        return Ok(BqValue::Time(ts.time()));
    }
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Time)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Non-temporal TIMESTAMP inputs fall through to cast_value"
)]
fn eval_timestamp_ctor(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("TIMESTAMP expects 1 or 2 arguments"));
    };
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if let Some(BqValue::String(tz_s)) = args.get(1) {
        let tz = BqTimezone::parse(tz_s)?;
        match first {
            BqValue::Datetime(dt) => return tz.local_to_utc(*dt).map(BqValue::Timestamp),
            BqValue::Date(d) => {
                let dt = d.and_hms_opt(0, 0, 0).unwrap_or_default();
                return tz.local_to_utc(dt).map(BqValue::Timestamp);
            }
            BqValue::String(s) => {
                if let Ok(dt) = crate::types::parse_naive_datetime(s) {
                    return tz.local_to_utc(dt).map(BqValue::Timestamp);
                }
            }
            _ => {}
        }
    }
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Timestamp)
}
