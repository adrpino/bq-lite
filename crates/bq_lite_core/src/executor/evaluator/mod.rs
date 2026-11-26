mod calls;
mod literals;
mod ops;
mod subquery;

use crate::ast::{Expr, Ident, UnaryOp, WhenBranch};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::query::execute_query;
use crate::types::coercion::cast_value;
use crate::types::BqValue;

pub(super) use calls::infer_expr_col_name;
use calls::{eval_array_literal, eval_function_expr, eval_struct_literal};
pub(super) use ops::{
    combine_logical, compare_values, eval_binary, eval_is_test, eval_unary, to_opt_bool,
    values_distinct_eq, values_equal,
};
use subquery::{
    eval_array_subquery, eval_extract, eval_in_expr, eval_scalar_subquery, eval_subscript,
};

pub(super) fn eval_expr(expr: &Expr, row: &EvalRow, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    match expr {
        Expr::Literal(lit) => literals::eval_literal(lit),
        Expr::Ident(id) => resolve_single_ident(&id.value, row, ctx),
        Expr::CompoundIdent(parts) => resolve_compound_ident(parts, row, ctx),
        Expr::Parameter(_) | Expr::PositionalParam(_) | Expr::SystemVar(_) => {
            eval_param_or_sysvar(expr, ctx)
        }
        Expr::Grouped(inner) => eval_expr(inner, row, ctx),
        Expr::Unary { op, expr } => eval_unary_expr(*op, expr, row, ctx),
        Expr::Binary { left, op, right } => eval_binary_expr(*op, left, right, row, ctx),
        Expr::Is { expr, test } => Ok(eval_is_test(&eval_expr(expr, row, ctx)?, *test)),
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => eval_between(expr, *negated, low, high, row, ctx),
        Expr::Like {
            expr,
            negated,
            pattern,
        } => eval_like_expr(expr, *negated, pattern, row, ctx),
        Expr::In {
            expr,
            negated,
            target,
        } => eval_in_expr(expr, *negated, target, row, ctx),
        Expr::Cast { expr, ty, safe, .. } => eval_cast_expr(expr, ty, *safe, row, ctx),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => eval_case_expr(operand.as_deref(), whens, else_result.as_deref(), row, ctx),
        Expr::Function(call) => eval_function_expr(call, row, ctx),
        Expr::ArrayLiteral {
            elem_type,
            elements,
        } => eval_array_literal(elem_type.as_ref(), elements, row, ctx),
        Expr::StructLiteral {
            explicit_type,
            fields,
        } => eval_struct_literal(explicit_type.as_ref(), fields, row, ctx),
        Expr::Subscript { expr, kind, index } => eval_subscript(expr, *kind, index, row, ctx),
        Expr::FieldAccess { expr, field } => {
            access_struct_field(&eval_expr(expr, row, ctx)?, &field.value)
        }
        Expr::Extract { part, expr } => eval_extract(&part.value, expr, row, ctx),
        Expr::Exists(q) => {
            let res = execute_query(q, &ctx.with_outer(row))?;
            Ok(BqValue::Bool(!res.rows.is_empty()))
        }
        Expr::Subquery(q) => eval_scalar_subquery(q, row, ctx),
        Expr::ArraySubquery(q) => eval_array_subquery(q, row, ctx),
        Expr::Interval { value, unit, .. } => eval_interval_expr(value, &unit.value, row, ctx),
        Expr::RangeLiteral {
            elem_type,
            start,
            end,
        } => eval_range_literal(elem_type, start, end, row, ctx),
        Expr::Star | Expr::QualifiedStar(_) => Err(BqError::query("Unexpected '*' in expression")),
        Expr::Lambda { .. } => Err(BqError::query(
            "Lambda expression is only valid inside ARRAY_TRANSFORM / ARRAY_FILTER",
        )),
    }
}

