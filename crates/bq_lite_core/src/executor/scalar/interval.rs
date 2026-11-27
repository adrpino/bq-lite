use crate::ast::BinaryOp;
use crate::error::{BqError, BqResult};
use crate::types::{validate_bq_datetime, validate_interval, BqInterval, BqValue};

const DAY_MICROS: i128 = 86_400_000_000;

pub(super) const INTERVAL_FUNCTIONS: &[&str] = &[
    "JUSTIFY_DAYS",
    "JUSTIFY_HOURS",
    "JUSTIFY_INTERVAL",
    "MAKE_INTERVAL",
];

pub(super) fn eval_interval_fn(name: &str, args: &[BqValue]) -> BqResult<Option<BqValue>> {
    match name {
        "MAKE_INTERVAL" => eval_make_interval(args).map(Some),
        "JUSTIFY_DAYS" => eval_justify_days(args).map(Some),
        "JUSTIFY_HOURS" => eval_justify_hours(args).map(Some),
        "JUSTIFY_INTERVAL" => eval_justify_interval(args).map(Some),
        _ => Ok(None),
    }
}

pub(in crate::executor) fn interval_neg(iv: BqInterval) -> BqResult<BqInterval> {
    validate_interval(
        -i128::from(iv.months),
        -i128::from(iv.days),
        -i128::from(iv.micros),
    )
}

pub(in crate::executor) fn interval_add(a: BqInterval, b: BqInterval) -> BqResult<BqInterval> {
    validate_interval(
        i128::from(a.months) + i128::from(b.months),
        i128::from(a.days) + i128::from(b.days),
        i128::from(a.micros) + i128::from(b.micros),
    )
}

pub(in crate::executor) fn interval_sub(a: BqInterval, b: BqInterval) -> BqResult<BqInterval> {
    validate_interval(
        i128::from(a.months) - i128::from(b.months),
        i128::from(a.days) - i128::from(b.days),
        i128::from(a.micros) - i128::from(b.micros),
    )
}

pub(in crate::executor) fn interval_mul_i64(iv: BqInterval, k: i64) -> BqResult<BqInterval> {
    let k128 = i128::from(k);
    let m = i128::from(iv.months)
        .checked_mul(k128)
        .ok_or_else(|| BqError::semantic("INTERVAL multiplication overflow"))?;
    let d = i128::from(iv.days)
        .checked_mul(k128)
        .ok_or_else(|| BqError::semantic("INTERVAL multiplication overflow"))?;
    let us = i128::from(iv.micros)
        .checked_mul(k128)
        .ok_or_else(|| BqError::semantic("INTERVAL multiplication overflow"))?;
    validate_interval(m, d, us)
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "GoogleSQL INTERVAL * FLOAT64 propagates fractional months -> days -> micros via IEEE-754"
)]
pub(in crate::executor) fn interval_mul_f64(iv: BqInterval, k: f64) -> BqResult<BqInterval> {
    if !k.is_finite() {
        return Err(BqError::semantic(
            "INTERVAL multiplication by NaN or Infinity is not allowed",
        ));
    }
    let m_f = f64::from(iv.months) * k;
    let m_whole = m_f.trunc();
    let m_frac = m_f - m_whole;

    let d_f = m_frac.mul_add(30.0, f64::from(iv.days) * k);
    let d_whole = d_f.trunc();
    let d_frac = d_f - d_whole;

    let us_f = d_frac.mul_add(DAY_MICROS as f64, (iv.micros as f64) * k);
    let us_whole = us_f.trunc();

    validate_interval(m_whole as i128, d_whole as i128, us_whole as i128)
}

pub(in crate::executor) fn interval_div_i64(iv: BqInterval, divisor: i64) -> BqResult<BqInterval> {
    if divisor == 0 {
        return Err(BqError::semantic("division by zero"));
    }
    interval_div_i128(
        i128::from(iv.months),
        i128::from(iv.days),
        i128::from(iv.micros),
        i128::from(divisor),
    )
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "GoogleSQL INTERVAL / FLOAT64 divides components with month->day->micro carry"
)]
pub(in crate::executor) fn interval_div_f64(iv: BqInterval, divisor: f64) -> BqResult<BqInterval> {
    if !divisor.is_finite() || divisor == 0.0 {
        return Err(BqError::semantic("Invalid INTERVAL divisor"));
    }
    let m_f = f64::from(iv.months) / divisor;
    let m_whole = m_f.trunc();
    let m_frac = m_f - m_whole;

    let d_f = m_frac.mul_add(30.0, f64::from(iv.days) / divisor);
    let d_whole = d_f.trunc();
    let d_frac = d_f - d_whole;

    let us_f = d_frac.mul_add(DAY_MICROS as f64, (iv.micros as f64) / divisor);
    validate_interval(m_whole as i128, d_whole as i128, us_f.trunc() as i128)
}

