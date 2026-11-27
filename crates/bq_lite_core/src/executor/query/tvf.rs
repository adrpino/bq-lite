use crate::ast::{
    CreateFunctionStmt, DataTypeAst, Expr, FunctionArg, FunctionBody, ObjectName, Query,
    RoutineParam, StructFieldAst,
};
use crate::catalog::RoutineMeta;
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::execute_query;
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::coercion::{can_implicitly_coerce, coerce_value};
use crate::types::BqValue;

use super::schema::infer_query_columns;

pub(super) fn eval_table_function(
    name: &ObjectName,
    args: &[FunctionArg],
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<EvalRow>> {
    let (cf, q) = resolve_tvf(name, ctx)?;
    let bound = match_tvf_args(name, &cf.params, args)?;
    let empty_outer = EvalRow::default();
    let mut sub_ctx = ctx.with_outer(&empty_outer);
    for (param, arg) in bound {
        if let DataTypeAst::Table(ref expected_fields) = param.ty {
            let table_q = require_table_arg(param, arg)?;
            let table_qr = execute_query(table_q, ctx)?;
            let validated = validate_and_coerce_tvf_table(expected_fields, table_qr)?;
            sub_ctx
                .ctes
                .insert(param.name.value.to_ascii_lowercase(), validated);
        } else {
            let val = eval_expr(&arg.expr, &EvalRow::default(), ctx)?;
            let target_ty = param.ty.to_bq_type()?;
            let coerced = coerce_value(val, &target_ty)?;
            sub_ctx
                .vars
                .insert(param.name.value.to_ascii_lowercase(), coerced);
        }
    }
    let qr = execute_query(q, &sub_ctx)?;
    Ok(EvalRow::from_query_result(alias, &qr))
}

pub(super) fn eval_table_function_schema(
    name: &ObjectName,
    args: &[FunctionArg],
    alias: Option<&str>,
    ctx: &ExecContext<'_>,
) -> BqResult<EvalRow> {
    let (cf, q) = resolve_tvf(name, ctx)?;
    let bound = match_tvf_args(name, &cf.params, args)?;
    let empty_outer = EvalRow::default();
    let mut sub_ctx = ctx.with_outer(&empty_outer);
    for (param, arg) in bound {
        if let DataTypeAst::Table(ref expected_fields) = param.ty {
            let table_q = require_table_arg(param, arg)?;
            let mut cols = infer_query_columns(table_q, ctx)?;
            validate_tvf_columns(expected_fields, &mut cols)?;
            sub_ctx.ctes.insert(
                param.name.value.to_ascii_lowercase(),
                QueryResult::new(cols, Vec::new()),
            );
        } else {
            let val = eval_expr(&arg.expr, &EvalRow::default(), ctx)?;
            let target_ty = param.ty.to_bq_type()?;
            let coerced = coerce_value(val, &target_ty)?;
            sub_ctx
                .vars
                .insert(param.name.value.to_ascii_lowercase(), coerced);
        }
    }
    let out_cols = infer_query_columns(q, &sub_ctx)?;
    Ok(EvalRow::new(
        out_cols
            .into_iter()
            .map(|c| RowBinding {
                qualifier: alias.map(str::to_owned),
                name: c.name,
                ty: c.ty,
                value: BqValue::Null,
            })
            .collect(),
    ))
}

fn resolve_tvf<'a>(
    name: &ObjectName,
    ctx: &'a ExecContext<'_>,
) -> BqResult<(&'a CreateFunctionStmt, &'a Query)> {
    let tref = ctx.catalog.resolve_table_name(name)?;
    let ds = ctx.catalog.get_dataset(&tref.project, &tref.dataset)?;
    let Some(routine) = ds.routines.get(&tref.table) else {
        return Err(BqError::unimplemented(format!(
            "Table function '{}' is not implemented",
            name.normalized_parts().join(".")
        )));
    };
    let RoutineMeta::Function(cf) = routine.as_ref() else {
        return Err(BqError::query("Routine is not a table function"));
    };
    let FunctionBody::Query(ref q) = cf.body else {
        return Err(BqError::query("Function is not a table function"));
    };
    Ok((cf.as_ref(), q.as_ref()))
}

