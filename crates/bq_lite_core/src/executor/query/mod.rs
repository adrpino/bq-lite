mod agg;
mod agg_approx;
mod agg_builtins;
mod agg_eval;
mod agg_grouping;
mod cte;
mod exec_util;
mod from;
mod governance;
mod joins;
mod joins_hash;
mod pipe;
mod pipe_ops;
mod pivot;
mod project;
mod schema;
mod set_ops;
mod sort;
mod tvf;
mod type_infer;
mod window;
mod window_fns;
mod window_frames;

use crate::ast::{Expr, OrderByExpr, Query, Select, SelectAsKind, SetExpr};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::eval_expr;
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::{BqType, BqValue};

pub(in crate::executor) use self::project::project_eval_rows as project_eval_rows_public;
pub(in crate::executor) use self::type_infer::infer_expr_type as infer_expr_type_public;
pub(in crate::executor) use from::{
    eval_from_clause as eval_from_clause_public,
    eval_table_factor_standalone as eval_table_factor_public,
};

use agg::{execute_grouped_select_with_order, select_uses_grouping};
use cte::eval_with_clause;
use from::eval_from_clause_with_schema;
use pipe::execute_query_with_pipes;
use project::{has_untyped_null, project_eval_rows};
use schema::infer_select_columns;
use set_ops::execute_set_op;
use sort::{apply_limit_offset, apply_order_by, apply_order_by_with_source, merge_proj_and_source};
use window::{apply_windows, execute_grouped_with_windows, select_has_windows};

pub(super) fn execute_query(query: &Query, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let mut local_ctx = ExecContext {
        catalog: ctx.catalog,
        storage: ctx.storage,
        ctes: ctx.ctes.clone(),
        params: ctx.params.clone(),
        positional_params: ctx.positional_params.clone(),
        vars: ctx.vars.clone(),
        sys_vars: ctx.sys_vars.clone(),
        outer_rows: ctx.outer_rows.clone(),
        now: ctx.now,
        governance: ctx.governance.clone(),
    };
    if let Some(ref with) = query.with {
        eval_with_clause(with, &mut local_ctx)?;
    }

    if !query.pipe_ops.is_empty() {
        return execute_query_with_pipes(query, &mut local_ctx);
    }

    if let SetExpr::Select(ref sel) = query.body {
        execute_select_full(
            sel,
            &query.order_by,
            query.limit.as_ref(),
            query.offset.as_ref(),
            &local_ctx,
        )
    } else {
        let mut res = execute_set_expr(&query.body, &local_ctx)?;
        if !query.order_by.is_empty() || query.limit.is_some() || query.offset.is_some() {
            let mut eval_rows = EvalRow::from_query_result(None, &res);
            if !query.order_by.is_empty() {
                apply_order_by(&mut eval_rows, &query.order_by, &local_ctx)?;
            }
            apply_limit_offset(
                &mut eval_rows,
                query.limit.as_ref(),
                query.offset.as_ref(),
                &local_ctx,
            )?;
            res.rows = eval_rows
                .into_iter()
                .map(|r| r.cols.into_iter().map(|c| c.value).collect())
                .collect();
        }
        Ok(res)
    }
}

pub(super) fn execute_set_expr(expr: &SetExpr, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    match expr {
        SetExpr::Select(sel) => execute_select(sel, ctx),
        SetExpr::Query(q) => execute_query(q, ctx),
        SetExpr::SetOperation {
            op,
            quantifier,
            correspondence,
            left,
            right,
        } => {
            let l = execute_set_expr(left, ctx)?;
            let r = execute_set_expr(right, ctx)?;
            execute_set_op(*op, *quantifier, *correspondence, l, r)
        }
        SetExpr::Values(rows_expr) => eval_values_expr(rows_expr, ctx),
    }
}

