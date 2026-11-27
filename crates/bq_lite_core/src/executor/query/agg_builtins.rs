use std::cmp::Ordering;

use crate::ast::{FunctionCall, HavingKind, NullHandling};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{compare_values, eval_expr, values_distinct_eq};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::{BqType, BqValue};

use super::agg_approx::{eval_approx_count_distinct, eval_approx_quantiles, eval_stat_agg};
use super::sort::sort_with_order_keys;

const AGGREGATE_FUNCTIONS: &[&str] = &[
    "ANY_VALUE",
    "APPROX_COUNT_DISTINCT",
    "APPROX_QUANTILES",
    "ARRAY_AGG",
    "AVG",
    "BIT_AND",
    "BIT_OR",
    "BIT_XOR",
    "COUNT",
    "COUNTIF",
    "LOGICAL_AND",
    "LOGICAL_OR",
    "MAX",
    "MIN",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "STRING_AGG",
    "SUM",
    "VAR_POP",
    "VAR_SAMP",
    "VARIANCE",
];

#[must_use]
pub(super) fn is_aggregate_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    AGGREGATE_FUNCTIONS.contains(&upper.as_str())
}

pub(super) fn eval_aggregate_call(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let filtered_storage;
    let rows = if let Some((having_kind, ref having_expr)) = call.having {
        filtered_storage = filter_rows_by_having(rows, having_kind, having_expr, ctx)?;
        filtered_storage.as_slice()
    } else {
        rows
    };
    let upper = call.name.normalized_parts().join(".").to_ascii_uppercase();
    match upper.as_str() {
        "COUNT" => eval_count(call, rows, ctx),
        "COUNTIF" => eval_countif(call, rows, ctx),
        "SUM" => eval_sum(call, rows, ctx),
        "AVG" => eval_avg(call, rows, ctx),
        "MIN" => eval_min_max(call, rows, ctx, true),
        "MAX" => eval_min_max(call, rows, ctx, false),
        "ANY_VALUE" => eval_any_value(call, rows, ctx),
        "APPROX_COUNT_DISTINCT" => eval_approx_count_distinct(call, rows, ctx),
        "APPROX_QUANTILES" => eval_approx_quantiles(call, rows, ctx),
        "ARRAY_AGG" => eval_array_agg(call, rows, ctx),
        "STRING_AGG" => eval_string_agg(call, rows, ctx),
        "LOGICAL_AND" => eval_logical(call, rows, ctx, true),
        "LOGICAL_OR" => eval_logical(call, rows, ctx, false),
        "BIT_AND" | "BIT_OR" | "BIT_XOR" => eval_bitwise_agg(&upper, call, rows, ctx),
        "STDDEV" | "STDDEV_SAMP" => eval_stat_agg(call, rows, ctx, true, false),
        "STDDEV_POP" => eval_stat_agg(call, rows, ctx, true, true),
        "VARIANCE" | "VAR_SAMP" => eval_stat_agg(call, rows, ctx, false, false),
        "VAR_POP" => eval_stat_agg(call, rows, ctx, false, true),
        _ => Err(BqError::unimplemented(format!(
            "Aggregate function '{upper}' is not implemented"
        ))),
    }
}

pub(super) fn collect_arg_values(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("Aggregate expects at least 1 argument"))?;
    let mut vals = Vec::with_capacity(rows.len());
    for row in rows {
        let v = eval_expr(&arg.expr, row, ctx)?;
        if !v.is_null() {
            vals.push(v);
        }
    }
    if call.distinct {
        dedup_values(&mut vals)?;
    }
    Ok(vals)
}

pub(super) fn dedup_values(vals: &mut Vec<BqValue>) -> BqResult<()> {
    let mut out = Vec::with_capacity(vals.len());
    for v in vals.drain(..) {
        let mut seen = false;
        for existing in &out {
            if values_distinct_eq(existing, &v)? {
                seen = true;
                break;
            }
        }
        if !seen {
            out.push(v);
        }
    }
    *vals = out;
    Ok(())
}

fn eval_count(call: &FunctionCall, rows: &[EvalRow], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    let Some(arg) = call.args.first() else {
        return Err(BqError::query("COUNT requires 1 argument"));
    };
    if matches!(arg.expr, crate::ast::Expr::Star) {
        return Ok(BqValue::Int64(
            i64::try_from(rows.len()).unwrap_or(i64::MAX),
        ));
    }
    let vals = collect_arg_values(call, rows, ctx)?;
    Ok(BqValue::Int64(
        i64::try_from(vals.len()).unwrap_or(i64::MAX),
    ))
}