fn match_tvf_args<'a>(
    name: &ObjectName,
    params: &'a [RoutineParam],
    args: &'a [FunctionArg],
) -> BqResult<Vec<(&'a RoutineParam, &'a FunctionArg)>> {
    if args.len() != params.len() {
        return Err(BqError::query(format!(
            "Table function '{}' expects {} argument(s), got {}",
            name.normalized_parts().join("."),
            params.len(),
            args.len()
        )));
    }
    let mut slots: Vec<Option<&'a FunctionArg>> = vec![None; params.len()];
    let mut seen_named = false;
    for (idx, arg) in args.iter().enumerate() {
        if let Some(ref arg_name) = arg.name {
            seen_named = true;
            let pos = params
                .iter()
                .position(|p| p.name.value.eq_ignore_ascii_case(&arg_name.value))
                .ok_or_else(|| {
                    BqError::query(format!(
                        "Unknown named argument '{}' for table function '{}'",
                        arg_name.value,
                        name.normalized_parts().join(".")
                    ))
                })?;
            let slot = slots
                .get_mut(pos)
                .ok_or_else(|| BqError::query("Internal error matching table function argument"))?;
            if slot.is_some() {
                return Err(BqError::query(format!(
                    "Duplicate argument '{}' for table function '{}'",
                    arg_name.value,
                    name.normalized_parts().join(".")
                )));
            }
            *slot = Some(arg);
        } else {
            if seen_named {
                return Err(BqError::query(
                    "Positional argument cannot follow a named argument",
                ));
            }
            if let Some(slot) = slots.get_mut(idx) {
                *slot = Some(arg);
            }
        }
    }
    params
        .iter()
        .zip(slots)
        .map(|(p, a)| {
            a.map(|arg| (p, arg)).ok_or_else(|| {
                BqError::query(format!(
                    "Missing argument for table function parameter '{}'",
                    p.name.value
                ))
            })
        })
        .collect()
}

fn require_table_arg<'a>(param: &RoutineParam, arg: &'a FunctionArg) -> BqResult<&'a Query> {
    let Expr::Subquery(ref table_q) = arg.expr else {
        return Err(BqError::query(format!(
            "Table-valued parameter '{}' expects TABLE <table> or (SELECT ...)",
            param.name.value
        )));
    };
    Ok(table_q.as_ref())
}

fn validate_tvf_columns(
    expected_fields: &[StructFieldAst],
    actual_cols: &mut [QueryColumn],
) -> BqResult<Vec<(usize, crate::types::BqType)>> {
    let mut coercions = Vec::new();
    for field in expected_fields {
        let Some(ref fname) = field.name else {
            continue;
        };
        let target_ty = field.ty.to_bq_type()?;
        let col_idx = actual_cols
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&fname.value))
            .ok_or_else(|| {
                BqError::query(format!(
                    "Input table argument is missing required column '{}'",
                    fname.value
                ))
            })?;
        if let Some(col) = actual_cols.get_mut(col_idx) {
            if !can_implicitly_coerce(&col.ty, &target_ty) {
                return Err(BqError::query(format!(
                    "Input table column '{}' has type {}, expected {}",
                    fname.value, col.ty, target_ty
                )));
            }
            if col.ty != target_ty {
                col.ty = target_ty.clone();
                coercions.push((col_idx, target_ty));
            }
        }
    }
    Ok(coercions)
}

fn validate_and_coerce_tvf_table(
    expected_fields: &[StructFieldAst],
    mut actual: QueryResult,
) -> BqResult<QueryResult> {
    let coercions = validate_tvf_columns(expected_fields, &mut actual.columns)?;
    for row in &mut actual.rows {
        for (col_idx, target_ty) in &coercions {
            if let Some(cell) = row.get_mut(*col_idx) {
                let prev = std::mem::replace(cell, BqValue::Null);
                *cell = coerce_value(prev, target_ty)?;
            }
        }
    }
    Ok(actual)
}
