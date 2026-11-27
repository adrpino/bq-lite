use std::cmp::Ordering;

use crate::ast::{FunctionCall, NullHandling};
use crate::error::{BqError, BqResult};
use crate::executor::context::ExecContext;
use crate::executor::evaluator::{compare_values, eval_expr};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::{BqType, BqValue};

use super::agg_builtins::{collect_arg_values, dedup_values};
use super::exec_util::RowSlice;
use super::sort::sort_with_order_keys;

pub(super) fn eval_any_value(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("ANY_VALUE requires 1 argument"))?;
    if call.having.is_some() {
        return rows
            .first()
            .map_or(Ok(BqValue::Null), |row| eval_expr(&arg.expr, row, ctx));
    }
    for row in rows.iter() {
        let v = eval_expr(&arg.expr, row, ctx)?;
        if !v.is_null() {
            return Ok(v);
        }
    }
    Ok(BqValue::Null)
}

pub(super) fn eval_array_concat_agg(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("ARRAY_CONCAT_AGG requires 1 argument"))?;
    let mut items = Vec::new();
    for row in rows.iter() {
        let val = eval_expr(&arg.expr, row, ctx)?;
        if val.is_null() {
            continue;
        }
        let keys = call
            .order_by
            .iter()
            .map(|ob| eval_expr(&ob.expr, row, ctx))
            .collect::<BqResult<Vec<_>>>()?;
        items.push((val, keys));
    }
    if !call.order_by.is_empty() {
        sort_with_order_keys(&mut items, &call.order_by)?;
    }
    if let Some(ref lim_expr) = call.limit {
        if let BqValue::Int64(n) =
            eval_expr(lim_expr, &crate::executor::context::EvalRow::default(), ctx)?
        {
            let cap = usize::try_from(n.max(0)).unwrap_or(0);
            items.truncate(cap);
        }
    }
    if items.is_empty() {
        return Ok(BqValue::Null);
    }
    let mut out = Vec::new();
    for (v, _) in items {
        let BqValue::Array(arr) = v else {
            return Err(BqError::query("ARRAY_CONCAT_AGG expects ARRAY arguments"));
        };
        out.extend(arr);
    }
    Ok(BqValue::Array(out))
}

pub(super) fn eval_bitwise_bytes_agg(name: &str, vals: Vec<BqValue>) -> BqResult<BqValue> {
    let mut it = vals.into_iter();
    let Some(BqValue::Bytes(mut acc)) = it.next() else {
        return Err(BqError::query(format!("{name} expects INT64 or BYTES")));
    };
    for v in it {
        let BqValue::Bytes(b) = v else {
            return Err(BqError::query(format!("{name} expects BYTES")));
        };
        if b.len() != acc.len() {
            return Err(BqError::semantic(format!(
                "{name} BYTES inputs must have equal length"
            )));
        }
        for (a, x) in acc.iter_mut().zip(b.iter()) {
            match name {
                "BIT_AND" => *a &= *x,
                "BIT_OR" => *a |= *x,
                _ => *a ^= *x,
            }
        }
    }
    Ok(BqValue::Bytes(acc))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Values are coerced to NUMERIC/BIGNUMERIC immediately prior to extraction"
)]
pub(super) fn sum_collected_values(vals: Vec<BqValue>) -> BqResult<BqValue> {
    let Some(first) = vals.first() else {
        return Ok(BqValue::Null);
    };
    if matches!(first, BqValue::Interval(_)) {
        return crate::executor::scalar::sum_intervals(&vals);
    }
    let types: Vec<BqType> = vals
        .iter()
        .map(BqValue::infer_type)
        .collect::<BqResult<_>>()?;
    let supertype = common_supertype(&types)?;
    if supertype == BqType::Numeric {
        let nums: Vec<crate::types::BqNumeric> = vals
            .into_iter()
            .map(|v| match coerce_value(v, &BqType::Numeric)? {
                BqValue::Numeric(n) => Ok(n),
                _ => Err(BqError::internal("expected NUMERIC")),
            })
            .collect::<BqResult<_>>()?;
        return crate::types::BqNumeric::sum_slice(&nums).map(BqValue::Numeric);
    }
    if supertype == BqType::BigNumeric {
        let bnums: Vec<crate::types::BqBigNumeric> = vals
            .into_iter()
            .map(|v| match coerce_value(v, &BqType::BigNumeric)? {
                BqValue::BigNumeric(bn) => Ok(*bn),
                _ => Err(BqError::internal("expected BIGNUMERIC")),
            })
            .collect::<BqResult<_>>()?;
        let refs: Vec<&crate::types::BqBigNumeric> = bnums.iter().collect();
        return crate::types::BqBigNumeric::sum_slice(&refs)
            .map(|v| BqValue::BigNumeric(Box::new(v)));
    }
    let mut acc = coerce_value(first.clone(), &supertype)?;
    for v in vals.into_iter().skip(1) {
        acc = crate::executor::evaluator::eval_binary(crate::ast::BinaryOp::Add, acc, v)?;
    }
    Ok(acc)
}

