use crate::ast::{BinaryOp, Expr, InTarget, StructFieldExpr, UnaryOp, WhenBranch};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{
    access_struct_field, combine_logical, eval_binary, eval_expr, eval_extract_value,
    eval_in_values, eval_is_test, eval_subscript_values, like_match_values, to_opt_bool,
    values_equal,
};
use crate::executor::query::execute_query;
use crate::types::{BqStructFieldValue, BqValue};

use super::agg::GroupedKeys;
use super::agg_builtins::{eval_aggregate_call, is_aggregate_name};
use super::agg_grouping::expr_eq_gb;
use super::exec_util::RowSlice;

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Remaining Expr variants evaluate against the sample row"
)]
pub(super) fn eval_grouped_expr(
    expr: &Expr,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if let Some((_, key_val)) = keys.active.iter().find(|(k, _)| expr_eq_gb(k, expr)) {
        return Ok(key_val.clone());
    }
    if keys.rolled_up.iter().any(|k| expr_eq_gb(k, expr)) {
        return Ok(BqValue::Null);
    }
    match expr {
        Expr::Function(call) if call.over.is_none() => eval_grouped_fn(call, bucket, keys, ctx),
        Expr::Binary { left, op, right } => {
            eval_grouped_binary(left, *op, right, bucket, keys, ctx)
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
        Expr::Subscript {
            expr: inner,
            kind,
            index,
        } => {
            let base = eval_grouped_expr(inner, bucket, keys, ctx)?;
            let idx_val = eval_grouped_expr(index, bucket, keys, ctx)?;
            eval_subscript_values(base, *kind, idx_val)
        }
        Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::In { .. }
        | Expr::Extract { .. }
        | Expr::ArrayLiteral { .. }
        | Expr::StructLiteral { .. } => eval_grouped_compound(expr, bucket, keys, ctx),
        Expr::Ident(id) => eval_grouped_ident(expr, &id.value, bucket, keys, ctx),
        _ => {
            let empty_row = EvalRow::default();
            let sample = bucket.first().unwrap_or(&empty_row);
            eval_expr(expr, sample, ctx)
        }
    }
}

fn eval_grouped_ident(
    expr: &Expr,
    name: &str,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if let Some(aliases) = keys.select_aliases {
        if let Some(aliased) = aliases.get(&name.to_ascii_lowercase()) {
            let next_keys = GroupedKeys {
                active: keys.active,
                rolled_up: keys.rolled_up,
                select_aliases: None,
            };
            return eval_grouped_expr(aliased, bucket, &next_keys, ctx);
        }
    }
    let empty_row = EvalRow::default();
    let sample = bucket.first().unwrap_or(&empty_row);
    eval_expr(expr, sample, ctx)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only compound grouped expression variants are dispatched here"
)]
fn eval_grouped_compound(
    expr: &Expr,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    match expr {
        Expr::Between {
            expr: inner,
            negated,
            low,
            high,
        } => {
            let v = eval_grouped_expr(inner, bucket, keys, ctx)?;
            let lo = eval_grouped_expr(low, bucket, keys, ctx)?;
            let hi = eval_grouped_expr(high, bucket, keys, ctx)?;
            let ge = to_opt_bool(&eval_binary(BinaryOp::Gte, v.clone(), lo)?)?;
            let le = to_opt_bool(&eval_binary(BinaryOp::Lte, v, hi)?)?;
            let res = combine_logical(BinaryOp::And, ge, le);
            if *negated {
                eval_unary_val(UnaryOp::Not, res)
            } else {
                Ok(res)
            }
        }
        Expr::Like {
            expr: inner,
            negated,
            pattern,
        } => eval_grouped_like(inner, *negated, pattern, bucket, keys, ctx),
        Expr::In {
            expr: inner,
            negated,
            target,
        } => eval_grouped_in(inner, *negated, target, bucket, keys, ctx),
        Expr::Extract {
            part,
            expr: inner,
            time_zone,
        } => {
            let val = eval_grouped_expr(inner, bucket, keys, ctx)?;
            let tz_val = time_zone
                .as_deref()
                .map(|tz_e| eval_grouped_expr(tz_e, bucket, keys, ctx))
                .transpose()?;
            eval_extract_value(&part.value, val, tz_val.as_ref())
        }
        Expr::ArrayLiteral { elements, .. } => {
            let mut items = Vec::with_capacity(elements.len());
            for el in elements {
                items.push(eval_grouped_expr(el, bucket, keys, ctx)?);
            }
            Ok(BqValue::Array(items))
        }
        Expr::StructLiteral { fields, .. } => eval_grouped_struct(fields, bucket, keys, ctx),
        _ => Ok(BqValue::Null),
    }
}

fn eval_grouped_like(
    inner: &Expr,
    negated: bool,
    pattern: &Expr,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let lv = eval_grouped_expr(inner, bucket, keys, ctx)?;
    let pv = eval_grouped_expr(pattern, bucket, keys, ctx)?;
    if lv.is_null() || pv.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (lv, pv) else {
        return Err(BqError::query("LIKE operands must be STRING"));
    };
    let matched = like_match_values(&s, &p);
    Ok(BqValue::Bool(if negated { !matched } else { matched }))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "UNNEST target must be Null or Array"
)]
fn eval_grouped_in(
    inner: &Expr,
    negated: bool,
    target: &InTarget,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let lhs = eval_grouped_expr(inner, bucket, keys, ctx)?;
    let candidates = match target {
        InTarget::List(exprs) => exprs
            .iter()
            .map(|e| eval_grouped_expr(e, bucket, keys, ctx))
            .collect::<BqResult<Vec<_>>>()?,
        InTarget::Unnest(arr_expr) => match eval_grouped_expr(arr_expr, bucket, keys, ctx)? {
            BqValue::Null => Vec::new(),
            BqValue::Array(items) => items,
            other => {
                return Err(BqError::query(format!(
                    "UNNEST expects ARRAY, got {}",
                    other.infer_type()?
                )))
            }
        },
        InTarget::Subquery(q) => {
            let empty_row = EvalRow::default();
            let sample = bucket.first().unwrap_or(&empty_row);
            let sub_ctx = ctx.with_outer(sample);
            let res = execute_query(q, &sub_ctx)?;
            if res.columns.len() != 1 {
                return Err(BqError::query("IN subquery must return exactly 1 column"));
            }
            res.rows
                .into_iter()
                .filter_map(|r| r.into_iter().next())
                .collect()
        }
    };
    eval_in_values(&lhs, negated, &candidates)
}

