use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::error::{BqError, BqResult};
use crate::types::coercion::cast_value;
use crate::types::{
    validate_bq_date, validate_bq_datetime, BqInterval, BqTimezone, BqType, BqValue,
};

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
    let sign = if add { 1_i64 } else { -1_i64 };
    let total_months = i64::from(iv.months) * sign;
    let d1 = if total_months >= 0 {
        let m = u32::try_from(total_months).map_err(|_| BqError::semantic("DATE overflow"))?;
        d.checked_add_months(Months::new(m))
    } else {
        let m = u32::try_from(total_months.unsigned_abs())
            .map_err(|_| BqError::semantic("DATE overflow"))?;
        d.checked_sub_months(Months::new(m))
    }
    .ok_or_else(|| BqError::semantic("DATE overflow"))?;
    let total_days = i64::from(iv.days) * sign;
    let d2 = d1
        .checked_add_signed(Duration::days(total_days))
        .ok_or_else(|| BqError::semantic("DATE overflow"))?;
    validate_bq_date(d2)
}

pub(super) fn eval_date_add_sub(args: Vec<BqValue>, add: bool) -> BqResult<BqValue> {
    if matches!(args.first(), Some(BqValue::Datetime(_))) {
        return eval_datetime_add_sub(args, add);
    }
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
    if matches!(args.first(), Some(BqValue::Timestamp(_))) {
        return eval_timestamp_add_sub(args, add);
    }
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
    let out = base
        .checked_add_signed(Duration::microseconds(micros))
        .ok_or_else(|| BqError::semantic("DATETIME overflow"))?;
    validate_bq_datetime(out).map(BqValue::Datetime)
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

fn parse_week_start_weekday(part_upper: &str) -> Option<Weekday> {
    if part_upper == "WEEK" {
        return Some(Weekday::Sun);
    }
    if part_upper == "ISOWEEK" {
        return Some(Weekday::Mon);
    }
    let inner = part_upper.strip_prefix("WEEK(")?.strip_suffix(')')?;
    match inner.trim() {
        "SUNDAY" => Some(Weekday::Sun),
        "MONDAY" => Some(Weekday::Mon),
        "TUESDAY" => Some(Weekday::Tue),
        "WEDNESDAY" => Some(Weekday::Wed),
        "THURSDAY" => Some(Weekday::Thu),
        "FRIDAY" => Some(Weekday::Fri),
        "SATURDAY" => Some(Weekday::Sat),
        _ => None,
    }
}

fn trunc_date_part(d: NaiveDate, part_upper: &str, raw_part: &str) -> BqResult<NaiveDate> {
    if let Some(start_wd) = parse_week_start_weekday(part_upper) {
        let cur = i64::from(d.weekday().num_days_from_sunday());
        let st = i64::from(start_wd.num_days_from_sunday());
        let back = (cur - st).rem_euclid(7);
        return Ok(d - Duration::days(back));
    }
    match part_upper {
        "DAY" => Ok(d),
        "MONTH" => Ok(NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d)),
        "QUARTER" => {
            let q_month = ((d.month() - 1) / 3) * 3 + 1;
            Ok(NaiveDate::from_ymd_opt(d.year(), q_month, 1).unwrap_or(d))
        }
        "YEAR" => Ok(NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap_or(d)),
        "ISOYEAR" => {
            Ok(NaiveDate::from_isoywd_opt(d.iso_week().year(), 1, Weekday::Mon).unwrap_or(d))
        }
        _ => Err(BqError::query(format!(
            "Unsupported DATE_TRUNC part '{raw_part}'"
        ))),
    }
}

fn date_diff_part(da: NaiveDate, db: NaiveDate, part_upper: &str, raw_part: &str) -> BqResult<i64> {
    if parse_week_start_weekday(part_upper).is_some() {
        let ta = trunc_date_part(da, part_upper, raw_part)?;
        let tb = trunc_date_part(db, part_upper, raw_part)?;
        return Ok((ta - tb).num_days() / 7);
    }
    match part_upper {
        "DAY" => Ok((da - db).num_days()),
        "MONTH" => Ok(i64::from(
            (da.year() - db.year()) * 12
                + (i32::try_from(da.month()).unwrap_or(0) - i32::try_from(db.month()).unwrap_or(0)),
        )),
        "QUARTER" => Ok(i64::from(
            (da.year() - db.year()) * 4
                + (i32::try_from((da.month() - 1) / 3).unwrap_or(0)
                    - i32::try_from((db.month() - 1) / 3).unwrap_or(0)),
        )),
        "YEAR" => Ok(i64::from(da.year() - db.year())),
        "ISOYEAR" => Ok(i64::from(da.iso_week().year() - db.iso_week().year())),
        _ => Err(BqError::query(format!(
            "Unsupported DATE_DIFF part '{raw_part}'"
        ))),
    }
}

pub(super) fn eval_date_diff(args: Vec<BqValue>) -> BqResult<BqValue> {
    if matches!(args.first(), Some(BqValue::Datetime(_)))
        || matches!(args.get(1), Some(BqValue::Datetime(_)))
    {
        return eval_datetime_diff(args);
    }
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
    let upper = p.to_ascii_uppercase();
    date_diff_part(da, db, &upper, &p).map(BqValue::Int64)
}

