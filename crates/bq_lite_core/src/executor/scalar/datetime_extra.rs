use chrono::{
    DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc,
    Weekday,
};

use crate::error::{BqError, BqResult};
use crate::types::{validate_bq_date, validate_bq_datetime, BqInterval, BqTimezone, BqValue};

use super::datetime_calc::{
    add_interval_to_date, coerce_date_arg, coerce_datetime_arg, coerce_time_arg,
    coerce_timestamp_arg,
};

pub(super) const DATETIME_EXTRA_FUNCTIONS: &[&str] = &[
    "ADD_MONTHS",
    "DATE_BUCKET",
    "DATETIME_BUCKET",
    "MONTHS_BETWEEN",
    "NEXT_DAY",
    "TIME_ADD",
    "TIME_DIFF",
    "TIME_SUB",
    "TIME_TRUNC",
    "TIMESTAMP_BUCKET",
];

pub(super) fn eval_datetime_extra_fn(name: &str, args: &[BqValue]) -> BqResult<Option<BqValue>> {
    match name {
        "ADD_MONTHS" => eval_add_months(args).map(Some),
        "NEXT_DAY" => eval_next_day(args).map(Some),
        "MONTHS_BETWEEN" => eval_months_between(args).map(Some),
        "DATE_BUCKET" => eval_date_bucket(args).map(Some),
        "DATETIME_BUCKET" => eval_datetime_bucket(args).map(Some),
        "TIMESTAMP_BUCKET" => eval_timestamp_bucket(args).map(Some),
        "TIME_ADD" => eval_time_add_sub(args, true).map(Some),
        "TIME_SUB" => eval_time_add_sub(args, false).map(Some),
        "TIME_TRUNC" => eval_time_trunc(args).map(Some),
        "TIME_DIFF" => eval_time_diff(args).map(Some),
        _ => Ok(None),
    }
}

/// Converts a UTC `DateTime` to wall-clock `NaiveDateTime` in a `BigQuery` timezone string.
pub(in crate::executor) fn timestamp_to_tz_naive(
    ts: DateTime<Utc>,
    tz_str: &str,
) -> BqResult<NaiveDateTime> {
    let tz = BqTimezone::parse(tz_str)?;
    Ok(tz.utc_to_local(ts))
}

fn is_last_day_of_month(d: NaiveDate) -> bool {
    d.succ_opt().is_some_and(|next| next.month() != d.month())
}

fn last_day_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, 1)?
        .checked_add_months(Months::new(1))?
        .pred_opt()
}

fn add_months_to_date(d: NaiveDate, months: i64) -> BqResult<NaiveDate> {
    let was_eom = is_last_day_of_month(d);
    let shifted = if months >= 0 {
        let m = u32::try_from(months).map_err(|_| BqError::semantic("ADD_MONTHS overflow"))?;
        d.checked_add_months(Months::new(m))
    } else {
        let m = u32::try_from(months.unsigned_abs())
            .map_err(|_| BqError::semantic("ADD_MONTHS overflow"))?;
        d.checked_sub_months(Months::new(m))
    }
    .ok_or_else(|| BqError::semantic("ADD_MONTHS overflow"))?;
    let res = if was_eom {
        last_day_of_month(shifted.year(), shifted.month()).unwrap_or(shifted)
    } else {
        shifted
    };
    validate_bq_date(res)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "ADD_MONTHS accepts DATE, DATETIME, or STRING"
)]
fn eval_add_months(args: &[BqValue]) -> BqResult<BqValue> {
    let [dt_val, m_val] = args else {
        return Err(BqError::query("ADD_MONTHS expects 2 arguments"));
    };
    if dt_val.is_null() || m_val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Int64(months) = m_val else {
        return Err(BqError::query("ADD_MONTHS expects INT64 months"));
    };
    match dt_val {
        BqValue::Date(d) => add_months_to_date(*d, *months).map(BqValue::Date),
        BqValue::Datetime(dt) => {
            let d = add_months_to_date(dt.date(), *months)?;
            Ok(BqValue::Datetime(NaiveDateTime::new(d, dt.time())))
        }
        BqValue::String(_) => {
            let BqValue::Date(d) = coerce_date_arg(dt_val.clone())? else {
                return Err(BqError::query("ADD_MONTHS expects DATE or DATETIME"));
            };
            add_months_to_date(d, *months).map(BqValue::Date)
        }
        _ => Err(BqError::query("ADD_MONTHS expects DATE or DATETIME")),
    }
}

