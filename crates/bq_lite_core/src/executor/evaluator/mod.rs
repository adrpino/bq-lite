mod calls;
mod cast_fmt;
mod lambda;
mod literals;
mod ops;
mod subquery;
mod subquery_cache;

use crate::ast::{Expr, Ident, UnaryOp};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowRef};
use crate::types::coercion::cast_value;
use crate::types::BqValue;

use calls::{eval_array_literal, eval_function_expr, eval_struct_literal};
pub(super) use calls::{is_date_part_arg_pos, lookup_udf_return_type};
pub(super) use lambda::infer_expr_col_name;
pub(super) use ops::{
    combine_logical, compare_values, eval_binary, eval_is_test, eval_unary, to_opt_bool,
    values_distinct_eq, values_equal,
};
pub(in crate::executor) use subquery::coerce_to_expr_supertype;
use subquery::{eval_case_expr, eval_extract, eval_in_expr, eval_subscript, eval_with_expr};
pub(in crate::executor) use subquery_cache::SubqueryCache;
use subquery_cache::{
    eval_array_subquery_cached, eval_exists_subquery, eval_scalar_subquery_cached,
};

pub(super) fn eval_subscript_values(
    base: BqValue,
    kind: crate::ast::ArrayAccessKind,
    idx_val: BqValue,
) -> BqResult<BqValue> {
    subquery::eval_subscript_values(base, kind, idx_val)
}

pub(super) fn eval_extract_value(
    part: &str,
    val: BqValue,
    tz_val: Option<&BqValue>,
) -> BqResult<BqValue> {
    subquery::eval_extract_value(part, val, tz_val)
}

pub(super) fn eval_in_values(
    lhs: &BqValue,
    negated: bool,
    candidates: &[BqValue],
) -> BqResult<BqValue> {
    subquery::eval_in_values(lhs, negated, candidates)
}

pub(super) fn like_match_values(text: &str, pattern: &str) -> bool {
    literals::eval_like(text, pattern)
}

pub(super) fn eval_expr(expr: &Expr, row: &EvalRow, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    eval_expr_ref(expr, RowRef::Single(row), ctx)
}

#[expect(
    clippy::too_many_lines,
    reason = "Single dispatch table over all Expr AST variants"
)]
pub(super) fn eval_expr_ref(
    expr: &Expr,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    match expr {
        Expr::Literal(lit) => literals::eval_literal(lit),
        Expr::Ident(id) => resolve_single_ident(&id.value, row, ctx),
        Expr::CompoundIdent(parts) => resolve_compound_ident(parts, row, ctx),
        Expr::Parameter(_) | Expr::PositionalParam(_) | Expr::SystemVar(_) => {
            eval_param_or_sysvar(expr, ctx)
        }
        Expr::Grouped(inner) => eval_expr_ref(inner, row, ctx),
        Expr::Unary { op, expr } => eval_unary_expr(*op, expr, row, ctx),
        Expr::Binary { left, op, right } => eval_binary_expr(*op, left, right, row, ctx),
        Expr::Is { expr, test } => Ok(eval_is_test(&eval_expr_ref(expr, row, ctx)?, *test)),
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
        } => eval_in_expr(expr, *negated, target, row.as_eval_row().as_ref(), ctx),
        Expr::Cast {
            expr,
            ty,
            safe,
            format,
            time_zone,
        } => eval_cast_expr(
            (expr, ty, *safe, format.as_deref(), time_zone.as_deref()),
            row,
            ctx,
        ),
        Expr::Case {
            operand,
            whens,
            else_result,
        } => eval_case_expr(
            operand.as_deref(),
            whens,
            else_result.as_deref(),
            row.as_eval_row().as_ref(),
            ctx,
        ),
        Expr::Function(call) => eval_function_expr(call, row.as_eval_row().as_ref(), ctx),
        Expr::ArrayLiteral {
            elem_type,
            elements,
        } => eval_array_literal(
            elem_type.as_ref(),
            elements,
            row.as_eval_row().as_ref(),
            ctx,
        ),
        Expr::StructLiteral {
            explicit_type,
            fields,
        } => eval_struct_literal(
            explicit_type.as_ref(),
            fields,
            row.as_eval_row().as_ref(),
            ctx,
        ),
        Expr::Subscript { expr, kind, index } => {
            eval_subscript(expr, *kind, index, row.as_eval_row().as_ref(), ctx)
        }
        Expr::FieldAccess { expr, field } => {
            access_struct_field(&eval_expr_ref(expr, row, ctx)?, &field.value)
        }
        Expr::Extract {
            part,
            expr,
            time_zone,
        } => eval_extract(
            &part.value,
            expr,
            time_zone.as_deref(),
            row.as_eval_row().as_ref(),
            ctx,
        ),
        Expr::Exists(q) => eval_exists_subquery(q, row.as_eval_row().as_ref(), ctx),
        Expr::Subquery(q) => eval_scalar_subquery_cached(q, row.as_eval_row().as_ref(), ctx),
        Expr::ArraySubquery(q) => eval_array_subquery_cached(q, row.as_eval_row().as_ref(), ctx),
        Expr::Interval {
            value,
            unit,
            end_unit,
        } => eval_interval_expr(
            value,
            &unit.value,
            end_unit.as_ref().map(|e| e.value.as_str()),
            row,
            ctx,
        ),
        Expr::RangeLiteral {
            elem_type,
            start,
            end,
        } => literals::eval_range_literal(elem_type, start, end, row.as_eval_row().as_ref(), ctx),
        Expr::WithExpr { bindings, body } => {
            eval_with_expr(bindings, body, row.as_eval_row().as_ref(), ctx)
        }
        Expr::Star | Expr::QualifiedStar(_) | Expr::ExprStar(_) => {
            Err(BqError::query("Unexpected '*' in expression"))
        }
        Expr::Lambda { .. } => Err(BqError::query(
            "Lambda expression is only valid inside ARRAY_TRANSFORM / ARRAY_FILTER",
        )),
    }
}