fn eval_grouped_struct(
    fields: &[StructFieldExpr],
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let value = eval_grouped_expr(&f.expr, bucket, keys, ctx)?;
        out.push(BqStructFieldValue {
            name: f.name.as_ref().map(|n| n.value.clone()),
            value,
        });
    }
    Ok(BqValue::Struct(out))
}

fn eval_grouped_binary(
    left: &Expr,
    op: BinaryOp,
    right: &Expr,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if matches!(op, BinaryOp::And | BinaryOp::Or) {
        let l = eval_grouped_expr(left, bucket, keys, ctx)?;
        let lb = to_opt_bool(&l)?;
        if (op == BinaryOp::And && lb == Some(false)) || (op == BinaryOp::Or && lb == Some(true)) {
            return Ok(BqValue::Bool(lb.unwrap_or(false)));
        }
        let r = eval_grouped_expr(right, bucket, keys, ctx)?;
        let rb = to_opt_bool(&r)?;
        return Ok(combine_logical(op, lb, rb));
    }
    let l = eval_grouped_expr(left, bucket, keys, ctx)?;
    let r = eval_grouped_expr(right, bucket, keys, ctx)?;
    eval_binary(op, l, r)
}

fn eval_grouped_fn(
    call: &crate::ast::FunctionCall,
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let name = call.name.normalized_parts().join(".");
    if name.eq_ignore_ascii_case("GROUPING") {
        return eval_grouping_fn(call, keys);
    }
    if is_aggregate_name(&name) {
        return match eval_aggregate_call(call, bucket, ctx) {
            Ok(val) => Ok(val),
            Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
            Err(err) => Err(err),
        };
    }
    let upper = name.to_ascii_uppercase();
    let mut args = Vec::with_capacity(call.args.len());
    for (idx, a) in call.args.iter().enumerate() {
        if crate::executor::evaluator::is_date_part_arg_pos(&upper, idx) {
            if let Expr::Ident(id) = &a.expr {
                args.push(BqValue::String(id.value.clone()));
                continue;
            }
        }
        args.push(eval_grouped_expr(&a.expr, bucket, keys, ctx)?);
    }
    let empty_row = EvalRow::default();
    let sample = bucket.first().unwrap_or(&empty_row);
    match crate::executor::scalar::eval_scalar_builtin(&name, args, ctx) {
        Ok(Some(val)) => {
            if matches!(upper.as_str(), "IF" | "COALESCE" | "IFNULL" | "NULLIF") {
                let branch_exprs: Vec<&Expr> = if upper == "IF" {
                    call.args.iter().skip(1).map(|a| &a.expr).collect()
                } else {
                    call.args.iter().map(|a| &a.expr).collect()
                };
                return crate::executor::evaluator::coerce_to_expr_supertype(
                    val,
                    &branch_exprs,
                    sample,
                    ctx,
                );
            }
            return Ok(val);
        }
        Ok(None) => {}
        Err(err) if call.safe_prefix && err.is_safe_suppressible() => return Ok(BqValue::Null),
        Err(err) => return Err(err),
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
    let target = if let Expr::Ident(id) = &arg.expr {
        keys.select_aliases
            .and_then(|m| m.get(&id.value.to_ascii_lowercase()))
            .unwrap_or(&arg.expr)
    } else {
        &arg.expr
    };
    let is_active = keys
        .active
        .iter()
        .any(|(k, _)| expr_eq_gb(k, &arg.expr) || expr_eq_gb(k, target));
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
    bucket: RowSlice<'_>,
    keys: &GroupedKeys<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let op_val = operand
        .map(|e| eval_grouped_expr(e, bucket, keys, ctx))
        .transpose()?;
    let empty_row = EvalRow::default();
    let sample = bucket.first().unwrap_or(&empty_row);
    let mut result_exprs: Vec<&Expr> = whens.iter().map(|w| &w.result).collect();
    if let Some(e) = else_result {
        result_exprs.push(e);
    }
    for w in whens {
        let cond = eval_grouped_expr(&w.condition, bucket, keys, ctx)?;
        let matched = match &op_val {
            Some(ov) => !ov.is_null() && !cond.is_null() && values_equal(ov, &cond)?,
            None => matches!(cond, BqValue::Bool(true)),
        };
        if matched {
            let val = eval_grouped_expr(&w.result, bucket, keys, ctx)?;
            return crate::executor::evaluator::coerce_to_expr_supertype(
                val,
                &result_exprs,
                sample,
                ctx,
            );
        }
    }
    let val = else_result.map_or(Ok(BqValue::Null), |e| {
        eval_grouped_expr(e, bucket, keys, ctx)
    })?;
    crate::executor::evaluator::coerce_to_expr_supertype(val, &result_exprs, sample, ctx)
}