/// Zero-allocation streaming fast-path for homogeneous `INT64` / `FLOAT64` sums.
/// Returns `Ok(None)` when values are mixed or non-primitive so callers fall back to generic coercion.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Fast path only handles Int64 and Float64; other variants fall back"
)]
pub(super) fn try_stream_sum(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("Aggregate expects at least 1 argument"))?;
    let mut int_acc: Option<i64> = None;
    let mut flt_acc: Option<f64> = None;
    for row in rows.iter() {
        let v = eval_expr(&arg.expr, row, ctx)?;
        match v {
            BqValue::Null => {}
            BqValue::Int64(n) if flt_acc.is_none() => {
                let Some(sum) = int_acc.map_or(Some(n), |cur| cur.checked_add(n)) else {
                    return Ok(None);
                };
                int_acc = Some(sum);
            }
            BqValue::Float64(f) if int_acc.is_none() => {
                let next = flt_acc.map_or(f, |cur| cur + f);
                if next.is_infinite() && !f.is_infinite() && !flt_acc.is_some_and(f64::is_infinite)
                {
                    return Ok(None);
                }
                flt_acc = Some(next);
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(int_acc.map_or_else(
        || flt_acc.map_or(BqValue::Null, BqValue::Float64),
        BqValue::Int64,
    )))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Fast path only handles Int64 and Float64; other variants fall back"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "GoogleSQL AVG converts integer sums to FLOAT64"
)]
fn try_stream_avg(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("Aggregate expects at least 1 argument"))?;
    let mut int_acc: Option<i64> = None;
    let mut flt_acc: Option<f64> = None;
    let mut count = 0_usize;
    for row in rows.iter() {
        let v = eval_expr(&arg.expr, row, ctx)?;
        match v {
            BqValue::Null => {}
            BqValue::Int64(n) if flt_acc.is_none() => {
                let Some(sum) = int_acc.map_or(Some(n), |cur| cur.checked_add(n)) else {
                    return Ok(None);
                };
                int_acc = Some(sum);
                count += 1;
            }
            BqValue::Float64(f) if int_acc.is_none() => {
                let next = flt_acc.map_or(f, |cur| cur + f);
                if next.is_infinite() && !f.is_infinite() && !flt_acc.is_some_and(f64::is_infinite)
                {
                    return Ok(None);
                }
                flt_acc = Some(next);
                count += 1;
            }
            _ => return Ok(None),
        }
    }
    if count == 0 {
        return Ok(Some(BqValue::Null));
    }
    let denom = count as f64;
    Ok(Some(int_acc.map_or_else(
        || flt_acc.map_or(BqValue::Null, |f| BqValue::Float64(f / denom)),
        |n| BqValue::Float64((n as f64) / denom),
    )))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "AVG only supports numeric scalar variants"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "GoogleSQL AVG converts integer sums to FLOAT64"
)]
pub(super) fn eval_avg(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if !call.distinct {
        if let Some(streamed) = try_stream_avg(call, rows, ctx)? {
            return Ok(streamed);
        }
    }
    let vals = collect_arg_values(call, rows, ctx)?;
    if vals.is_empty() {
        return Ok(BqValue::Null);
    }
    if matches!(vals.first(), Some(BqValue::Interval(_))) {
        return crate::executor::scalar::avg_intervals(&vals);
    }
    let types: Vec<BqType> = vals
        .iter()
        .map(BqValue::infer_type)
        .collect::<BqResult<_>>()?;
    let supertype = common_supertype(&types)?;
    if supertype == BqType::Numeric {
        let nums: Vec<crate::types::BqNumeric> = vals
            .into_iter()
            .map(|v| match coerce_value(v, &BqType::Numeric)? {
                BqValue::Numeric(n) => Ok(n),
                _ => Err(BqError::internal("expected NUMERIC")),
            })
            .collect::<BqResult<_>>()?;
        return crate::types::BqNumeric::avg_slice(&nums).map(BqValue::Numeric);
    }
    if supertype == BqType::BigNumeric {
        let bnums: Vec<crate::types::BqBigNumeric> = vals
            .into_iter()
            .map(|v| match coerce_value(v, &BqType::BigNumeric)? {
                BqValue::BigNumeric(bn) => Ok(*bn),
                _ => Err(BqError::internal("expected BIGNUMERIC")),
            })
            .collect::<BqResult<_>>()?;
        let refs: Vec<&crate::types::BqBigNumeric> = bnums.iter().collect();
        return crate::types::BqBigNumeric::avg_slice(&refs)
            .map(|v| BqValue::BigNumeric(Box::new(v)));
    }
    let count = vals.len();
    let sum = sum_collected_values(vals)?;
    match sum {
        BqValue::Int64(n) => Ok(BqValue::Float64((n as f64) / (count as f64))),
        BqValue::Float64(f) => Ok(BqValue::Float64(f / (count as f64))),
        other => Err(BqError::query(format!(
            "AVG not supported for {}",
            other.infer_type()?
        ))),
    }
}