fn eval_range_literal(
    elem_type: &crate::ast::DataTypeAst,
    start: &Expr,
    end: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let bq_ty = elem_type.to_bq_type()?;
    let rt = match bq_ty {
        crate::types::BqType::Date => crate::types::RangeElementType::Date,
        crate::types::BqType::Datetime => crate::types::RangeElementType::Datetime,
        crate::types::BqType::Timestamp => crate::types::RangeElementType::Timestamp,
        crate::types::BqType::Range(inner) => inner,
        crate::types::BqType::UntypedNull
        | crate::types::BqType::Bool
        | crate::types::BqType::Int64
        | crate::types::BqType::Float64
        | crate::types::BqType::Numeric
        | crate::types::BqType::BigNumeric
        | crate::types::BqType::String
        | crate::types::BqType::Bytes
        | crate::types::BqType::Time
        | crate::types::BqType::Interval
        | crate::types::BqType::Json
        | crate::types::BqType::Geography
        | crate::types::BqType::Array(_)
        | crate::types::BqType::Struct(_) => {
            return Err(BqError::query(
                "RANGE literal requires DATE/DATETIME/TIMESTAMP element type",
            ))
        }
    };
    let s_raw = eval_expr(start, row, ctx)?;
    let e_raw = eval_expr(end, row, ctx)?;
    let target = rt.to_bq_type();
    let s_opt = if s_raw.is_null() {
        None
    } else {
        Some(cast_value(s_raw, &target)?)
    };
    let e_opt = if e_raw.is_null() {
        None
    } else {
        Some(cast_value(e_raw, &target)?)
    };
    if let (Some(sv), Some(ev)) = (&s_opt, &e_opt) {
        if matches!(compare_values(sv, ev)?, Some(std::cmp::Ordering::Greater)) {
            return Err(BqError::semantic("RANGE start must not exceed end"));
        }
    }
    Ok(BqValue::Range(Box::new(crate::types::BqRange::new(
        rt, s_opt, e_opt,
    )?)))
}

fn eval_binary_expr(
    op: crate::ast::BinaryOp,
    left: &Expr,
    right: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if matches!(op, crate::ast::BinaryOp::And | crate::ast::BinaryOp::Or) {
        let l = eval_expr(left, row, ctx)?;
        let lb = to_opt_bool(&l)?;
        if (op == crate::ast::BinaryOp::And && lb == Some(false))
            || (op == crate::ast::BinaryOp::Or && lb == Some(true))
        {
            return Ok(BqValue::Bool(lb.unwrap_or(false)));
        }
        let r = eval_expr(right, row, ctx)?;
        let rb = to_opt_bool(&r)?;
        return Ok(combine_logical(op, lb, rb));
    }
    let l = eval_expr(left, row, ctx)?;
    let r = eval_expr(right, row, ctx)?;
    eval_binary(op, l, r)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only parameter and system var Expr variants reach eval_param_or_sysvar"
)]
fn eval_param_or_sysvar(expr: &Expr, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    match expr {
        Expr::Parameter(name) => ctx
            .resolve_param(name)
            .cloned()
            .ok_or_else(|| BqError::query(format!("Query parameter @{name} not provided"))),
        Expr::PositionalParam(idx) => ctx
            .positional_params
            .get(idx.saturating_sub(1))
            .cloned()
            .ok_or_else(|| BqError::query(format!("Positional parameter ?{idx} not provided"))),
        Expr::SystemVar(name) => ctx
            .sys_vars
            .get(&name.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| BqError::query(format!("Unknown system variable @@{name}"))),
        _ => Err(BqError::internal("unexpected expr in eval_param_or_sysvar")),
    }
}

fn eval_interval_expr(
    value: &Expr,
    unit: &str,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(value, row, ctx)?;
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Int64(n) = val else {
        return Err(BqError::query("INTERVAL quantity must be INT64"));
    };
    let n32 = i32::try_from(n).map_err(|_| BqError::semantic("INTERVAL quantity out of range"))?;
    let iv = match unit.to_ascii_uppercase().as_str() {
        "YEAR" => crate::types::BqInterval::new(n32.saturating_mul(12), 0, 0),
        "QUARTER" => crate::types::BqInterval::new(n32.saturating_mul(3), 0, 0),
        "MONTH" => crate::types::BqInterval::new(n32, 0, 0),
        "WEEK" => crate::types::BqInterval::new(0, n32.saturating_mul(7), 0),
        "DAY" => crate::types::BqInterval::new(0, n32, 0),
        "HOUR" => crate::types::BqInterval::new(0, 0, n.saturating_mul(3_600_000_000)),
        "MINUTE" => crate::types::BqInterval::new(0, 0, n.saturating_mul(60_000_000)),
        "SECOND" => crate::types::BqInterval::new(0, 0, n.saturating_mul(1_000_000)),
        "MILLISECOND" => crate::types::BqInterval::new(0, 0, n.saturating_mul(1_000)),
        "MICROSECOND" => crate::types::BqInterval::new(0, 0, n),
        _ => {
            return Err(BqError::query(format!(
                "Unsupported INTERVAL unit '{unit}'"
            )))
        }
    };
    Ok(BqValue::Interval(iv))
}

fn eval_unary_expr(
    op: UnaryOp,
    expr: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(expr, row, ctx)?;
    eval_unary(op, val)
}

