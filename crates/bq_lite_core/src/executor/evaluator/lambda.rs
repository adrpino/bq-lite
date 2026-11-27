use crate::ast::{Expr, FunctionCall};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::types::BqValue;

use super::eval_expr;

/// Evaluates lambda-accepting array functions (`ARRAY_TRANSFORM`, `ARRAY_FILTER`,
/// `ARRAY_INCLUDES`, `ARRAY_FIND`, `ARRAY_OFFSET`).
/// Per `GoogleSQL` `SAFE.` semantics, errors evaluating the array argument itself are NOT
/// suppressed by `SAFE.`; only errors inside the lambda body are suppressed.
pub(super) fn eval_array_lambda_call(
    upper_fn: &str,
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let arr_arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query(format!("{upper_fn} expects at least 2 arguments")))?;
    let lambda_arg = call
        .args
        .get(1)
        .ok_or_else(|| BqError::query(format!("{upper_fn} expects at least 2 arguments")))?;
    let arr_val = eval_expr(&arr_arg.expr, row, ctx)?;
    if arr_val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Array(items) = arr_val else {
        return Err(BqError::query(format!(
            "{upper_fn} first argument must be ARRAY"
        )));
    };
    let Expr::Lambda { params, body } = &lambda_arg.expr else {
        return Err(BqError::query(format!(
            "{upper_fn} second argument must be a lambda expression"
        )));
    };
    if params.is_empty() || params.len() > 2 {
        return Err(BqError::query(format!(
            "{upper_fn} lambda expects 1 or 2 parameters"
        )));
    }
    let res = match upper_fn {
        "ARRAY_TRANSFORM" | "ARRAY_FILTER" => {
            run_transform_or_filter(upper_fn == "ARRAY_TRANSFORM", items, params, body, row, ctx)
        }
        "ARRAY_INCLUDES" => run_includes_lambda(&items, params, body, row, ctx),
        "ARRAY_FIND" | "ARRAY_OFFSET" => {
            run_find_or_offset_lambda(upper_fn == "ARRAY_OFFSET", items, params, body, row, ctx)
        }
        _ => Err(BqError::internal("unexpected array lambda fn")),
    };
    match res {
        Ok(v) => Ok(v),
        Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}

fn eval_lambda_for_item(
    params: &[crate::ast::Ident],
    body: &Expr,
    item: &BqValue,
    idx: usize,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut sub_ctx = ctx.with_outer(row);
    if let Some(p0) = params.first() {
        sub_ctx
            .vars
            .insert(p0.value.to_ascii_lowercase(), item.clone());
    }
    if let Some(p1) = params.get(1) {
        let off = i64::try_from(idx).unwrap_or(i64::MAX);
        sub_ctx
            .vars
            .insert(p1.value.to_ascii_lowercase(), BqValue::Int64(off));
    }
    eval_expr(body, row, &sub_ctx)
}

fn run_transform_or_filter(
    is_transform: bool,
    items: Vec<BqValue>,
    params: &[crate::ast::Ident],
    body: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut out = Vec::new();
    for (idx, item) in items.into_iter().enumerate() {
        let val = eval_lambda_for_item(params, body, &item, idx, row, ctx)?;
        if is_transform {
            out.push(val);
        } else if matches!(val, BqValue::Bool(true)) {
            out.push(item);
        }
    }
    Ok(BqValue::Array(out))
}

fn run_includes_lambda(
    items: &[BqValue],
    params: &[crate::ast::Ident],
    body: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    for (idx, item) in items.iter().enumerate() {
        let val = eval_lambda_for_item(params, body, item, idx, row, ctx)?;
        if matches!(val, BqValue::Bool(true)) {
            return Ok(BqValue::Bool(true));
        }
    }
    Ok(BqValue::Bool(false))
}

fn run_find_or_offset_lambda(
    return_offset: bool,
    items: Vec<BqValue>,
    params: &[crate::ast::Ident],
    body: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    for (idx, item) in items.into_iter().enumerate() {
        let val = eval_lambda_for_item(params, body, &item, idx, row, ctx)?;
        if matches!(val, BqValue::Bool(true)) {
            return if return_offset {
                Ok(BqValue::Int64(i64::try_from(idx).unwrap_or(i64::MAX)))
            } else {
                Ok(item)
            };
        }
    }
    Ok(BqValue::Null)
}

pub(super) fn eval_make_interval_named(
    fn_name: &str,
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut slots = vec![BqValue::Int64(0); 6];
    for (idx, arg) in call.args.iter().enumerate() {
        let val = eval_expr(&arg.expr, row, ctx)?;
        let pos = if let Some(ref nm) = arg.name {
            match nm.value.to_ascii_lowercase().as_str() {
                "year" => 0,
                "month" => 1,
                "day" => 2,
                "hour" => 3,
                "minute" => 4,
                "second" => 5,
                other => {
                    return Err(BqError::query(format!(
                        "Unknown MAKE_INTERVAL parameter '{other}'"
                    )))
                }
            }
        } else {
            idx
        };
        if let Some(slot) = slots.get_mut(pos) {
            *slot = val;
        }
    }
    match crate::executor::scalar::eval_scalar_builtin(fn_name, slots, ctx) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Ok(BqValue::Null),
        Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}