fn eval_binary_expr(
    op: crate::ast::BinaryOp,
    left: &Expr,
    right: &Expr,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if matches!(op, crate::ast::BinaryOp::And | crate::ast::BinaryOp::Or) {
        let l = eval_expr_ref(left, row, ctx)?;
        let lb = to_opt_bool(&l)?;
        if (op == crate::ast::BinaryOp::And && lb == Some(false))
            || (op == crate::ast::BinaryOp::Or && lb == Some(true))
        {
            return Ok(BqValue::Bool(lb.unwrap_or(false)));
        }
        let r = eval_expr_ref(right, row, ctx)?;
        let rb = to_opt_bool(&r)?;
        return Ok(combine_logical(op, lb, rb));
    }
    let mut l = eval_expr_ref(left, row, ctx)?;
    let mut r = eval_expr_ref(right, row, ctx)?;
    if matches!(
        op,
        crate::ast::BinaryOp::Eq
            | crate::ast::BinaryOp::Neq
            | crate::ast::BinaryOp::Lt
            | crate::ast::BinaryOp::Lte
            | crate::ast::BinaryOp::Gt
            | crate::ast::BinaryOp::Gte
    ) {
        let is_lit_str = |e: &Expr| matches!(e, Expr::Literal(crate::ast::Literal::String(_)));
        let is_temporal = |v: &BqValue| {
            matches!(
                v,
                BqValue::Date(_) | BqValue::Datetime(_) | BqValue::Time(_) | BqValue::Timestamp(_)
            )
        };
        if is_lit_str(right) && is_temporal(&l) {
            r = cast_value(r, &l.infer_type()?)?;
        } else if is_lit_str(left) && is_temporal(&r) {
            l = cast_value(l, &r.infer_type()?)?;
        }
    }
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
    end_unit: Option<&str>,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr_ref(value, row, ctx)?;
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    if let BqValue::String(s) = val {
        let iv = crate::executor::scalar::parse_compound_interval(&s, unit, end_unit)?;
        return Ok(BqValue::Interval(iv));
    }
    let BqValue::Int64(n) = val else {
        return Err(BqError::query("INTERVAL quantity must be INT64 or STRING"));
    };
    let iv = crate::executor::scalar::single_unit_interval(n, &unit.to_ascii_uppercase())?;
    Ok(BqValue::Interval(iv))
}

fn eval_unary_expr(
    op: UnaryOp,
    expr: &Expr,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr_ref(expr, row, ctx)?;
    eval_unary(op, val)
}