fn parse_weekday_name(s: &str) -> BqResult<Weekday> {
    match s.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Ok(Weekday::Sun),
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        _ => Err(BqError::semantic(format!("Invalid day of week: {s}"))),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "NEXT_DAY accepts DATE, DATETIME, or STRING"
)]
fn eval_next_day(args: &[BqValue]) -> BqResult<BqValue> {
    let [dt_val, dow_val] = args else {
        return Err(BqError::query("NEXT_DAY expects 2 arguments"));
    };
    if dt_val.is_null() || dow_val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(dow_str) = dow_val else {
        return Err(BqError::query("NEXT_DAY expects STRING day of week"));
    };
    let target_wd = parse_weekday_name(dow_str)?;
    let base_date = match dt_val {
        BqValue::Date(d) => *d,
        BqValue::Datetime(dt) => dt.date(),
        BqValue::String(_) => {
            let BqValue::Date(d) = coerce_date_arg(dt_val.clone())? else {
                return Err(BqError::query("NEXT_DAY expects DATE or DATETIME"));
            };
            d
        }
        _ => return Err(BqError::query("NEXT_DAY expects DATE or DATETIME")),
    };
    let cur = i64::from(base_date.weekday().num_days_from_sunday());
    let tgt = i64::from(target_wd.num_days_from_sunday());
    let delta = ((tgt - cur).rem_euclid(7) + 6) % 7 + 1;
    let out = base_date
        .checked_add_signed(Duration::days(delta))
        .ok_or_else(|| BqError::semantic("NEXT_DAY date overflow"))?;
    validate_bq_date(out).map(BqValue::Date)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "MONTHS_BETWEEN returns IEEE-754 FLOAT64 fractional month difference"
)]
fn eval_months_between(args: &[BqValue]) -> BqResult<BqValue> {
    let [a, b] = args else {
        return Err(BqError::query("MONTHS_BETWEEN expects 2 arguments"));
    };
    if a.is_null() || b.is_null() {
        return Ok(BqValue::Null);
    }
    let dt1 = to_naive_datetime_for_months(a)?;
    let dt2 = to_naive_datetime_for_months(b)?;
    let d1 = dt1.date();
    let d2 = dt2.date();
    let month_diff = f64::from(
        (d1.year() - d2.year()) * 12
            + (i32::try_from(d1.month()).unwrap_or(0) - i32::try_from(d2.month()).unwrap_or(0)),
    );
    if d1.day() == d2.day() || (is_last_day_of_month(d1) && is_last_day_of_month(d2)) {
        return Ok(BqValue::Float64(month_diff));
    }
    let day_diff =
        f64::from(i32::try_from(d1.day()).unwrap_or(0) - i32::try_from(d2.day()).unwrap_or(0));
    let t1_us = i64::from(dt1.time().num_seconds_from_midnight()) * 1_000_000
        + i64::from(dt1.time().nanosecond() / 1_000);
    let t2_us = i64::from(dt2.time().num_seconds_from_midnight()) * 1_000_000
        + i64::from(dt2.time().nanosecond() / 1_000);
    let time_frac_day = ((t1_us - t2_us) as f64) / 86_400_000_000.0;
    Ok(BqValue::Float64(
        month_diff + (day_diff + time_frac_day) / 31.0,
    ))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "MONTHS_BETWEEN accepts DATE, DATETIME, or STRING"
)]
fn to_naive_datetime_for_months(v: &BqValue) -> BqResult<NaiveDateTime> {
    match v {
        BqValue::Datetime(dt) => Ok(*dt),
        BqValue::Date(d) => Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default()),
        BqValue::String(_) => {
            let BqValue::Date(d) = coerce_date_arg(v.clone())? else {
                return Err(BqError::query("MONTHS_BETWEEN expects DATE or DATETIME"));
            };
            Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default())
        }
        _ => Err(BqError::query("MONTHS_BETWEEN expects DATE or DATETIME")),
    }
}

