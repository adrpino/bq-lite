use chrono::Duration;

use crate::error::{BqError, BqResult};
use crate::executor::evaluator::{values_distinct_eq, values_equal};
use crate::types::BqValue;

use super::datetime_calc::coerce_timestamp_arg;
use super::interval::avg_intervals;

pub(super) const ARRAY_EXTRA_FUNCTIONS: &[&str] = &[
    "ARRAY_AVG",
    "ARRAY_DISTINCT",
    "ARRAY_FIND",
    "ARRAY_FIRST_N",
    "ARRAY_INCLUDES",
    "ARRAY_INCLUDES_ALL",
    "ARRAY_INCLUDES_ANY",
    "ARRAY_IS_DISTINCT",
    "ARRAY_LAST_N",
    "ARRAY_OFFSET",
    "DEGREES",
    "GENERATE_TIMESTAMP_ARRAY",
    "RADIANS",
];

pub(super) fn eval_array_extra_fn(name: &str, args: Vec<BqValue>) -> BqResult<Option<BqValue>> {
    match name {
        "ARRAY_IS_DISTINCT" => eval_array_is_distinct(args).map(Some),
        "ARRAY_DISTINCT" => eval_array_distinct(args).map(Some),
        "ARRAY_INCLUDES" => eval_array_includes(args).map(Some),
        "ARRAY_INCLUDES_ANY" => eval_array_includes_any_all(args, false).map(Some),
        "ARRAY_INCLUDES_ALL" => eval_array_includes_any_all(args, true).map(Some),
        "ARRAY_FIRST_N" => eval_array_first_last_n(args, true).map(Some),
        "ARRAY_LAST_N" => eval_array_first_last_n(args, false).map(Some),
        "ARRAY_FIND" => eval_array_find_or_offset(args, false).map(Some),
        "ARRAY_OFFSET" => eval_array_find_or_offset(args, true).map(Some),
        "ARRAY_AVG" => eval_array_avg(args).map(Some),
        "DEGREES" => eval_angle_conv(args, true).map(Some),
        "RADIANS" => eval_angle_conv(args, false).map(Some),
        "GENERATE_TIMESTAMP_ARRAY" => eval_generate_timestamp_array(args).map(Some),
        _ => Ok(None),
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::wildcard_enum_match_arm,
    reason = "DEGREES/RADIANS return IEEE-754 FLOAT64 for numeric variants"
)]
fn eval_angle_conv(args: Vec<BqValue>, to_degrees: bool) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("DEGREES/RADIANS expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let f = match arg {
        BqValue::Int64(n) => n as f64,
        BqValue::Float64(x) => x,
        BqValue::Numeric(n) => n.to_f64(),
        BqValue::BigNumeric(bn) => bn.to_f64(),
        other => {
            return Err(BqError::query(format!(
                "DEGREES/RADIANS expects numeric input, got {}",
                other.infer_type()?
            )))
        }
    };
    let out = if to_degrees {
        f.to_degrees()
    } else {
        f.to_radians()
    };
    Ok(BqValue::Float64(out))
}

fn eval_array_is_distinct(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_IS_DISTINCT expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arg else {
        return Err(BqError::query("ARRAY_IS_DISTINCT expects ARRAY"));
    };
    for (i, a) in items.iter().enumerate() {
        for b in items.get(i + 1..).unwrap_or(&[]) {
            if values_distinct_eq(a, b)? {
                return Ok(BqValue::Bool(false));
            }
        }
    }
    Ok(BqValue::Bool(true))
}

fn eval_array_distinct(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_DISTINCT expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arg else {
        return Err(BqError::query("ARRAY_DISTINCT expects ARRAY"));
    };
    let mut out: Vec<BqValue> = Vec::with_capacity(items.len());
    for item in items {
        let mut seen = false;
        for existing in &out {
            if values_distinct_eq(existing, &item)? {
                seen = true;
                break;
            }
        }
        if !seen {
            out.push(item);
        }
    }
    Ok(BqValue::Array(out))
}

fn eval_array_includes(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arr_v, elem]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_INCLUDES expects 2 arguments"))?;
    if arr_v.is_null() || elem.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arr_v else {
        return Err(BqError::query(
            "ARRAY_INCLUDES expects ARRAY first argument",
        ));
    };
    for item in &items {
        if !item.is_null() && values_equal(item, &elem)? {
            return Ok(BqValue::Bool(true));
        }
    }
    Ok(BqValue::Bool(false))
}

