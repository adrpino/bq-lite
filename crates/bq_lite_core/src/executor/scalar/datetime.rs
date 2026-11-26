use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;

use crate::error::{BqError, BqResult};
use crate::executor::context::ExecContext;
use crate::types::BqValue;

use super::datetime_calc::{
    eval_date_add_sub, eval_date_diff, eval_date_from_unix, eval_date_trunc, eval_datetime_add_sub,
    eval_datetime_diff, eval_datetime_trunc, eval_generate_date_array, eval_last_day,
    eval_timestamp_add_sub, eval_timestamp_diff, eval_timestamp_trunc, eval_ts_epoch,
    eval_unix_date, eval_unix_epoch,
};
use super::datetime_fmt::{eval_format_dt, eval_parse_dt};

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
        "CURRENT_DATETIME" => Ok(Some(BqValue::Datetime(ctx.now.naive_utc()))),
        "CURRENT_TIME" => Ok(Some(BqValue::Time(ctx.now.time()))),
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
    if let Some(BqValue::String(tz_name)) = args.first() {
        let tz: Tz = tz_name
            .parse()
            .map_err(|_| BqError::semantic(format!("Invalid timezone '{tz_name}'")))?;
        return Ok(BqValue::Date(ctx.now.with_timezone(&tz).date_naive()));
    }
    Ok(BqValue::Date(ctx.now.date_naive()))
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
        return Ok(BqValue::Date(date));
    }
    let Some(first) = args.first() else {
        return Err(BqError::query("DATE expects 1 to 3 arguments"));
    };
    if first.is_null() {
        return Ok(BqValue::Null);
    }
    match first {
        BqValue::Date(d) => Ok(BqValue::Date(*d)),
        BqValue::Datetime(dt) => Ok(BqValue::Date(dt.date())),
        BqValue::Timestamp(ts) => {
            if let Some(BqValue::String(tz_s)) = args.get(1) {
                let tz: Tz = tz_s
                    .parse()
                    .map_err(|_| BqError::semantic(format!("Invalid timezone '{tz_s}'")))?;
                Ok(BqValue::Date(ts.with_timezone(&tz).date_naive()))
            } else {
                Ok(BqValue::Date(ts.date_naive()))
            }
        }
        _ => crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Date),
    }
}

fn eval_datetime_ctor(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("DATETIME expects at least 1 argument"));
    };
    if first.is_null() {
        return Ok(BqValue::Null);
    }
    if let (Some(BqValue::Date(d)), Some(BqValue::Time(t))) = (args.first(), args.get(1)) {
        return Ok(BqValue::Datetime(NaiveDateTime::new(*d, *t)));
    }
    if let BqValue::Timestamp(ts) = first {
        if let Some(BqValue::String(tz_s)) = args.get(1) {
            let tz: Tz = tz_s
                .parse()
                .map_err(|_| BqError::semantic(format!("Invalid timezone '{tz_s}'")))?;
            return Ok(BqValue::Datetime(ts.with_timezone(&tz).naive_local()));
        }
        return Ok(BqValue::Datetime(ts.naive_utc()));
    }
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Datetime)
}

fn eval_time_ctor(args: Vec<BqValue>) -> BqResult<BqValue> {
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
        return Err(BqError::query("TIME expects 1 or 3 arguments"));
    };
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Time)
}

fn eval_timestamp_ctor(args: &[BqValue]) -> BqResult<BqValue> {
    let Some(first) = args.first() else {
        return Err(BqError::query("TIMESTAMP expects 1 or 2 arguments"));
    };
    if first.is_null() {
        return Ok(BqValue::Null);
    }
    if let (BqValue::Datetime(dt), Some(BqValue::String(tz_s))) = (first, args.get(1)) {
        let tz: Tz = tz_s
            .parse()
            .map_err(|_| BqError::semantic(format!("Invalid timezone '{tz_s}'")))?;
        let local = dt
            .and_local_timezone(tz)
            .single()
            .ok_or_else(|| BqError::semantic("Ambiguous or invalid local datetime for timezone"))?;
        return Ok(BqValue::Timestamp(local.with_timezone(&Utc)));
    }
    crate::types::coercion::cast_value(first.clone(), &crate::types::BqType::Timestamp)
}