fn eval_date_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("DATE_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if matches!(args.first(), Some(BqValue::Datetime(_))) {
        return eval_datetime_bucket(args);
    }
    let d_val = coerce_date_arg(args.first().cloned().unwrap_or(BqValue::Null))?;
    let (BqValue::Date(d), Some(BqValue::Interval(w))) = (d_val, args.get(1)) else {
        return Err(BqError::query(
            "DATE_BUCKET expects (DATE, INTERVAL[, DATE])",
        ));
    };
    let origin = if let Some(v) = args.get(2) {
        let BqValue::Date(o) = coerce_date_arg(v.clone())? else {
            return Err(BqError::query("DATE_BUCKET origin must be DATE"));
        };
        o
    } else {
        NaiveDate::from_ymd_opt(1950, 1, 1).unwrap_or_default()
    };
    if w.micros != 0 || (w.months == 0 && w.days == 0) || (w.months != 0 && w.days != 0) {
        return Err(BqError::semantic(
            "DATE_BUCKET width must be a positive single-component month or day interval",
        ));
    }
    if w.days > 0 {
        let diff = (d - origin).num_days();
        let width = i64::from(w.days);
        let bucket_idx = diff.div_euclid(width);
        let target = origin + Duration::days(bucket_idx.saturating_mul(width));
        return validate_bq_date(target).map(BqValue::Date);
    }
    if w.months > 0 {
        let diff_m = (d.year() - origin.year()) * 12
            + (i32::try_from(d.month()).unwrap_or(0) - i32::try_from(origin.month()).unwrap_or(0));
        let mut idx = diff_m.div_euclid(w.months);
        let mut candidate =
            add_interval_to_date(origin, BqInterval::new(idx * w.months, 0, 0), true)?;
        if candidate > d {
            idx -= 1;
            candidate = add_interval_to_date(origin, BqInterval::new(idx * w.months, 0, 0), true)?;
        }
        return validate_bq_date(candidate).map(BqValue::Date);
    }
    Err(BqError::semantic("DATE_BUCKET width must be positive"))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "DATETIME_BUCKET accepts DATETIME, DATE, or STRING"
)]
fn coerce_to_datetime_val(v: &BqValue) -> BqResult<NaiveDateTime> {
    match v {
        BqValue::Datetime(dt) => Ok(*dt),
        BqValue::Date(d) => Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default()),
        BqValue::String(_) => {
            let BqValue::Datetime(dt) = coerce_datetime_arg(v.clone())? else {
                return Err(BqError::query("Expected DATETIME"));
            };
            Ok(dt)
        }
        _ => Err(BqError::query(
            "DATETIME_BUCKET expects (DATETIME, INTERVAL[, DATETIME])",
        )),
    }
}