fn eval_array_includes_any_all(args: Vec<BqValue>, all: bool) -> BqResult<BqValue> {
    let [arr_v, search_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_INCLUDES_ANY/ALL expects 2 arguments"))?;
    if arr_v.is_null() || search_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Array(items), BqValue::Array(search)) = (arr_v, search_v) else {
        return Err(BqError::query(
            "ARRAY_INCLUDES_ANY/ALL expects (ARRAY, ARRAY)",
        ));
    };
    if all {
        for s in &search {
            if s.is_null() {
                return Ok(BqValue::Bool(false));
            }
            let mut found = false;
            for item in &items {
                if !item.is_null() && values_equal(item, s)? {
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(BqValue::Bool(false));
            }
        }
        Ok(BqValue::Bool(true))
    } else {
        for s in &search {
            if s.is_null() {
                continue;
            }
            for item in &items {
                if !item.is_null() && values_equal(item, s)? {
                    return Ok(BqValue::Bool(true));
                }
            }
        }
        Ok(BqValue::Bool(false))
    }
}

fn eval_array_first_last_n(args: Vec<BqValue>, first: bool) -> BqResult<BqValue> {
    let [arr_v, n_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_FIRST_N/LAST_N expects 2 arguments"))?;
    if arr_v.is_null() || n_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Array(items), BqValue::Int64(n)) = (arr_v, n_v) else {
        return Err(BqError::query(
            "ARRAY_FIRST_N/LAST_N expects (ARRAY, INT64)",
        ));
    };
    if n < 0 {
        return Err(BqError::semantic(
            "ARRAY_FIRST_N/LAST_N count cannot be negative",
        ));
    }
    let k = usize::try_from(n).unwrap_or(usize::MAX).min(items.len());
    if first {
        Ok(BqValue::Array(items.into_iter().take(k).collect()))
    } else {
        let start = items.len().saturating_sub(k);
        Ok(BqValue::Array(items.into_iter().skip(start).collect()))
    }
}

fn eval_array_find_or_offset(args: Vec<BqValue>, return_offset: bool) -> BqResult<BqValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(BqError::query("ARRAY_FIND/OFFSET expects 2 or 3 arguments"));
    }
    if args.iter().any(BqValue::is_null) {
        return Ok(BqValue::Null);
    }
    let mut it = args.into_iter();
    let arr_v = it.next().unwrap_or(BqValue::Null);
    let target = it.next().unwrap_or(BqValue::Null);
    let mode = it.next();
    let BqValue::Array(items) = arr_v else {
        return Err(BqError::query("ARRAY_FIND/OFFSET expects ARRAY"));
    };
    let last_mode = matches!(mode, Some(BqValue::String(ref m)) if m.eq_ignore_ascii_case("LAST"));
    let iter_indices: Box<dyn Iterator<Item = usize>> = if last_mode {
        Box::new((0..items.len()).rev())
    } else {
        Box::new(0..items.len())
    };
    for idx in iter_indices {
        if let Some(item) = items.get(idx) {
            if !item.is_null() && values_equal(item, &target)? {
                return if return_offset {
                    Ok(BqValue::Int64(i64::try_from(idx).unwrap_or(i64::MAX)))
                } else {
                    Ok(item.clone())
                };
            }
        }
    }
    Ok(BqValue::Null)
}

#[expect(
    clippy::cast_precision_loss,
    clippy::wildcard_enum_match_arm,
    reason = "ARRAY_AVG converts numeric scalar variants to FLOAT64"
)]
fn eval_array_avg(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ARRAY_AVG expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arg else {
        return Err(BqError::query("ARRAY_AVG expects ARRAY"));
    };
    let non_null: Vec<BqValue> = items.into_iter().filter(|v| !v.is_null()).collect();
    if non_null.is_empty() {
        return Ok(BqValue::Null);
    }
    if matches!(non_null.first(), Some(BqValue::Interval(_))) {
        return avg_intervals(&non_null);
    }
    let mut sum = 0.0_f64;
    for v in &non_null {
        match v {
            BqValue::Int64(n) => sum += *n as f64,
            BqValue::Float64(f) => sum += *f,
            BqValue::Numeric(n) => sum += n.to_f64(),
            BqValue::BigNumeric(bn) => sum += bn.to_f64(),
            other => {
                return Err(BqError::query(format!(
                    "ARRAY_AVG not supported for {}",
                    other.infer_type()?
                )))
            }
        }
    }
    Ok(BqValue::Float64(sum / (non_null.len() as f64)))
}

fn eval_generate_timestamp_array(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [start_v, end_v, step_v]: [BqValue; 3] = args
        .try_into()
        .map_err(|_| BqError::query("GENERATE_TIMESTAMP_ARRAY expects 3 arguments"))?;
    if start_v.is_null() || end_v.is_null() || step_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (start_v, end_v) = (coerce_timestamp_arg(start_v)?, coerce_timestamp_arg(end_v)?);
    let (BqValue::Timestamp(start), BqValue::Timestamp(end), BqValue::Interval(step)) =
        (start_v, end_v, step_v)
    else {
        return Err(BqError::query(
            "GENERATE_TIMESTAMP_ARRAY expects (TIMESTAMP, TIMESTAMP, INTERVAL)",
        ));
    };
    if step.months != 0 {
        return Err(BqError::semantic(
            "GENERATE_TIMESTAMP_ARRAY step cannot contain months/years",
        ));
    }
    let step_us = i64::from(step.days)
        .checked_mul(86_400_000_000)
        .and_then(|d| d.checked_add(step.micros))
        .ok_or_else(|| BqError::semantic("GENERATE_TIMESTAMP_ARRAY step overflow"))?;
    if step_us == 0 {
        return Err(BqError::semantic(
            "GENERATE_TIMESTAMP_ARRAY step cannot be zero",
        ));
    }
    let forward = step_us > 0;
    let delta = Duration::microseconds(step_us);
    let mut out = Vec::new();
    let mut cur = start;
    while (if forward { cur <= end } else { cur >= end }) && out.len() < 100_000 {
        out.push(BqValue::Timestamp(cur));
        cur += delta;
    }
    Ok(BqValue::Array(out))
}