fn interval_div_i128(
    months: i128,
    days: i128,
    micros: i128,
    divisor: i128,
) -> BqResult<BqInterval> {
    let m_q = months / divisor;
    let m_r = months % divisor;
    let days_total = days + m_r * 30;
    let d_q = days_total / divisor;
    let d_r = days_total % divisor;
    let us_total = micros + d_r * DAY_MICROS;
    let us_q = us_total / divisor;
    validate_interval(m_q, d_q, us_q)
}

/// Sums a non-empty list of `INTERVAL` values in `i128` to avoid intermediate overflow.
pub(in crate::executor) fn sum_intervals(vals: &[BqValue]) -> BqResult<BqValue> {
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;
    for v in vals {
        let BqValue::Interval(iv) = v else {
            return Err(BqError::query("SUM(INTERVAL) received non-INTERVAL value"));
        };
        months += i128::from(iv.months);
        days += i128::from(iv.days);
        micros += i128::from(iv.micros);
    }
    validate_interval(months, days, micros).map(BqValue::Interval)
}

/// Averages a non-empty list of `INTERVAL` values in `i128` with month -> day -> micro carry.
pub(in crate::executor) fn avg_intervals(vals: &[BqValue]) -> BqResult<BqValue> {
    let count = i128::try_from(vals.len()).unwrap_or(1);
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;
    for v in vals {
        let BqValue::Interval(iv) = v else {
            return Err(BqError::query("AVG(INTERVAL) received non-INTERVAL value"));
        };
        months += i128::from(iv.months);
        days += i128::from(iv.days);
        micros += i128::from(iv.micros);
    }
    interval_div_i128(months, days, micros, count).map(BqValue::Interval)
}

pub(in crate::executor) fn extract_interval_part(
    upper: &str,
    raw_part: &str,
    iv: BqInterval,
) -> BqResult<BqValue> {
    let val = match upper {
        "YEAR" => i64::from(iv.months / 12),
        "MONTH" => i64::from(iv.months % 12),
        "DAY" => i64::from(iv.days),
        "HOUR" => iv.micros / 3_600_000_000,
        "MINUTE" => (iv.micros % 3_600_000_000) / 60_000_000,
        "SECOND" => (iv.micros % 60_000_000) / 1_000_000,
        "MILLISECOND" => (iv.micros % 1_000_000) / 1_000,
        "MICROSECOND" => iv.micros % 1_000_000,
        _ => {
            return Err(BqError::query(format!(
                "Unsupported EXTRACT part '{raw_part}' for INTERVAL"
            )))
        }
    };
    Ok(BqValue::Int64(val))
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
    let months = i128::from(nums[0]) * 12 + i128::from(nums[1]);
    let days = i128::from(nums[2]);
    let micros = i128::from(nums[3]) * 3_600_000_000
        + i128::from(nums[4]) * 60_000_000
        + i128::from(nums[5]) * 1_000_000;
    validate_interval(months, days, micros).map(BqValue::Interval)
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
    let mut months = i128::from(iv.months) + i128::from(iv.days) / 30;
    let mut rem_days = i128::from(iv.days) % 30;
    if months > 0 && rem_days < 0 {
        months -= 1;
        rem_days += 30;
    } else if months < 0 && rem_days > 0 {
        months += 1;
        rem_days -= 30;
    }
    validate_interval(months, rem_days, i128::from(iv.micros)).map(BqValue::Interval)
}

fn eval_justify_hours(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("JUSTIFY_HOURS expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Interval(iv) = arg else {
        return Err(BqError::query("JUSTIFY_HOURS expects INTERVAL"));
    };
    let mut days = i128::from(iv.days) + i128::from(iv.micros) / DAY_MICROS;
    let mut rem_micros = i128::from(iv.micros) % DAY_MICROS;
    if days > 0 && rem_micros < 0 {
        days -= 1;
        rem_micros += DAY_MICROS;
    } else if days < 0 && rem_micros > 0 {
        days += 1;
        rem_micros -= DAY_MICROS;
    }
    validate_interval(i128::from(iv.months), days, rem_micros).map(BqValue::Interval)
}

