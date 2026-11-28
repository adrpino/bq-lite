use crate::ast::{Expr, FunctionCall, StructFieldExpr};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowRef};
use crate::executor::scalar::{eval_scalar_builtin, is_scalar_registered};
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::{BqStructFieldValue, BqType, BqValue};

use super::lambda::infer_expr_col_name;
use super::{coerce_to_expr_supertype, eval_expr, eval_expr_ref};

pub(super) fn eval_function_expr(
    call: &FunctionCall,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if call.over.is_some() {
        return Err(BqError::query(
            "Window function cannot be evaluated outside SELECT/QUALIFY window pass",
        ));
    }
    let mut stack_buf = [0_u8; 40];
    let (owned_fn_name, owned_upper);
    let (fn_name, upper): (&str, &str) = match call.name.parts.as_slice() {
        [single] if single.value.len() <= stack_buf.len() && single.value.is_ascii() => {
            let s = single.value.as_str();
            for (d, b) in stack_buf.iter_mut().zip(s.as_bytes()) {
                *d = b.to_ascii_uppercase();
            }
            (
                s,
                std::str::from_utf8(stack_buf.get(..s.len()).unwrap_or(&[])).unwrap_or(""),
            )
        }
        [single] => {
            owned_upper = single.value.to_ascii_uppercase();
            (single.value.as_str(), owned_upper.as_str())
        }
        _ => {
            owned_fn_name = call.name.normalized_parts().join(".");
            owned_upper = owned_fn_name.to_ascii_uppercase();
            (owned_fn_name.as_str(), owned_upper.as_str())
        }
    };
    if matches!(
        upper,
        "ARRAY_TRANSFORM" | "ARRAY_FILTER" | "ARRAY_INCLUDES" | "ARRAY_FIND" | "ARRAY_OFFSET"
    ) && call
        .args
        .get(1)
        .is_some_and(|a| matches!(a.expr, Expr::Lambda { .. }))
    {
        return super::lambda::eval_array_lambda_call(upper, call, row.as_eval_row().as_ref(), ctx);
    }
    if matches!(upper, "IF" | "COALESCE" | "IFNULL" | "NULLIF" | "RANGE") {
        return eval_short_circuit_fn(upper, call, row, ctx);
    }
    if upper == "MAKE_INTERVAL" && call.args.iter().any(|a| a.name.is_some()) {
        return super::lambda::eval_make_interval_named(
            fn_name,
            call,
            row.as_eval_row().as_ref(),
            ctx,
        );
    }
    let mut args = Vec::with_capacity(call.args.len());
    for (idx, arg) in call.args.iter().enumerate() {
        if is_date_part_arg_pos(upper, idx) {
            if let Some(part_str) = extract_date_part_arg(&arg.expr) {
                args.push(BqValue::String(part_str));
                continue;
            }
        }
        args.push(eval_expr_ref(&arg.expr, row, ctx)?);
    }
    let res = if is_scalar_registered(upper) {
        eval_scalar_builtin(upper, args, ctx).and_then(|opt| {
            opt.ok_or_else(|| {
                BqError::unimplemented(format!("Function '{fn_name}' is not implemented"))
            })
        })
    } else {
        eval_user_scalar_udf(call, &args, ctx)
    };
    match res {
        Ok(val) => Ok(val),
        Err(err) if call.safe_prefix && err.is_safe_suppressible() => Ok(BqValue::Null),
        Err(err) => Err(err),
    }
}

