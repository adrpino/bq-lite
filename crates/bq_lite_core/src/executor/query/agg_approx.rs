use std::cmp::Ordering;

use crate::ast::{FunctionCall, NullHandling};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{compare_values, eval_expr};
use crate::types::BqValue;

use super::agg_builtins::dedup_values;

const MAX_APPROX_QUANTILES: i64 = 1_000_000;

pub(super) fn eval_approx_quantiles(
    call: &FunctionCall,
    rows: &[EvalRow],
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
        return Err(BqError::semantic(format!(
            "APPROX_QUANTILES number of quantiles must be between 1 and {MAX_APPROX_QUANTILES}"
        )));
    }
    let k = usize::try_from(k_raw)
        .map_err(|_| BqError::semantic("APPROX_QUANTILES number of quantiles out of range"))?;

    let respect_nulls = matches!(call.null_handling, Some(NullHandling::RespectNulls));
    let mut vals = Vec::with_capacity(rows.len());
    for row in rows {
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
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [val_arg] = call.args.as_slice() else {
        return Err(BqError::query("APPROX_COUNT_DISTINCT expects 1 argument"));
    };
    let mut vals = Vec::with_capacity(rows.len());
    for row in rows {
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
