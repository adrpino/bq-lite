use crate::ast::{BinaryOp, Expr, UnaryOp, WhenBranch};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{
    access_struct_field, combine_logical, eval_binary, eval_expr, eval_is_test, to_opt_bool,
    values_equal,
};
use crate::types::BqValue;

use super::agg::GroupedKeys;
use super::agg_builtins::{eval_aggregate_call, is_aggregate_name};

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining Expr variants evaluate against the sample row"
)]
pub(super) fn eval_grouped_expr(
    expr: &Expr,
    bucket: &[EvalRow],
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if let Some((_, key_val)) = keys.active.iter().find(|(k, _)| k == expr) {
        return Ok(key_val.clone());
    }
    if keys.rolled_up.iter().any(|k| k == expr) {
        return Ok(BqValue::Null);
    }
    match expr {
        Expr::Function(call) if call.over.is_none() => eval_grouped_fn(call, bucket, keys, ctx),
        Expr::Binary { left, op, right } => {
            if matches!(op, BinaryOp::And | BinaryOp::Or) {
                let l = eval_grouped_expr(left, bucket, keys, ctx)?;
                let lb = to_opt_bool(&l)?;
                if (*op == BinaryOp::And && lb == Some(false))
                    || (*op == BinaryOp::Or && lb == Some(true))
                {
                    return Ok(BqValue::Bool(lb.unwrap_or(false)));
                }
                let r = eval_grouped_expr(right, bucket, keys, ctx)?;
                let rb = to_opt_bool(&r)?;
                return Ok(combine_logical(*op, lb, rb));
            }
            let l = eval_grouped_expr(left, bucket, keys, ctx)?;
            let r = eval_grouped_expr(right, bucket, keys, ctx)?;
            eval_binary(*op, l, r)
        }
        Expr::Is { expr: inner, test } => {
            let v = eval_grouped_expr(inner, bucket, keys, ctx)?;
            Ok(eval_is_test(&v, *test))
        }
        Expr::Unary { op, expr: inner } => {
            let v = eval_grouped_expr(inner, bucket, keys, ctx)?;
            eval_unary_val(*op, v)
        }
        Expr::Grouped(inner) => eval_grouped_expr(inner, bucket, keys, ctx),
        Expr::Cast {
            expr: inner,
            ty,
            safe,
            ..
        } => {
            let v = eval_grouped_expr(inner, bucket, keys, ctx)?;
            let target = ty.to_bq_type()?;
            match crate::types::coercion::cast_value(v, &target) {
                Ok(out) => Ok(out),
                Err(e) if *safe && e.is_safe_suppressible() => Ok(BqValue::Null),
                Err(e) => Err(e),
            }
        }
        Expr::Case {
            operand,
            whens,
            else_result,
        } => eval_grouped_case(
            operand.as_deref(),
            whens,
            else_result.as_deref(),
            bucket,
            keys,
            ctx,
        ),
        Expr::FieldAccess { expr: inner, field } => {
            let base = eval_grouped_expr(inner, bucket, keys, ctx)?;
            access_struct_field(&base, &field.value)
        }
        _ => {
            let sample = bucket.first().cloned().unwrap_or_default();
            eval_expr(expr, &sample, ctx)
        }
    }
}

fn eval_grouped_fn(
    call: &crate::ast::FunctionCall,
    bucket: &[EvalRow],
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let name = call.name.normalized_parts().join(".");
    if name.eq_ignore_ascii_case("GROUPING") {
        return eval_grouping_fn(call, keys);
    }
    if is_aggregate_name(&name) {
        return eval_aggregate_call(call, bucket, ctx);
    }
    let args = call
        .args
        .iter()
        .map(|a| eval_grouped_expr(&a.expr, bucket, keys, ctx))
        .collect::<BqResult<Vec<_>>>()?;
    if let Some(val) = crate::executor::scalar::eval_scalar_builtin(&name, args, ctx)? {
        return Ok(val);
    }
    Err(BqError::unimplemented(format!(
        "Function '{name}' is not implemented"
    )))
}

fn eval_grouping_fn(call: &crate::ast::FunctionCall, keys: &GroupedKeys<'_>) -> BqResult<BqValue> {
    let arg = call
        .args
        .first()
        .ok_or_else(|| BqError::query("GROUPING requires 1 argument"))?;
    let is_active = keys.active.iter().any(|(k, _)| k == &arg.expr);
    Ok(BqValue::Int64(i64::from(!is_active)))
}

fn eval_unary_val(op: UnaryOp, val: BqValue) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    match (op, val) {
        (UnaryOp::Not, BqValue::Bool(b)) => Ok(BqValue::Bool(!b)),
        (UnaryOp::Minus, BqValue::Int64(n)) => Ok(BqValue::Int64(-n)),
        (UnaryOp::Minus, BqValue::Float64(f)) => Ok(BqValue::Float64(-f)),
        (UnaryOp::Plus, v) => Ok(v),
        _ => Err(BqError::query(
            "Unsupported unary operator in grouped expression",
        )),
    }
}

fn eval_grouped_case(
    operand: Option<&Expr>,
    whens: &[WhenBranch],
    else_result: Option<&Expr>,
    bucket: &[EvalRow],
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let op_val = operand
        .map(|e| eval_grouped_expr(e, bucket, keys, ctx))
        .transpose()?;
    for w in whens {
        let cond = eval_grouped_expr(&w.condition, bucket, keys, ctx)?;
        let matched = match &op_val {
            Some(ov) => !ov.is_null() && !cond.is_null() && values_equal(ov, &cond)?,
            None => matches!(cond, BqValue::Bool(true)),
        };
        if matched {
            return eval_grouped_expr(&w.result, bucket, keys, ctx);
        }
    }
    else_result.map_or(Ok(BqValue::Null), |e| {
        eval_grouped_expr(e, bucket, keys, ctx)
    })
}