fn resolve_single_ident(name: &str, row: RowRef<'_>, ctx: &ExecContext<'_>) -> BqResult<BqValue> {
    if let Some(binding) = row.resolve_ident(name)? {
        return Ok(binding.value.clone());
    }
    if let Some(struct_val) = row.resolve_range_var_struct(name) {
        return Ok(struct_val);
    }
    if let Some(var_val) = ctx.vars.get(&name.to_ascii_lowercase()) {
        return Ok(var_val.clone());
    }
    if let Some(const_val) = ctx.catalog.get_constant(name) {
        return Ok(const_val.clone());
    }
    for outer in ctx.outer_rows.iter().rev() {
        match outer.row.resolve_ident(name) {
            Ok(Some(binding)) => {
                outer.mark_touched();
                return Ok(binding.value.clone());
            }
            Err(err) => {
                outer.mark_touched();
                return Err(err);
            }
            Ok(None) => {}
        }
        if let Some(struct_val) = outer.row.resolve_range_var_struct(name) {
            outer.mark_touched();
            return Ok(struct_val);
        }
    }
    match name.to_ascii_uppercase().as_str() {
        "CURRENT_DATE" => return Ok(BqValue::Date(ctx.now.date_naive())),
        "CURRENT_DATETIME" => return Ok(BqValue::Datetime(ctx.now.naive_utc())),
        "CURRENT_TIME" => return Ok(BqValue::Time(ctx.now.time())),
        "CURRENT_TIMESTAMP" => return Ok(BqValue::Timestamp(ctx.now)),
        _ => {}
    }
    Err(BqError::query(format!("Unrecognized name: {name}")))
}

fn resolve_compound_ident(
    parts: &[Ident],
    row: RowRef<'_>,
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
        for outer in ctx.outer_rows.iter().rev() {
            match resolve_qualified_from_row(first, second, tail, RowRef::Single(outer.row)) {
                Ok(Some(val)) => {
                    outer.mark_touched();
                    return Ok(val);
                }
                Err(err) => {
                    outer.mark_touched();
                    return Err(err);
                }
                Ok(None) => {}
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
    row: RowRef<'_>,
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
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut v = eval_expr_ref(expr, row, ctx)?;
    let mut l = eval_expr_ref(low, row, ctx)?;
    let mut h = eval_expr_ref(high, row, ctx)?;
    let is_lit_str = |e: &Expr| matches!(e, Expr::Literal(crate::ast::Literal::String(_)));
    let temporal_ty = [&v, &l, &h].iter().find_map(|val| {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "Only temporal scalar variants trigger string-literal coercion in BETWEEN"
        )]
        match val {
            BqValue::Date(_) => Some(crate::types::BqType::Date),
            BqValue::Datetime(_) => Some(crate::types::BqType::Datetime),
            BqValue::Time(_) => Some(crate::types::BqType::Time),
            BqValue::Timestamp(_) => Some(crate::types::BqType::Timestamp),
            _ => None,
        }
    });
    if let Some(ref tty) = temporal_ty {
        if is_lit_str(expr) && !v.is_null() {
            v = cast_value(v, tty)?;
        }
        if is_lit_str(low) && !l.is_null() {
            l = cast_value(l, tty)?;
        }
        if is_lit_str(high) && !h.is_null() {
            h = cast_value(h, tty)?;
        }
    }
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
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let val = eval_expr_ref(expr, row, ctx)?;
    let pat = eval_expr_ref(pattern, row, ctx)?;
    if val.is_null() || pat.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::String(s), BqValue::String(p)) = (val, pat) else {
        return Err(BqError::query("LIKE requires STRING operands"));
    };
    let matched = literals::eval_like(&s, &p);
    Ok(BqValue::Bool(if negated { !matched } else { matched }))
}

fn eval_cast_expr(
    cast: (
        &Expr,
        &crate::ast::DataTypeAst,
        bool,
        Option<&Expr>,
        Option<&Expr>,
    ),
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let (expr, ty, safe, format, time_zone) = cast;
    let val = eval_expr_ref(expr, row, ctx)?;
    let fmt_val = format.map(|f| eval_expr_ref(f, row, ctx)).transpose()?;
    let tz_val = time_zone.map(|z| eval_expr_ref(z, row, ctx)).transpose()?;
    match cast_fmt::eval_cast_with_format(val, ty, fmt_val, tz_val) {
        Ok(v) => Ok(v),
        Err(err) if safe && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}