fn eval_justify_interval(args: &[BqValue]) -> BqResult<BqValue> {
    let [arg] = args else {
        return Err(BqError::query("JUSTIFY_INTERVAL expects 1 argument"));
    };
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Interval(iv) = arg else {
        return Err(BqError::query("JUSTIFY_INTERVAL expects INTERVAL"));
    };
    let mut days = i128::from(iv.days) + i128::from(iv.micros) / DAY_MICROS;
    let mut micros = i128::from(iv.micros) % DAY_MICROS;
    let mut months = i128::from(iv.months) + days / 30;
    days %= 30;
    let total = (months * 30 + days) * DAY_MICROS + micros;
    if total > 0 {
        if micros < 0 {
            days -= 1;
            micros += DAY_MICROS;
        }
        if days < 0 {
            months -= 1;
            days += 30;
        }
    } else if total < 0 {
        if micros > 0 {
            days += 1;
            micros -= DAY_MICROS;
        }
        if days > 0 {
            months += 1;
            days -= 30;
        }
    }
    validate_interval(months, days, micros).map(BqValue::Interval)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Add/Sub/Mul are valid temporal or interval operators here"
)]
pub(in crate::executor) fn eval_temporal_interval_arith(
    op: BinaryOp,
    left: &BqValue,
    right: &BqValue,
) -> BqResult<BqValue> {
    match (op, left, right) {
        (BinaryOp::Add, BqValue::Interval(a), BqValue::Interval(b)) => {
            return interval_add(*a, *b).map(BqValue::Interval);
        }
        (BinaryOp::Sub, BqValue::Interval(a), BqValue::Interval(b)) => {
            return interval_sub(*a, *b).map(BqValue::Interval);
        }
        (BinaryOp::Mul, BqValue::Interval(iv), BqValue::Int64(k))
        | (BinaryOp::Mul, BqValue::Int64(k), BqValue::Interval(iv)) => {
            return interval_mul_i64(*iv, *k).map(BqValue::Interval);
        }
        (BinaryOp::Mul, BqValue::Interval(iv), BqValue::Float64(k))
        | (BinaryOp::Mul, BqValue::Float64(k), BqValue::Interval(iv)) => {
            return interval_mul_f64(*iv, *k).map(BqValue::Interval);
        }
        _ => {}
    }
    let add = match op {
        BinaryOp::Add => true,
        BinaryOp::Sub => false,
        _ => return Err(BqError::query("Invalid temporal interval operator")),
    };
    let (base, iv) = match (left, right) {
        (BqValue::Interval(iv), other) if add => (other, *iv),
        (other, BqValue::Interval(iv)) => (other, *iv),
        _ => {
            return Err(BqError::query(format!(
                "Unsupported arithmetic between {} and {}",
                left.infer_type()?,
                right.infer_type()?
            )))
        }
    };
    match base {
        BqValue::Date(d) => {
            let shifted_d = super::datetime_calc::add_interval_to_date(*d, iv, add)?;
            if iv.micros != 0 {
                let dt = shifted_d.and_hms_opt(0, 0, 0).unwrap_or_default();
                let micros = if add { iv.micros } else { -iv.micros };
                let out = dt + chrono::Duration::microseconds(micros);
                validate_bq_datetime(out).map(BqValue::Datetime)
            } else {
                Ok(BqValue::Date(shifted_d))
            }
        }
        BqValue::Datetime(dt) => {
            let d = super::datetime_calc::add_interval_to_date(dt.date(), iv, add)?;
            let base_dt = chrono::NaiveDateTime::new(d, dt.time());
            let micros = if add { iv.micros } else { -iv.micros };
            let out = base_dt + chrono::Duration::microseconds(micros);
            validate_bq_datetime(out).map(BqValue::Datetime)
        }
        BqValue::Timestamp(ts) => {
            let sign = if add { 1_i64 } else { -1_i64 };
            let delta = chrono::Duration::days(i64::from(iv.days) * sign)
                + chrono::Duration::microseconds(iv.micros * sign);
            Ok(BqValue::Timestamp(*ts + delta))
        }
        _ => Err(BqError::query(format!(
            "Unsupported arithmetic between {} and {}",
            left.infer_type()?,
            right.infer_type()?
        ))),
    }
}