fn eval_countif(call: &FunctionCall, rows: &[EvalRow], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("COUNTIF requires 1 argument"))?;
    let mut count = 0_i64;
    for row in rows {
        if matches!(eval_expr(&arg.expr, row, ctx)?, BqValue::Bool(true)) {
            count += 1;
        }
    }
    Ok(BqValue::Int64(count))
}

fn eval_sum(call: &FunctionCall, rows: &[EvalRow], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    let Some(first) = vals.first() else {
        return Ok(BqValue::Null);
    };
    let types: Vec<BqType> = vals
        .iter()
        .map(BqValue::infer_type)
        .collect::<BqResult<_>>()?;
    let supertype = common_supertype(&types)?;
    let mut acc = coerce_value(first.clone(), &supertype)?;
    for v in vals.into_iter().skip(1) {
        acc = crate::executor::evaluator::eval_binary(crate::ast::BinaryOp::Add, acc, v)?;
    }
    Ok(acc)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "AVG only supports numeric scalar variants"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "GoogleSQL AVG converts integer sums to FLOAT64"
)]
fn eval_avg(call: &FunctionCall, rows: &[EvalRow], ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    if vals.is_empty() {
        return Ok(BqValue::Null);
    }
    let count = vals.len();
    let sum = eval_sum(call, rows, ctx)?;
    match sum {
        BqValue::Int64(n) => Ok(BqValue::Float64((n as f64) / (count as f64))),
        BqValue::Float64(f) => Ok(BqValue::Float64(f / (count as f64))),
        BqValue::Numeric(n) => n
            .checked_div(crate::types::BqNumeric::from_i64(
                i64::try_from(count).unwrap_or(i64::MAX),
            ))
            .map(BqValue::Numeric),
        BqValue::BigNumeric(bn) => bn
            .checked_div(&crate::types::BqBigNumeric::from_i64(
                i64::try_from(count).unwrap_or(i64::MAX),
            ))
            .map(|v| BqValue::BigNumeric(Box::new(v))),
        other => Err(BqError::query(format!(
            "AVG not supported for {}",
            other.infer_type()?
        ))),
    }
}

fn eval_min_max(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
    is_min: bool,
) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    let mut best: Option<BqValue> = None;
    for v in vals {
        if matches!(v, BqValue::Float64(f) if f.is_nan()) {
            return Ok(v);
        }
        best = match best {
            None => Some(v),
            Some(cur) => {
                let ord = compare_values(&v, &cur)?;
                let take_new = if is_min {
                    ord == Some(Ordering::Less)
                } else {
                    ord == Some(Ordering::Greater)
                };
                Some(if take_new { v } else { cur })
            }
        };
    }
    Ok(best.unwrap_or(BqValue::Null))
}

fn eval_any_value(
    call: &FunctionCall,
    rows: &[EvalRow],
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
    for row in rows {
        let v = eval_expr(&arg.expr, row, ctx)?;
        if !v.is_null() {
            return Ok(v);
        }
    }
    Ok(BqValue::Null)
}

fn filter_rows_by_having(
    rows: &[EvalRow],
    kind: HavingKind,
    having_expr: &crate::ast::Expr,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let mut best_key: Option<BqValue> = None;
    let mut winners: Vec<EvalRow> = Vec::new();
    for row in rows {
        let key = eval_expr(having_expr, row, ctx)?;
        if key.is_null() {
            continue;
        }
        if matches!(key, BqValue::Float64(f) if f.is_nan()) {
            return Ok(Vec::new());
        }
        match &best_key {
            None => {
                best_key = Some(key);
                winners.clear();
                winners.push(row.clone());
            }
            Some(cur) => {
                let ord = compare_values(&key, cur)?;
                let is_better = match kind {
                    HavingKind::Max => ord == Some(Ordering::Greater),
                    HavingKind::Min => ord == Some(Ordering::Less),
                };
                if is_better {
                    best_key = Some(key);
                    winners.clear();
                    winners.push(row.clone());
                } else if ord == Some(Ordering::Equal) {
                    winners.push(row.clone());
                }
            }
        }
    }
    Ok(winners)
}