fn eval_short_circuit_fn(
    upper: &str,
    call: &FunctionCall,
    row: RowRef<'_>,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let owned = row.as_eval_row();
    let res = match upper {
        "IF" => eval_if_short_circuit(call, owned.as_ref(), ctx),
        "COALESCE" => eval_coalesce_short_circuit(call, owned.as_ref(), ctx),
        "IFNULL" => eval_ifnull_short_circuit(call, owned.as_ref(), ctx),
        "NULLIF" => eval_nullif_typed(call, owned.as_ref(), ctx),
        _ => eval_range_call_typed(call, owned.as_ref(), ctx),
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
    reason = "Only Ident and WEEK(dow) function calls represent date parts"
)]
fn extract_date_part_arg(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.value.clone()),
        Expr::Function(fc)
            if fc.name.normalized_parts().len() == 1
                && fc
                    .name
                    .normalized_parts()
                    .first()
                    .is_some_and(|n| n.eq_ignore_ascii_case("WEEK"))
                && fc.args.len() == 1 =>
        {
            if let Some(Expr::Ident(dow)) = fc.args.first().map(|a| &a.expr) {
                return Some(format!("WEEK({})", dow.value.to_ascii_uppercase()));
            }
            None
        }
        _ => None,
    }
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
    let val = match cond {
        BqValue::Bool(true) => eval_expr(&t_arg.expr, row, ctx)?,
        BqValue::Bool(false) | BqValue::Null => eval_expr(&f_arg.expr, row, ctx)?,
        other => {
            return Err(BqError::query(format!(
                "IF condition must be BOOL, got {}",
                other.infer_type()?
            )))
        }
    };
    coerce_to_expr_supertype(val, &[&t_arg.expr, &f_arg.expr], row, ctx)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only Date, Datetime, and Timestamp are valid RANGE element types"
)]
fn eval_range_call_typed(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [s_arg, e_arg] = call.args.as_slice() else {
        return Err(BqError::query("RANGE expects 2 arguments"));
    };
    let start = eval_expr(&s_arg.expr, row, ctx)?;
    let end = eval_expr(&e_arg.expr, row, ctx)?;
    if start.is_null() && end.is_null() {
        let t1 = crate::executor::query::infer_expr_type_public(&s_arg.expr, row, ctx);
        let t2 = crate::executor::query::infer_expr_type_public(&e_arg.expr, row, ctx);
        let inferred = if matches!(t1, BqType::UntypedNull) {
            t2
        } else {
            t1
        };
        let rt = match inferred {
            BqType::Date => crate::types::RangeElementType::Date,
            BqType::Datetime => crate::types::RangeElementType::Datetime,
            BqType::Timestamp => crate::types::RangeElementType::Timestamp,
            _ => {
                return Err(BqError::query(
                    "RANGE(NULL, NULL) requires explicit RANGE<T> literal",
                ))
            }
        };
        return Ok(BqValue::Range(Box::new(crate::types::BqRange::new(
            rt, None, None,
        )?)));
    }
    eval_scalar_builtin("RANGE", vec![start, end], ctx)?
        .ok_or_else(|| BqError::internal("RANGE scalar missing"))
}

fn eval_coalesce_short_circuit(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let branch_exprs: Vec<&Expr> = call.args.iter().map(|a| &a.expr).collect();
    for arg in &call.args {
        let val = eval_expr(&arg.expr, row, ctx)?;
        if !val.is_null() {
            return coerce_to_expr_supertype(val, &branch_exprs, row, ctx);
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
    let val = if a.is_null() {
        eval_expr(&b_arg.expr, row, ctx)?
    } else {
        a
    };
    coerce_to_expr_supertype(val, &[&a_arg.expr, &b_arg.expr], row, ctx)
}

fn eval_nullif_typed(
    call: &FunctionCall,
    row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    let [a_arg, b_arg] = call.args.as_slice() else {
        return Err(BqError::query("NULLIF expects 2 arguments"));
    };
    let a = eval_expr(&a_arg.expr, row, ctx)?;
    let b = eval_expr(&b_arg.expr, row, ctx)?;
    let val = if a.is_null() || b.is_null() {
        a
    } else if super::values_equal(&a, &b)? {
        BqValue::Null
    } else {
        a
    };
    coerce_to_expr_supertype(val, &[&a_arg.expr, &b_arg.expr], row, ctx)
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
            ctx.mark_impure();
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
        None => infer_array_elem_type(&types, elements)?,
    };
    if matches!(target_elem, BqType::Array(_)) {
        return Err(BqError::query("Arrays of arrays are not supported"));
    }
    let coerced = values
        .into_iter()
        .zip(elements.iter())
        .map(|(v, el)| {
            let is_str_lit = matches!(el, Expr::Literal(crate::ast::Literal::String(_)));
            let is_float_lit = matches!(el, Expr::Literal(crate::ast::Literal::Float(_)))
                || matches!(
                    el,
                    Expr::Unary { expr: inner, .. }
                        if matches!(inner.as_ref(), Expr::Literal(crate::ast::Literal::Float(_)))
                );
            if (is_str_lit
                && matches!(
                    target_elem,
                    BqType::Date | BqType::Datetime | BqType::Time | BqType::Timestamp
                ))
                || (is_float_lit && matches!(target_elem, BqType::Numeric | BqType::BigNumeric))
            {
                crate::types::coercion::cast_value(v, &target_elem)
            } else {
                coerce_value(v, &target_elem)
            }
        })
        .collect::<BqResult<Vec<_>>>()?;
    Ok(BqValue::Array(coerced))
}

fn infer_array_elem_type(types: &[BqType], elements: &[Expr]) -> BqResult<BqType> {
    if let Ok(t) = common_supertype(types) {
        return Ok(t);
    }
    let temporal = types.iter().find(|t| {
        matches!(
            t,
            BqType::Date | BqType::Datetime | BqType::Time | BqType::Timestamp
        )
    });
    if let Some(temp_ty) = temporal {
        let adjusted: Vec<BqType> = types
            .iter()
            .zip(elements.iter())
            .map(|(t, el)| {
                if *t == BqType::String
                    && matches!(el, Expr::Literal(crate::ast::Literal::String(_)))
                {
                    temp_ty.clone()
                } else {
                    t.clone()
                }
            })
            .collect();
        return common_supertype(&adjusted);
    }
    common_supertype(types)
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