fn resolve_single_ident(name: &str, row: &EvalRow, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    if let Some(binding) = row.resolve_ident(name)? {
        return Ok(binding.value.clone());
    }
    if let Some(var_val) = ctx.vars.get(&name.to_ascii_lowercase()) {
        return Ok(var_val.clone());
    }
    if let Some(outer) = ctx.outer_row {
        if let Some(binding) = outer.resolve_ident(name)? {
            return Ok(binding.value.clone());
        }
    }
    Err(BqError::query(format!("Unrecognized name: {name}")))
}

fn resolve_compound_ident(
    parts: &[Ident],
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [first, rest @ ..] = parts else {
        return Err(BqError::query("Empty compound identifier"));
    };
    if let Some(second) = rest.first() {
        let tail = rest.get(1..).unwrap_or(&[]);
        if let Some(val) = resolve_qualified_from_row(first, second, tail, row)? {
            return Ok(val);
        }
        if let Some(outer) = ctx.outer_row {
            if let Some(val) = resolve_qualified_from_row(first, second, tail, outer)? {
                return Ok(val);
            }
        }
    }
    let mut current = resolve_single_ident(&first.value, row, ctx)?;
    for field in rest {
        current = access_struct_field(&current, &field.value)?;
    }
    Ok(current)
}

fn resolve_qualified_from_row(
    first: &Ident,
    second: &Ident,
    tail: &[Ident],
    row: &EvalRow,
) -> BqResult<Option<BqValue>> {
    let Some(binding) = row.resolve_qualified(&first.value, &second.value) else {
        return Ok(None);
    };
    let mut current = binding.value.clone();
    for field in tail {
        current = access_struct_field(&current, &field.value)?;
    }
    Ok(Some(current))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Struct and Json support field access"
)]
pub(super) fn access_struct_field(base: &BqValue, field: &str) -> BqResult<BqValue> {
    match base {
        BqValue::Null => Ok(BqValue::Null),
        BqValue::Struct(fields) => fields
            .iter()
            .find(|f| {
                f.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(field))
            })
            .map(|f| f.value.clone())
            .ok_or_else(|| BqError::query(format!("Field name {field} does not exist in STRUCT"))),
        BqValue::Json(j) => Ok(j
            .get(field)
            .map_or(BqValue::Null, |v| BqValue::Json(Box::new(v.clone())))),
        other => Err(BqError::query(format!(
            "Cannot access field '{field}' on {}",
            other.infer_type()?
        ))),
    }
}

fn eval_between(
    expr: &Expr,
    negated: bool,
    low: &Expr,
    high: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let v = eval_expr(expr, row, ctx)?;
    let l = eval_expr(low, row, ctx)?;
    let h = eval_expr(high, row, ctx)?;
    let ge_low = eval_binary(crate::ast::BinaryOp::Gte, v.clone(), l)?;
    let le_high = eval_binary(crate::ast::BinaryOp::Lte, v, h)?;
    let in_range = eval_binary(crate::ast::BinaryOp::And, ge_low, le_high)?;
    if negated {
        eval_unary(UnaryOp::Not, in_range)
    } else {
        Ok(in_range)
    }
}

fn eval_like_expr(
    expr: &Expr,
    negated: bool,
    pattern: &Expr,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(expr, row, ctx)?;
    let pat = eval_expr(pattern, row, ctx)?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query("LIKE requires STRING operands"));
    };
    let matched = ops::eval_like(&s, &p);
    Ok(BqValue::Bool(if negated { !matched } else { matched }))
}

fn eval_cast_expr(
    expr: &Expr,
    ty: &crate::ast::DataTypeAst,
    safe: bool,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr(expr, row, ctx)?;
    let target = ty.to_bq_type()?;
    match cast_value(val, &target) {
        Ok(v) => Ok(v),
        Err(err) if safe && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}

fn eval_case_expr(
    operand: Option<&Expr>,
    whens: &[WhenBranch],
    else_result: Option<&Expr>,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let op_val = operand.map(|e| eval_expr(e, row, ctx)).transpose()?;
    for branch in whens {
        let cond = eval_expr(&branch.condition, row, ctx)?;
        let matched = match &op_val {
            Some(ov) => !ov.is_null() && !cond.is_null() && values_equal(ov, &cond)?,
            None => matches!(cond, BqValue::Bool(true)),
        };
        if matched {
            return eval_expr(&branch.result, row, ctx);
        }
    }
    else_result.map_or(Ok(BqValue::Null), |e| eval_expr(e, row, ctx))
}