fn eval_datetime_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("DATETIME_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if matches!(args.first(), Some(BqValue::Date(_)))
        && args.get(2).is_none_or(|v| matches!(v, BqValue::Date(_)))
    {
        return eval_date_bucket(args);
    }
    let dt = coerce_to_datetime_val(args.first().unwrap_or(&BqValue::Null))?;
    let Some(BqValue::Interval(w)) = args.get(1) else {
        return Err(BqError::query(
            "DATETIME_BUCKET expects (DATETIME, INTERVAL[, DATETIME])",
        ));
    };
    let origin = if let Some(v) = args.get(2) {
        coerce_to_datetime_val(v)?
    } else {
        NaiveDate::from_ymd_opt(1950, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default()
    };
    if w.months > 0 && w.days == 0 && w.micros == 0 {
        let diff_m = (dt.year() - origin.year()) * 12
            + (i32::try_from(dt.month()).unwrap_or(0) - i32::try_from(origin.month()).unwrap_or(0));
        let mut idx = diff_m.div_euclid(w.months);
        let mut cand_d =
            add_interval_to_date(origin.date(), BqInterval::new(idx * w.months, 0, 0), true)?;
        let mut cand = NaiveDateTime::new(cand_d, origin.time());
        if cand > dt {
            idx -= 1;
            cand_d =
                add_interval_to_date(origin.date(), BqInterval::new(idx * w.months, 0, 0), true)?;
            cand = NaiveDateTime::new(cand_d, origin.time());
        }
        return validate_bq_datetime(cand).map(BqValue::Datetime);
    }
    let width_us = i64::from(w.days)
        .saturating_mul(86_400_000_000)
        .saturating_add(w.micros);
    if w.months != 0 || width_us <= 0 {
        return Err(BqError::semantic(
            "DATETIME_BUCKET requires a positive interval width",
        ));
    }
    let diff_us = (dt - origin)
        .num_microseconds()
        .ok_or_else(|| BqError::semantic("DATETIME_BUCKET overflow"))?;
    let idx = diff_us.div_euclid(width_us);
    validate_bq_datetime(origin + Duration::microseconds(idx.saturating_mul(width_us)))
        .map(BqValue::Datetime)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "TIMESTAMP_BUCKET promotes DATE/DATETIME and coerces other inputs"
)]
fn eval_timestamp_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("TIMESTAMP_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    if matches!(args.first(), Some(BqValue::Date(_)))
        && args.get(2).is_none_or(|v| matches!(v, BqValue::Date(_)))
    {
        return eval_date_bucket(args);
    }
    if matches!(args.first(), Some(BqValue::Datetime(_)))
        && args
            .get(2)
            .is_none_or(|v| matches!(v, BqValue::Datetime(_) | BqValue::Date(_)))
    {
        return eval_datetime_bucket(args);
    }
    let ts_val = match args.first().unwrap_or(&BqValue::Null) {
        BqValue::Date(d) => BqValue::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(
            d.and_hms_opt(0, 0, 0).unwrap_or_default(),
            Utc,
        )),
        BqValue::Datetime(dt) => {
            BqValue::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(*dt, Utc))
        }
        other => coerce_timestamp_arg(other.clone())?,
    };
    let (BqValue::Timestamp(ts), Some(BqValue::Interval(w))) = (ts_val, args.get(1)) else {
        return Err(BqError::query(
            "TIMESTAMP_BUCKET expects (TIMESTAMP, INTERVAL[, TIMESTAMP])",
        ));
    };
    let origin = if let Some(v) = args.get(2) {
        let o_val = match v {
            BqValue::Date(d) => BqValue::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(
                d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                Utc,
            )),
            BqValue::Datetime(dt) => {
                BqValue::Timestamp(DateTime::<Utc>::from_naive_utc_and_offset(*dt, Utc))
            }
            other => coerce_timestamp_arg(other.clone())?,
        };
        let BqValue::Timestamp(o) = o_val else {
            return Err(BqError::query("TIMESTAMP_BUCKET origin must be TIMESTAMP"));
        };
        o
    } else {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default()
    };
    let width_us = i64::from(w.days)
        .saturating_mul(86_400_000_000)
        .saturating_add(w.micros);
    if w.months != 0 || width_us <= 0 {
        return Err(BqError::semantic(
            "TIMESTAMP_BUCKET requires a positive day/time interval width",
        ));
    }
    let diff_us = (ts - origin)
        .num_microseconds()
        .ok_or_else(|| BqError::semantic("TIMESTAMP_BUCKET overflow"))?;
    let idx = diff_us.div_euclid(width_us);
    Ok(BqValue::Timestamp(
        origin + Duration::microseconds(idx.saturating_mul(width_us)),
    ))
}