fn eval_values_expr(rows_expr: &[Vec<Expr>], ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let empty = EvalRow::default();
    let mut rows = Vec::with_capacity(rows_expr.len());
    for row_e in rows_expr {
        let vals = row_e
            .iter()
            .map(|e| eval_expr(e, &empty, ctx))
            .collect::<BqResult<Vec<_>>>()?;
        rows.push(vals);
    }
    let cols = rows
        .first()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, v)| {
                    QueryColumn::new(
                        format!("f{i}_"),
                        v.infer_type().unwrap_or(BqType::UntypedNull),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(QueryResult::new(cols, rows))
}

fn execute_select(select: &Select, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    execute_select_full(select, &[], None, None, ctx)
}

fn execute_select_full(
    select: &Select,
    order_by: &[OrderByExpr],
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let (mut rows, schema_row) = eval_from_clause_with_schema(&select.from, ctx)?;
    if let Some(ref pred) = select.where_clause {
        let mut filtered = Vec::with_capacity(rows.len());
        for row in rows {
            if matches!(eval_expr(pred, &row, ctx)?, BqValue::Bool(true)) {
                filtered.push(row);
            }
        }
        rows = filtered;
    }

    let need_src = select.qualify.is_some() || !order_by.is_empty();
    let (mut projected, mut qualify_src, effective_order) = if select_uses_grouping(select) {
        if select_has_windows(select) {
            execute_grouped_with_windows(select, order_by, &rows, &schema_row, ctx)?
        } else {
            execute_grouped_select_with_order(select, order_by, &rows, &schema_row, ctx)?
        }
    } else if select_has_windows(select) {
        let (rewritten, aug_rows) = apply_windows(select, rows, ctx)?;
        let out = project_eval_rows(&rewritten, &aug_rows, ctx)?;
        let (p, s) = if need_src {
            filter_qualify_with_src(&rewritten, out, &aug_rows, ctx)?
        } else {
            (out, Vec::new())
        };
        (p, s, order_by.to_vec())
    } else {
        let out = project_eval_rows(select, &rows, ctx)?;
        let (p, s) = if need_src {
            filter_qualify_with_src(select, out, &rows, ctx)?
        } else {
            drop(rows);
            (out, Vec::new())
        };
        (p, s, order_by.to_vec())
    };

    if select.distinct {
        let (deduped_proj, deduped_src) = dedup_eval_rows_with_sources(&projected, &qualify_src)?;
        projected = deduped_proj;
        qualify_src = deduped_src;
    }

    if !effective_order.is_empty() {
        apply_order_by_with_source(&mut projected, &qualify_src, &effective_order, ctx)?;
    }
    apply_limit_offset(&mut projected, limit, offset, ctx)?;

    build_select_result(select, projected, &schema_row, ctx)
}

fn dedup_eval_rows_with_sources(
    projected: &[EvalRow],
    qualify_src: &[EvalRow],
) -> BqResult<(Vec<EvalRow>, Vec<EvalRow>)> {
    for row in projected {
        for col in &row.cols {
            let ty = col.value.infer_type().unwrap_or_else(|_| col.ty.clone());
            if matches!(ty, BqType::Array(_) | BqType::Json)
                || matches!(col.ty, BqType::Array(_) | BqType::Json)
            {
                return Err(BqError::query(format!(
                    "SELECT DISTINCT is not supported on expressions of type {}",
                    col.ty
                )));
            }
        }
    }
    let mut out_proj: Vec<EvalRow> = Vec::with_capacity(projected.len());
    let mut out_src: Vec<EvalRow> = Vec::with_capacity(qualify_src.len());
    let mut index: std::collections::HashMap<u64, Vec<usize>> =
        std::collections::HashMap::with_capacity(projected.len());
    let mut key_buf: Vec<BqValue> = Vec::new();
    for (idx, prow) in projected.iter().enumerate() {
        key_buf.clear();
        key_buf.extend(prow.cols.iter().map(|c| c.value.clone()));
        let h = exec_util::hash_value_slice(&key_buf);
        let cands = index.entry(h).or_default();
        let mut dup = false;
        for &ex_idx in cands.iter() {
            if let Some(existing) = out_proj.get(ex_idx) {
                if existing.cols.len() == key_buf.len()
                    && existing.cols.iter().zip(&key_buf).all(|(ec, kv)| {
                        crate::executor::evaluator::values_distinct_eq(&ec.value, kv)
                            .unwrap_or(false)
                    })
                {
                    dup = true;
                    break;
                }
            }
        }
        if !dup {
            let new_idx = out_proj.len();
            out_proj.push(prow.clone());
            cands.push(new_idx);
            if let Some(srow) = qualify_src.get(idx) {
                out_src.push(srow.clone());
            }
        }
    }
    Ok((out_proj, out_src))
}

pub(super) fn filter_qualify_with_src(
    select: &Select,
    projected: Vec<EvalRow>,
    source_rows: &[EvalRow],
    ctx: &ExecContext<'_>,
) -> BqResult<(Vec<EvalRow>, Vec<EvalRow>)> {
    let Some(ref qualify) = select.qualify else {
        return Ok((projected, source_rows.to_vec()));
    };
    let mut out_proj = Vec::new();
    let mut out_src = Vec::new();
    for (proj_row, src_row) in projected.into_iter().zip(source_rows.iter()) {
        let combined = merge_proj_and_source(&proj_row, src_row);
        if matches!(eval_expr(qualify, &combined, ctx)?, BqValue::Bool(true)) {
            out_proj.push(proj_row);
            out_src.push(src_row.clone());
        }
    }
    Ok((out_proj, out_src))
}

fn build_select_result(
    select: &Select,
    projected: Vec<EvalRow>,
    schema_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    if projected.is_empty() {
        let cols = infer_select_columns(select, schema_row, ctx);
        return Ok(QueryResult::new(cols, Vec::new()));
    }
    match select.as_kind {
        Some(SelectAsKind::Struct) => {
            let rows = projected
                .into_iter()
                .map(|r| vec![r.to_struct_value()])
                .collect::<Vec<_>>();
            let ty = rows
                .first()
                .and_then(|r| r.first())
                .map_or(Ok(BqType::UntypedNull), BqValue::infer_type)?;
            Ok(QueryResult::new(vec![QueryColumn::new("f0_", ty)], rows))
        }
        Some(SelectAsKind::Value) => {
            let mut rows = Vec::with_capacity(projected.len());
            for r in projected {
                if r.cols.len() != 1 {
                    return Err(BqError::query(
                        "SELECT AS VALUE requires exactly 1 expression",
                    ));
                }
                if let Some(first) = r.cols.into_iter().next() {
                    rows.push(vec![first.value]);
                }
            }
            let ty = rows
                .first()
                .and_then(|r| r.first())
                .map_or(Ok(BqType::UntypedNull), BqValue::infer_type)?;
            Ok(QueryResult::new(vec![QueryColumn::new("f0_", ty)], rows))
        }
        None => build_tabular_result(projected),
    }
}

fn build_tabular_result(projected: Vec<EvalRow>) -> BqResult<QueryResult> {
    let mut cols: Vec<QueryColumn> = projected
        .first()
        .map(|r| {
            r.cols
                .iter()
                .map(|c| QueryColumn::new(c.name.as_ref().to_owned(), c.ty.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (idx, col) in cols.iter_mut().enumerate() {
        for r in &projected {
            if let Some(cell) = r.cols.get(idx) {
                if cell.ty != col.ty {
                    if let Ok(sup) =
                        crate::types::coercion::common_supertype(&[col.ty.clone(), cell.ty.clone()])
                    {
                        col.ty = sup;
                    }
                }
            }
        }
        if has_untyped_null(&col.ty) {
            if let Some(found) = projected
                .iter()
                .filter_map(|r| r.cols.get(idx))
                .find(|c| !has_untyped_null(&c.ty))
            {
                col.ty = found.ty.clone();
            }
        }
    }
    let mut rows = Vec::with_capacity(projected.len());
    for r in projected {
        let mut out_row = Vec::with_capacity(r.cols.len());
        for (c, target_col) in r.cols.into_iter().zip(&cols) {
            let coerced = if !c.value.is_null()
                && c.ty != target_col.ty
                && crate::types::coercion::can_implicitly_coerce(&c.ty, &target_col.ty)
            {
                crate::types::coercion::coerce_value(c.value, &target_col.ty)?
            } else {
                c.value
            };
            out_row.push(coerced);
        }
        rows.push(out_row);
    }
    Ok(QueryResult::new(cols, rows))
}