const MAX_APPROX_QUANTILES: i64 = 1_000_000;

pub(super) fn eval_approx_quantiles(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [val_arg, count_arg] = call.args.as_slice() else {
        return Err(BqError::query(
            "APPROX_QUANTILES expects 2 arguments (expression, number)",
        ));
    };
    let sample = rows.first().cloned().unwrap_or_default();
    let count_val = eval_expr(&count_arg.expr, &sample, ctx)?;
    if count_val.is_null() {
        if call.safe_prefix {
            return Ok(BqValue::Null);
        }
        return Err(BqError::semantic(
            "APPROX_QUANTILES number of quantiles cannot be NULL",
        ));
    }
    let BqValue::Int64(k_raw) = count_val else {
        return Err(BqError::query(
            "APPROX_QUANTILES second argument must be INT64",
        ));
    };
    if !(1..=MAX_APPROX_QUANTILES).contains(&k_raw) {
        if call.safe_prefix {
            return Ok(BqValue::Null);
        }
        return Err(BqError::semantic(format!(
            "APPROX_QUANTILES number of quantiles must be between 1 and {MAX_APPROX_QUANTILES}"
        )));
    }
    let k = usize::try_from(k_raw)
        .map_err(|_| BqError::semantic("APPROX_QUANTILES number of quantiles out of range"))?;

    let respect_nulls = matches!(call.null_handling, Some(NullHandling::RespectNulls));
    let mut vals = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let v = eval_expr(&val_arg.expr, row, ctx)?;
        if v.is_null() && !respect_nulls {
            continue;
        }
        vals.push(v);
    }
    if call.distinct {
        dedup_values(&mut vals)?;
    }
    if vals.is_empty() {
        return Ok(BqValue::Null);
    }

    sort_approx_values(&mut vals)?;
    let len = vals.len();
    let mut out = Vec::with_capacity(k + 1);
    for i in 0..=k {
        let idx = (i.saturating_mul(len - 1)) / k;
        if let Some(v) = vals.get(idx) {
            out.push(v.clone());
        }
    }
    Ok(BqValue::Array(out))
}

pub(super) fn eval_approx_count_distinct(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [val_arg] = call.args.as_slice() else {
        return Err(BqError::query("APPROX_COUNT_DISTINCT expects 1 argument"));
    };
    let mut vals = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let v = eval_expr(&val_arg.expr, row, ctx)?;
        if !v.is_null() {
            vals.push(v);
        }
    }
    dedup_values(&mut vals)?;
    Ok(BqValue::Int64(
        i64::try_from(vals.len()).unwrap_or(i64::MAX),
    ))
}

fn sort_approx_values(vals: &mut [BqValue]) -> BqResult<()> {
    let mut sort_err: Option<Box<BqError>> = None;
    vals.sort_by(|a, b| {
        if sort_err.is_some() {
            return Ordering::Equal;
        }
        match (a.is_null(), b.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => {
                if let Some(ord) = compare_nan_aware(a, b) {
                    return ord;
                }
                match compare_values(a, b) {
                    Ok(Some(ord)) => ord,
                    Ok(None) => Ordering::Equal,
                    Err(e) => {
                        sort_err = Some(e);
                        Ordering::Equal
                    }
                }
            }
        }
    });
    if let Some(e) = sort_err {
        return Err(e);
    }
    Ok(())
}

fn compare_nan_aware(a: &BqValue, b: &BqValue) -> Option<Ordering> {
    let a_nan = matches!(a, BqValue::Float64(f) if f.is_nan());
    let b_nan = matches!(b, BqValue::Float64(f) if f.is_nan());
    match (a_nan, b_nan) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (false, false) => None,
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Statistical variance/stddev converts integer/numeric values to IEEE-754 FLOAT64"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only numeric variants are valid statistical aggregate inputs"
)]
pub(super) fn eval_stat_agg(
    call: &FunctionCall,
    rows: RowSlice<'_>,
    ctx: &ExecContext<'_>,
    is_stddev: bool,
    is_pop: bool,
) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    if vals.is_empty() || (!is_pop && vals.len() < 2) {
        return Ok(BqValue::Null);
    }
    let nums: Vec<f64> = vals
        .into_iter()
        .map(|v| match v {
            BqValue::Int64(n) => Ok(n as f64),
            BqValue::Float64(f) => Ok(f),
            BqValue::Numeric(n) => Ok(n.to_f64()),
            BqValue::BigNumeric(bn) => Ok(bn.to_f64()),
            other => Err(BqError::query(format!(
                "Statistical aggregate expects numeric input, got {}",
                other.infer_type()?
            ))),
        })
        .collect::<BqResult<_>>()?;
    let n = nums.len() as f64;
    let mean = nums.iter().sum::<f64>() / n;
    let sq_sum: f64 = nums.iter().map(|x| (x - mean) * (x - mean)).sum();
    let denom = if is_pop { n } else { n - 1.0 };
    let var = sq_sum / denom;
    Ok(BqValue::Float64(if is_stddev { var.sqrt() } else { var }))
}
