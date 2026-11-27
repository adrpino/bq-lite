use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc,
};
use chrono_tz::Tz;

use crate::error::{BqError, BqResult};
use crate::types::{BqInterval, BqValue};

use super::datetime_calc::{
    add_interval_to_date, coerce_date_arg, coerce_datetime_arg, coerce_time_arg,
    coerce_timestamp_arg,
};

pub(super) const DATETIME_EXTRA_FUNCTIONS: &[&str] = &[
    "DATE_BUCKET",
    "DATETIME_BUCKET",
    "JUSTIFY_DAYS",
    "JUSTIFY_HOURS",
    "JUSTIFY_INTERVAL",
    "MAKE_INTERVAL",
    "TIME_ADD",
    "TIME_DIFF",
    "TIME_SUB",
    "TIME_TRUNC",
    "TIMESTAMP_BUCKET",
];

pub(super) fn eval_datetime_extra_fn(name: &str, args: &[BqValue]) -> BqResult<Option<BqValue>> {
    match name {
        "MAKE_INTERVAL" => eval_make_interval(args).map(Some),
        "JUSTIFY_DAYS" => eval_justify_days(args).map(Some),
        "JUSTIFY_HOURS" => eval_justify_hours(args).map(Some),
        "JUSTIFY_INTERVAL" => eval_justify_interval(args).map(Some),
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

/// Converts a UTC `DateTime` to wall-clock `NaiveDateTime` in a `BigQuery` timezone string
/// (IANA name like `'America/New_York'` or fixed offset like `'+05:30'` / `'-05'`).
pub(in crate::executor) fn timestamp_to_tz_naive(
    ts: DateTime<Utc>,
    tz_str: &str,
) -> BqResult<NaiveDateTime> {
    let trimmed = tz_str.trim();
    if trimmed.eq_ignore_ascii_case("UTC") || trimmed == "Z" {
        return Ok(ts.naive_utc());
    }
    if let Ok(tz) = trimmed.parse::<Tz>() {
        return Ok(ts.with_timezone(&tz).naive_local());
    }
    if let Some(off) = parse_fixed_tz_offset(trimmed) {
        return Ok(ts.with_timezone(&off).naive_local());
    }
    Err(BqError::semantic(format!("Invalid time zone: '{tz_str}'")))
}

fn parse_fixed_tz_offset(s: &str) -> Option<FixedOffset> {
    let sign = if s.starts_with('+') {
        1_i32
    } else if s.starts_with('-') {
        -1_i32
    } else {
        return None;
    };
    let rest = s.get(1..)?;
    let (h, m) = if let Some((hs, ms)) = rest.split_once(':') {
        (hs.parse::<i32>().ok()?, ms.parse::<i32>().ok()?)
    } else {
        (rest.parse::<i32>().ok()?, 0_i32)
    };
    if !(0..=14).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    let total = sign * (h * 3600 + m * 60);
    if sign >= 0 {
        FixedOffset::east_opt(total)
    } else {
        FixedOffset::west_opt(-total)
    }
}

/// Parses a `BigQuery` compound `INTERVAL` string literal (e.g. `INTERVAL '1-2 3 4:5:6' YEAR TO SECOND`).
pub(in crate::executor) fn parse_compound_interval(
    raw: &str,
    start_unit: &str,
    end_unit: Option<&str>,
) -> BqResult<BqInterval> {
    let trimmed = raw.trim();
    let su = start_unit.to_ascii_uppercase();
    let eu = end_unit.map(str::to_ascii_uppercase);
    let Some(end) = eu.as_deref() else {
        let n = trimmed
            .parse::<i64>()
            .map_err(|_| BqError::semantic(format!("Invalid INTERVAL literal '{raw}'")))?;
        return single_unit_interval(n, &su);
    };
    let (sign, body) = trimmed.strip_prefix('-').map_or_else(
        || (1_i64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
        |b| (-1_i64, b),
    );
    let mut months: i32 = 0;
    let mut days: i32 = 0;
    let mut micros: i64 = 0;
    match (su.as_str(), end) {
        ("YEAR", "MONTH") => {
            let (y, m) = split_pair_i32(body, '-', raw)?;
            months = y.saturating_mul(12).saturating_add(m);
        }
        ("YEAR", "DAY") => {
            let parts: Vec<&str> = body.split_whitespace().collect();
            let [ym, d_str] = parts.as_slice() else {
                return Err(BqError::semantic(format!(
                    "Invalid YEAR TO DAY interval '{raw}'"
                )));
            };
            let (y, m) = split_pair_i32(ym, '-', raw)?;
            months = y.saturating_mul(12).saturating_add(m);
            days = parse_i32(d_str, raw)?;
        }
        ("YEAR", "HOUR" | "MINUTE" | "SECOND") => {
            let parts: Vec<&str> = body.split_whitespace().collect();
            let [ym, d_str, hms] = parts.as_slice() else {
                return Err(BqError::semantic(format!(
                    "Invalid compound interval '{raw}'"
                )));
            };
            let (y, m) = split_pair_i32(ym, '-', raw)?;
            months = y.saturating_mul(12).saturating_add(m);
            days = parse_i32(d_str, raw)?;
            micros = parse_hms_micros(hms, "HOUR", end, raw)?;
        }
        ("DAY", "HOUR" | "MINUTE" | "SECOND") => {
            let parts: Vec<&str> = body.split_whitespace().collect();
            let [d_str, hms] = parts.as_slice() else {
                return Err(BqError::semantic(format!(
                    "Invalid DAY TO {end} interval '{raw}'"
                )));
            };
            days = parse_i32(d_str, raw)?;
            micros = parse_hms_micros(hms, "HOUR", end, raw)?;
        }
        ("HOUR", "MINUTE" | "SECOND") | ("MINUTE", "SECOND") => {
            micros = parse_hms_micros(body, &su, end, raw)?;
        }
        _ => {
            return Err(BqError::query(format!(
                "Unsupported INTERVAL range '{start_unit} TO {end}'"
            )))
        }
    }
    let s32 = i32::try_from(sign).unwrap_or(1);
    Ok(BqInterval::new(
        months.saturating_mul(s32),
        days.saturating_mul(s32),
        micros.saturating_mul(sign),
    ))
}

pub(in crate::executor) fn single_unit_interval(n: i64, upper_unit: &str) -> BqResult<BqInterval> {
    let n32 = i32::try_from(n).map_err(|_| BqError::semantic("INTERVAL quantity out of range"))?;
    match upper_unit {
        "YEAR" => Ok(BqInterval::new(n32.saturating_mul(12), 0, 0)),
        "QUARTER" => Ok(BqInterval::new(n32.saturating_mul(3), 0, 0)),
        "MONTH" => Ok(BqInterval::new(n32, 0, 0)),
        "WEEK" => Ok(BqInterval::new(0, n32.saturating_mul(7), 0)),
        "DAY" => Ok(BqInterval::new(0, n32, 0)),
        "HOUR" => Ok(BqInterval::new(0, 0, n.saturating_mul(3_600_000_000))),
        "MINUTE" => Ok(BqInterval::new(0, 0, n.saturating_mul(60_000_000))),
        "SECOND" => Ok(BqInterval::new(0, 0, n.saturating_mul(1_000_000))),
        "MILLISECOND" => Ok(BqInterval::new(0, 0, n.saturating_mul(1_000))),
        "MICROSECOND" => Ok(BqInterval::new(0, 0, n)),
        _ => Err(BqError::query(format!(
            "Unsupported INTERVAL unit '{upper_unit}'"
        ))),
    }
}

fn split_pair_i32(s: &str, sep: char, raw: &str) -> BqResult<(i32, i32)> {
    let (a, b) = s
        .split_once(sep)
        .ok_or_else(|| BqError::semantic(format!("Invalid INTERVAL literal '{raw}'")))?;
    Ok((parse_i32(a, raw)?, parse_i32(b, raw)?))
}

fn parse_i32(s: &str, raw: &str) -> BqResult<i32> {
    s.parse::<i32>()
        .map_err(|_| BqError::semantic(format!("Invalid INTERVAL literal '{raw}'")))
}

fn parse_hms_micros(s: &str, start: &str, end: &str, raw: &str) -> BqResult<i64> {
    let parts: Vec<&str> = s.split(':').collect();
    match (start, end, parts.as_slice()) {
        ("HOUR", "HOUR", [h]) => Ok(parse_i64(h, raw)?.saturating_mul(3_600_000_000)),
        ("HOUR", "MINUTE", [h, m]) => Ok(parse_i64(h, raw)?
            .saturating_mul(3_600_000_000)
            .saturating_add(parse_i64(m, raw)?.saturating_mul(60_000_000))),
        ("HOUR", "SECOND", [h, m, sec]) => Ok(parse_i64(h, raw)?
            .saturating_mul(3_600_000_000)
            .saturating_add(parse_i64(m, raw)?.saturating_mul(60_000_000))
            .saturating_add(parse_sec_micros(sec, raw)?)),
        ("MINUTE", "SECOND", [m, sec]) => Ok(parse_i64(m, raw)?
            .saturating_mul(60_000_000)
            .saturating_add(parse_sec_micros(sec, raw)?)),
        _ => Err(BqError::semantic(format!(
            "Invalid INTERVAL literal '{raw}'"
        ))),
    }
}

fn parse_i64(s: &str, raw: &str) -> BqResult<i64> {
    s.parse::<i64>()
        .map_err(|_| BqError::semantic(format!("Invalid INTERVAL literal '{raw}'")))
}

fn parse_sec_micros(s: &str, raw: &str) -> BqResult<i64> {
    let (whole_s, frac_s) = s.split_once('.').unwrap_or((s, ""));
    let secs = parse_i64(whole_s, raw)?;
    if frac_s.is_empty() {
        return Ok(secs.saturating_mul(1_000_000));
    }
    let mut padded = frac_s.chars().take(6).collect::<String>();
    while padded.len() < 6 {
        padded.push('0');
    }
    let frac = parse_i64(&padded, raw)?;
    Ok(secs.saturating_mul(1_000_000).saturating_add(frac))
}

fn eval_make_interval(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() > 6 {
        return Err(BqError::query("MAKE_INTERVAL expects at most 6 arguments"));
    }
    let mut nums = [0_i64; 6];
    for (idx, v) in args.iter().enumerate() {
        if v.is_null() {
            return Ok(BqValue::Null);
        }
        let BqValue::Int64(n) = v else {
            return Err(BqError::query("MAKE_INTERVAL arguments must be INT64"));
        };
        if let Some(slot) = nums.get_mut(idx) {
            *slot = *n;
        }
    }
    let months = i32::try_from(nums[0].saturating_mul(12).saturating_add(nums[1]))
        .map_err(|_| BqError::semantic("MAKE_INTERVAL months out of range"))?;
    let days =
        i32::try_from(nums[2]).map_err(|_| BqError::semantic("MAKE_INTERVAL days out of range"))?;
    let micros = nums[3]
        .saturating_mul(3_600_000_000)
        .saturating_add(nums[4].saturating_mul(60_000_000))
        .saturating_add(nums[5].saturating_mul(1_000_000));
    Ok(BqValue::Interval(BqInterval::new(months, days, micros)))
}

fn eval_justify_days(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("JUSTIFY_DAYS expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Interval(iv) = arg else {
        return Err(BqError::query("JUSTIFY_DAYS expects INTERVAL"));
    };
    let extra_months = iv.days / 30;
    let rem_days = iv.days % 30;
    Ok(BqValue::Interval(BqInterval::new(
        iv.months.saturating_add(extra_months),
        rem_days,
        iv.micros,
    )))
}

fn eval_justify_hours(args: &[BqValue]) -> BqResult<BqValue> {
    const DAY_MICROS: i64 = 86_400_000_000;
    let [arg] = args else {
        return Err(BqError::query("JUSTIFY_HOURS expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Interval(iv) = arg else {
        return Err(BqError::query("JUSTIFY_HOURS expects INTERVAL"));
    };
    let extra_days = i32::try_from(iv.micros / DAY_MICROS).unwrap_or(0);
    let rem_micros = iv.micros % DAY_MICROS;
    Ok(BqValue::Interval(BqInterval::new(
        iv.months,
        iv.days.saturating_add(extra_days),
        rem_micros,
    )))
}

fn eval_justify_interval(args: &[BqValue]) -> BqResult<BqValue> {
    let step1 = eval_justify_hours(args)?;
    eval_justify_days(&[step1])
}

fn eval_date_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("DATE_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
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
        return Ok(BqValue::Date(target));
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
        return Ok(BqValue::Date(candidate));
    }
    Err(BqError::semantic("DATE_BUCKET width must be positive"))
}

fn eval_datetime_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("DATETIME_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let dt_val = coerce_datetime_arg(args.first().cloned().unwrap_or(BqValue::Null))?;
    let (BqValue::Datetime(dt), Some(BqValue::Interval(w))) = (dt_val, args.get(1)) else {
        return Err(BqError::query(
            "DATETIME_BUCKET expects (DATETIME, INTERVAL[, DATETIME])",
        ));
    };
    let origin = if let Some(v) = args.get(2) {
        let BqValue::Datetime(o) = coerce_datetime_arg(v.clone())? else {
            return Err(BqError::query("DATETIME_BUCKET origin must be DATETIME"));
        };
        o
    } else {
        NaiveDate::from_ymd_opt(1950, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .unwrap_or_default()
    };
    let width_us = i64::from(w.days)
        .saturating_mul(86_400_000_000)
        .saturating_add(w.micros);
    if w.months != 0 || width_us <= 0 {
        return Err(BqError::semantic(
            "DATETIME_BUCKET requires a positive day/time interval width",
        ));
    }
    let diff_us = (dt - origin)
        .num_microseconds()
        .ok_or_else(|| BqError::semantic("DATETIME_BUCKET overflow"))?;
    let idx = diff_us.div_euclid(width_us);
    Ok(BqValue::Datetime(
        origin + Duration::microseconds(idx.saturating_mul(width_us)),
    ))
}

fn eval_timestamp_bucket(args: &[BqValue]) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("TIMESTAMP_BUCKET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let ts_val = coerce_timestamp_arg(args.first().cloned().unwrap_or(BqValue::Null))?;
    let (BqValue::Timestamp(ts), Some(BqValue::Interval(w))) = (ts_val, args.get(1)) else {
        return Err(BqError::query(
            "TIMESTAMP_BUCKET expects (TIMESTAMP, INTERVAL[, TIMESTAMP])",
        ));
    };
    let origin = if let Some(v) = args.get(2) {
        let BqValue::Timestamp(o) = coerce_timestamp_arg(v.clone())? else {
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
