use crate::ast::{Expr, FunctionCall, StructFieldExpr};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::scalar::eval_scalar_builtin;
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::{BqStructFieldValue, BqType, BqValue};

use super::eval_expr;

pub(super) fn eval_function_expr(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if call.over.is_some() {
        return Err(BqError::query(
            "Window function cannot be evaluated outside SELECT/QUALIFY window pass",
        ));
    }
    let fn_name = call.name.normalized_parts().join(".");
    let upper = fn_name.to_ascii_uppercase();
    let short_res = match upper.as_str() {
        "ARRAY_TRANSFORM" | "ARRAY_FILTER" => Some(eval_array_lambda(&upper, call, row, ctx)),
        "IF" => Some(eval_if_short_circuit(call, row, ctx)),
        "COALESCE" => Some(eval_coalesce_short_circuit(call, row, ctx)),
        "IFNULL" => Some(eval_ifnull_short_circuit(call, row, ctx)),
        _ => None,
    };
    if let Some(res) = short_res {
        return match res {
            Ok(val) => Ok(val),
            Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
            Err(err) => Err(err),
        };
    }
    if upper == "MAKE_INTERVAL" && call.args.iter().any(|a| a.name.is_some()) {
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
        return match eval_scalar_builtin(&fn_name, slots, ctx) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Ok(BqValue::Null),
            Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
            Err(err) => Err(err),
        };
    }
    let mut args = Vec::with_capacity(call.args.len());
    for (idx, arg) in call.args.iter().enumerate() {
        if is_date_part_arg_pos(&upper, idx) {
            if let Expr::Ident(id) = &arg.expr {
                args.push(BqValue::String(id.value.clone()));
                continue;
            }
        }
        args.push(eval_expr(&arg.expr, row, ctx)?);
    }
    let res = match eval_scalar_builtin(&fn_name, args.clone(), ctx) {
        Ok(Some(val)) => Ok(val),
        Ok(None) => eval_user_scalar_udf(call, &args, ctx),
        Err(err) => Err(err),
    };
    match res {
        Ok(val) => Ok(val),
        Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}

pub(in crate::executor) fn is_date_part_arg_pos(upper_fn: &str, idx: usize) -> bool {
    matches!(
        (upper_fn, idx),
        (
            "DATE_TRUNC" | "DATETIME_TRUNC" | "TIMESTAMP_TRUNC" | "TIME_TRUNC" | "LAST_DAY",
            1
        ) | (
            "DATE_DIFF" | "DATETIME_DIFF" | "TIMESTAMP_DIFF" | "TIME_DIFF",
            2
        )
    )
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only BOOL and NULL are valid IF conditions"
)]
fn eval_if_short_circuit(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [cond_arg, t_arg, f_arg] = call.args.as_slice() else {
        return Err(BqError::query("IF expects 3 arguments"));
    };
    let cond = eval_expr(&cond_arg.expr, row, ctx)?;
    match cond {
        BqValue::Bool(true) => eval_expr(&t_arg.expr, row, ctx),
        BqValue::Bool(false) | BqValue::Null => eval_expr(&f_arg.expr, row, ctx),
        other => Err(BqError::query(format!(
            "IF condition must be BOOL, got {}",
            other.infer_type()?
        ))),
    }
}

fn eval_coalesce_short_circuit(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    for arg in &call.args {
        let val = eval_expr(&arg.expr, row, ctx)?;
        if !val.is_null() {
            return Ok(val);
        }
    }
    Ok(BqValue::Null)
}

fn eval_ifnull_short_circuit(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [a_arg, b_arg] = call.args.as_slice() else {
        return Err(BqError::query("IFNULL expects 2 arguments"));
    };
    let a = eval_expr(&a_arg.expr, row, ctx)?;
    if a.is_null() {
        eval_expr(&b_arg.expr, row, ctx)
    } else {
        Ok(a)
    }
}

fn eval_array_lambda(
    upper_fn: &str,
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [arr_arg, lambda_arg] = call.args.as_slice() else {
        return Err(BqError::query(format!(
            "{upper_fn} expects 2 arguments (array, lambda)"
        )));
    };
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
    let mut out = Vec::new();
    for (idx, item) in items.into_iter().enumerate() {
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
        let val = eval_expr(body, row, &sub_ctx)?;
        if upper_fn == "ARRAY_TRANSFORM" {
            out.push(val);
        } else if matches!(val, BqValue::Bool(true)) {
            out.push(item);
        }
    }
    Ok(BqValue::Array(out))
}

pub(in crate::executor) fn lookup_udf_return_type(
    name: &crate::ast::ObjectName,
    ctx: &ExecContext<'_>,
) -> Option<BqType> {
    let routine = resolve_routine_meta(name, ctx)?;
    let crate::catalog::RoutineMeta::Function(cf) = routine.as_ref() else {
        return None;
    };
    cf.returns.as_ref().and_then(|rt| rt.to_bq_type().ok())
}

fn resolve_routine_meta(
    name: &crate::ast::ObjectName,
    ctx: &ExecContext<'_>,
) -> Option<std::sync::Arc<crate::catalog::RoutineMeta>> {
    let parts = name.normalized_parts();
    if let [single] = parts.as_slice() {
        let temp_ref =
            crate::catalog::TableRef::new(ctx.catalog.default_project(), "__temp", single);
        ctx.catalog
            .get_dataset(&temp_ref.project, &temp_ref.dataset)
            .ok()
            .and_then(|ds| ds.routines.get(&temp_ref.table).cloned())
            .or_else(|| {
                ctx.catalog.resolve_table_name(name).ok().and_then(|tr| {
                    ctx.catalog
                        .get_dataset(&tr.project, &tr.dataset)
                        .ok()
                        .and_then(|ds| ds.routines.get(&tr.table).cloned())
                })
            })
    } else {
        ctx.catalog.resolve_table_name(name).ok().and_then(|tr| {
            ctx.catalog
                .get_dataset(&tr.project, &tr.dataset)
                .ok()
                .and_then(|ds| ds.routines.get(&tr.table).cloned())
        })
    }
}