pub(super) fn eval_datetime_diff(args: Vec<BqValue>) -> BqResult<BqValue> {
    if matches!(args.first(), Some(BqValue::Timestamp(_)))
        || matches!(args.get(1), Some(BqValue::Timestamp(_)))
    {
        return eval_timestamp_diff(args);
    }
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
    let upper = p.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "DAY" | "MONTH" | "QUARTER" | "YEAR" | "ISOYEAR"
    ) || parse_week_start_weekday(&upper).is_some()
    {
        return date_diff_part(da.date(), db.date(), &upper, &p).map(BqValue::Int64);
    }
    let ta = trunc_naive_datetime(da, &p)?;
    let tb = trunc_naive_datetime(db, &p)?;
    diff_duration(ta - tb, &p)
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
    if matches!(args.first(), Some(BqValue::Datetime(_))) {
        return eval_datetime_trunc(args);
    }
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
    trunc_date_part(d, &p.to_ascii_uppercase(), &p).map(BqValue::Date)
}

pub(super) fn eval_datetime_trunc(args: Vec<BqValue>) -> BqResult<BqValue> {
    if matches!(args.first(), Some(BqValue::Timestamp(_))) {
        return eval_timestamp_trunc(args);
    }
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
    let zone_val = it.next();
    if ts_val.is_null() || zone_val.as_ref().is_some_and(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let ts_val = coerce_timestamp_arg(ts_val)?;
    let (BqValue::Timestamp(ts), BqValue::String(p)) = (ts_val, part) else {
        return Err(BqError::query("TIMESTAMP_TRUNC expects (TIMESTAMP, PART)"));
    };
    let tz = match zone_val {
        Some(BqValue::String(tz_s)) => BqTimezone::parse(&tz_s)?,
        Some(_) => return Err(BqError::query("TIMESTAMP_TRUNC timezone must be STRING")),
        None => BqTimezone::Utc,
    };
    let local_dt = tz.utc_to_local(ts);
    let trunc_local = trunc_naive_datetime(local_dt, &p)?;
    tz.local_to_utc(trunc_local).map(BqValue::Timestamp)
}

fn trunc_naive_datetime(dt: NaiveDateTime, part: &str) -> BqResult<NaiveDateTime> {
    let zero_time = NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
    let upper = part.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "DAY" | "MONTH" | "QUARTER" | "YEAR" | "ISOYEAR"
    ) || parse_week_start_weekday(&upper).is_some()
    {
        let d = trunc_date_part(dt.date(), &upper, part)?;
        return Ok(NaiveDateTime::new(d, zero_time));
    }
    match upper.as_str() {
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
        "MILLISECOND" => Ok(NaiveDateTime::new(
            dt.date(),
            NaiveTime::from_hms_milli_opt(
                dt.hour(),
                dt.minute(),
                dt.second(),
                dt.nanosecond() / 1_000_000,
            )
            .unwrap_or(zero_time),
        )),
        "MICROSECOND" => Ok(NaiveDateTime::new(
            dt.date(),
            NaiveTime::from_hms_micro_opt(
                dt.hour(),
                dt.minute(),
                dt.second(),
                dt.nanosecond() / 1_000,
            )
            .unwrap_or(zero_time),
        )),
        _ => Err(BqError::query(format!("Unsupported TRUNC part '{part}'"))),
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
    let part = match args.get(1) {
        None => "MONTH".to_owned(),
        Some(BqValue::String(s)) => s.to_ascii_uppercase(),
        Some(_) => return Err(BqError::query("LAST_DAY part must be a date part")),
    };
    let res = if parse_week_start_weekday(&part).is_some() {
        let start = trunc_date_part(d, &part, &part)?;
        start
            .checked_add_signed(Duration::days(6))
            .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?
    } else {
        match part.as_str() {
            "MONTH" => d
                .with_day(1)
                .and_then(|start| start.checked_add_months(Months::new(1)))
                .and_then(|nm| nm.pred_opt())
                .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?,
            "QUARTER" => {
                let q_start_m = ((d.month() - 1) / 3) * 3 + 1;
                NaiveDate::from_ymd_opt(d.year(), q_start_m, 1)
                    .and_then(|start| start.checked_add_months(Months::new(3)))
                    .and_then(|nq| nq.pred_opt())
                    .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?
            }
            "YEAR" => NaiveDate::from_ymd_opt(d.year(), 12, 31)
                .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?,
            "ISOYEAR" => {
                let next_iso = d.iso_week().year() + 1;
                NaiveDate::from_isoywd_opt(next_iso, 1, Weekday::Mon)
                    .and_then(|s| s.pred_opt())
                    .ok_or_else(|| BqError::semantic("LAST_DAY overflow"))?
            }
            _ => {
                return Err(BqError::query(format!(
                    "Unsupported LAST_DAY part '{part}'"
                )))
            }
        }
    };
    validate_bq_date(res).map(BqValue::Date)
}