fn eval_time_add_sub(args: &[BqValue], add: bool) -> BqResult<BqValue> {
    let [t_val, iv_val] = args else {
        return Err(BqError::query("TIME_ADD/SUB expects 2 arguments"));
    };
    if t_val.is_null() || iv_val.is_null() {
        return Ok(BqValue::Null);
    }
    let t_val = coerce_time_arg(t_val.clone())?;
    let (BqValue::Time(t), BqValue::Interval(iv)) = (t_val, iv_val) else {
        return Err(BqError::query("TIME_ADD/SUB expects (TIME, INTERVAL)"));
    };
    let delta = if add { iv.micros } else { -iv.micros };
    let (res, _) = t.overflowing_add_signed(Duration::microseconds(delta));
    Ok(BqValue::Time(res))
}

fn eval_time_trunc(args: &[BqValue]) -> BqResult<BqValue> {
    let [t_val, part_val] = args else {
        return Err(BqError::query("TIME_TRUNC expects 2 arguments"));
    };
    if t_val.is_null() || part_val.is_null() {
        return Ok(BqValue::Null);
    }
    let t_val = coerce_time_arg(t_val.clone())?;
    let (BqValue::Time(t), BqValue::String(p)) = (t_val, part_val) else {
        return Err(BqError::query("TIME_TRUNC expects (TIME, PART)"));
    };
    let res = match p.to_ascii_uppercase().as_str() {
        "HOUR" => NaiveTime::from_hms_opt(t.hour(), 0, 0),
        "MINUTE" => NaiveTime::from_hms_opt(t.hour(), t.minute(), 0),
        "SECOND" => NaiveTime::from_hms_opt(t.hour(), t.minute(), t.second()),
        "MILLISECOND" => NaiveTime::from_hms_milli_opt(
            t.hour(),
            t.minute(),
            t.second(),
            t.nanosecond() / 1_000_000,
        ),
        "MICROSECOND" => {
            NaiveTime::from_hms_micro_opt(t.hour(), t.minute(), t.second(), t.nanosecond() / 1_000)
        }
        _ => return Err(BqError::query(format!("Unsupported TIME_TRUNC part '{p}'"))),
    }
    .ok_or_else(|| BqError::semantic("Invalid TIME_TRUNC result"))?;
    Ok(BqValue::Time(res))
}

fn eval_time_diff(args: &[BqValue]) -> BqResult<BqValue> {
    let [a, b, part] = args else {
        return Err(BqError::query("TIME_DIFF expects 3 arguments"));
    };
    if a.is_null() || b.is_null() || part.is_null() {
        return Ok(BqValue::Null);
    }
    let (a, b) = (coerce_time_arg(a.clone())?, coerce_time_arg(b.clone())?);
    let (BqValue::Time(ta), BqValue::Time(tb), BqValue::String(p)) = (a, b, part) else {
        return Err(BqError::query("TIME_DIFF expects (TIME, TIME, PART)"));
    };
    let diff = ta - tb;
    let out = match p.to_ascii_uppercase().as_str() {
        "HOUR" => diff.num_hours(),
        "MINUTE" => diff.num_minutes(),
        "SECOND" => diff.num_seconds(),
        "MILLISECOND" => diff.num_milliseconds(),
        "MICROSECOND" => diff.num_microseconds().unwrap_or(0),
        _ => return Err(BqError::query(format!("Unsupported TIME_DIFF part '{p}'"))),
    };
    Ok(BqValue::Int64(out))
}