fn eval_user_scalar_udf(
    call: &FunctionCall,
    args: &[BqValue],
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let fn_name = call.name.normalized_parts().join(".");
    let routine = resolve_routine_meta(&call.name, ctx);

    let Some(r) = routine else {
        return Err(BqError::unimplemented(format!(
            "Function '{fn_name}' is not implemented yet"
        )));
    };
    let crate::catalog::RoutineMeta::Function(cf) = r.as_ref() else {
        return Err(BqError::query(format!(
            "'{fn_name}' is not a scalar function"
        )));
    };
    if cf.params.len() != args.len() {
        return Err(BqError::query(format!(
            "Function '{fn_name}' expects {} arguments, got {}",
            cf.params.len(),
            args.len()
        )));
    }
    match &cf.body {
        crate::ast::FunctionBody::Expr(body_expr) => {
            let empty = EvalRow::default();
            let mut sub_ctx = ctx.with_outer(&empty);
            for (param, val) in cf.params.iter().zip(args.iter()) {
                sub_ctx
                    .vars
                    .insert(param.name.value.to_ascii_lowercase(), val.clone());
            }
            let result = eval_expr(body_expr, &empty, &sub_ctx)?;
            match &cf.returns {
                Some(ret_ty) => coerce_value(result, &ret_ty.to_bq_type()?),
                None => Ok(result),
            }
        }
        crate::ast::FunctionBody::Code(js_code) => {
            eval_js_udf_dispatch(&fn_name, &cf.params, args, js_code, cf.returns.as_ref())
        }
        crate::ast::FunctionBody::Query(_) => Err(BqError::query(format!(
            "'{fn_name}' is a table-valued function"
        ))),
    }
}

#[cfg(feature = "js-udf")]
fn eval_js_udf_dispatch(
    _fn_name: &str,
    params: &[crate::ast::RoutineParam],
    args: &[BqValue],
    js_code: &str,
    returns: Option<&crate::ast::DataTypeAst>,
) -> BqResult<BqValue> {
    crate::executor::js_udf::eval_js_udf(params, args, js_code, returns)
}

#[cfg(not(feature = "js-udf"))]
fn eval_js_udf_dispatch(
    fn_name: &str,
    _params: &[crate::ast::RoutineParam],
    _args: &[BqValue],
    _js_code: &str,
    _returns: Option<&crate::ast::DataTypeAst>,
) -> BqResult<BqValue> {
    Err(BqError::unimplemented(format!(
        "Non-SQL UDF '{fn_name}' requires js-udf feature"
    )))
}

pub(super) fn eval_array_literal(
    elem_type: Option<&crate::ast::DataTypeAst>,
    elements: &[Expr],
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut values = Vec::with_capacity(elements.len());
    let mut types = Vec::with_capacity(elements.len());
    for el in elements {
        let v = eval_expr(el, row, ctx)?;
        types.push(v.infer_type()?);
        values.push(v);
    }
    let target_elem = match elem_type {
        Some(ast_ty) => ast_ty.to_bq_type()?,
        None => common_supertype(&types)?,
    };
    if matches!(target_elem, BqType::Array(_)) {
        return Err(BqError::query("Arrays of arrays are not supported"));
    }
    let coerced = values
        .into_iter()
        .map(|v| coerce_value(v, &target_elem))
        .collect::<BqResult<Vec<_>>>()?;
    Ok(BqValue::Array(coerced))
}

pub(super) fn eval_struct_literal(
    explicit_type: Option<&crate::ast::DataTypeAst>,
    fields: &[StructFieldExpr],
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let value = eval_expr(&field.expr, row, ctx)?;
        let name = field
            .name
            .as_ref()
            .map(|id| id.value.clone())
            .or_else(|| infer_expr_col_name(&field.expr));
        out.push(BqStructFieldValue { name, value });
    }
    let val = BqValue::Struct(out);
    if let Some(ast_ty) = explicit_type {
        let target = ast_ty.to_bq_type()?;
        coerce_value(val, &target)
    } else {
        Ok(val)
    }
}

#[must_use]
pub(in crate::executor) fn infer_expr_col_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.value.clone()),
        Expr::CompoundIdent(parts) => parts.last().map(|id| id.value.clone()),
        Expr::FieldAccess { field, .. } => Some(field.value.clone()),
        Expr::Literal(_)
        | Expr::Parameter(_)
        | Expr::PositionalParam(_)
        | Expr::SystemVar(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Binary { .. }
        | Expr::Unary { .. }
        | Expr::Is { .. }
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::In { .. }
        | Expr::Cast { .. }
        | Expr::Case { .. }
        | Expr::Function(_)
        | Expr::ArrayLiteral { .. }
        | Expr::ArraySubquery(_)
        | Expr::StructLiteral { .. }
        | Expr::Subscript { .. }
        | Expr::Extract { .. }
        | Expr::Interval { .. }
        | Expr::RangeLiteral { .. }
        | Expr::Lambda { .. }
        | Expr::Exists(_)
        | Expr::Subquery(_)
        | Expr::Grouped(_) => None,
    }
}