fn eval_array_agg(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("ARRAY_AGG requires 1 argument"))?;
    let ignore_nulls = call.null_handling == Some(NullHandling::IgnoreNulls);
    let mut items = Vec::new();
    for row in rows {
        let val = eval_expr(&arg.expr, row, ctx)?;
        if ignore_nulls && val.is_null() {
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
    let mut vals: Vec<BqValue> = items.into_iter().map(|(v, _)| v).collect();
    if call.distinct {
        dedup_values(&mut vals)?;
    }
    if let Some(ref lim_expr) = call.limit {
        if let BqValue::Int64(n) = eval_expr(lim_expr, &EvalRow::default(), ctx)? {
            let cap = usize::try_from(n.max(0)).unwrap_or(0);
            vals.truncate(cap);
        }
    }
    if vals.is_empty() {
        Ok(BqValue::Null)
    } else {
        Ok(BqValue::Array(vals))
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "STRING_AGG elements must be STRING or BYTES"
)]
fn join_string_or_bytes(vals: Vec<BqValue>, delim: Option<BqValue>) -> BqResult<BqValue> {
    if matches!(vals.first(), Some(BqValue::Bytes(_))) {
        let sep = match delim {
            None => b",".to_vec(),
            Some(BqValue::Bytes(b)) => b,
            _ => return Err(BqError::query("STRING_AGG delimiter must be BYTES")),
        };
        let mut out = Vec::new();
        for (i, v) in vals.into_iter().enumerate() {
            let BqValue::Bytes(b) = v else {
                return Err(BqError::query("STRING_AGG expects BYTES"));
            };
            if i > 0 {
                out.extend_from_slice(&sep);
            }
            out.extend_from_slice(&b);
        }
        return Ok(BqValue::Bytes(out));
    }
    let sep = match delim {
        None => ",".to_owned(),
        Some(BqValue::String(s)) => s,
        _ => return Err(BqError::query("STRING_AGG delimiter must be STRING")),
    };
    let joined = vals
        .into_iter()
        .map(|v| match v {
            BqValue::String(s) => Ok(s),
            other => Err(BqError::query(format!(
                "STRING_AGG expects STRING, got {}",
                other.infer_type()?
            ))),
        })
        .collect::<BqResult<Vec<_>>>()?
        .join(&sep);
    Ok(BqValue::String(joined))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "STRING_AGG delimiter must be STRING, BYTES, or NULL"
)]
fn eval_string_agg(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("STRING_AGG requires 1 or 2 arguments"))?;
    let delim = match call.args.get(1) {
        Some(d) => match eval_expr(&d.expr, &EvalRow::default(), ctx)? {
            BqValue::Null => return Ok(BqValue::Null),
            v @ (BqValue::String(_) | BqValue::Bytes(_)) => Some(v),
            _ => {
                return Err(BqError::query(
                    "STRING_AGG delimiter must be STRING or BYTES",
                ))
            }
        },
        None => None,
    };
    let mut items = Vec::new();
    for row in rows {
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
    let mut strs: Vec<BqValue> = items.into_iter().map(|(v, _)| v).collect();
    if call.distinct {
        dedup_values(&mut strs)?;
    }
    if strs.is_empty() {
        return Ok(BqValue::Null);
    }
    join_string_or_bytes(strs, delim)
}

fn eval_logical(
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
    is_and: bool,
) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    if vals.is_empty() {
        return Ok(BqValue::Null);
    }
    let mut acc = is_and;
    for v in vals {
        let BqValue::Bool(b) = v else {
            return Err(BqError::query("LOGICAL_AND/OR expects BOOL"));
        };
        acc = if is_and { acc && b } else { acc || b };
    }
    Ok(BqValue::Bool(acc))
}

fn eval_bitwise_agg(
    name: &str,
    call: &FunctionCall,
    rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let vals = collect_arg_values(call, rows, ctx)?;
    if vals.is_empty() {
        return Ok(BqValue::Null);
    }
    let mut ints = Vec::with_capacity(vals.len());
    for v in vals {
        let BqValue::Int64(n) = v else {
            return Err(BqError::query(format!("{name} expects INT64")));
        };
        ints.push(n);
    }
    let first = ints.first().copied().unwrap_or(0);
    let acc = ints.into_iter().skip(1).fold(first, |a, b| match name {
        "BIT_AND" => a & b,
        "BIT_OR" => a | b,
        _ => a ^ b,
    });
    Ok(BqValue::Int64(acc))
}
