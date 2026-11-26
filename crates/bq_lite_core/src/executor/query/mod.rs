mod agg;
mod agg_builtins;
mod agg_eval;
mod from;
mod governance;
mod joins;
mod pipe;
mod pivot;
mod project;
mod schema;
mod set_ops;
mod sort;
mod type_infer;
mod window;
mod window_fns;
mod window_frames;

use crate::ast::{
    Expr, OrderByExpr, Query, Select, SelectAsKind, SetExpr, SetQuantifier, WithClause,
};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::eval_expr;
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::{BqType, BqValue};

pub(in crate::executor) use self::project::project_eval_rows as project_eval_rows_public;
pub(in crate::executor) use from::{
    eval_from_clause as eval_from_clause_public,
    eval_table_factor_standalone as eval_table_factor_public,
};

use agg::{execute_grouped_select, select_uses_grouping};
use from::eval_from_clause_with_schema;
use pipe::apply_pipe_ops;
use project::project_eval_rows;
use schema::infer_select_columns;
use set_ops::{dedup_rows, execute_set_op, is_union_set_expr, rows_equal};
use sort::{apply_limit_offset, apply_order_by, apply_order_by_with_source, merge_proj_and_source};
use window::{apply_windows, execute_grouped_with_windows, select_has_windows};

const MAX_CTE_RECURSION: usize = 100;

pub(super) fn execute_query(query: &Query, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let mut local_ctx = ExecContext {
        catalog: ctx.catalog,
        storage: ctx.storage,
        ctes: ctx.ctes.clone(),
        params: ctx.params.clone(),
        positional_params: ctx.positional_params.clone(),
        vars: ctx.vars.clone(),
        sys_vars: ctx.sys_vars.clone(),
        outer_row: ctx.outer_row,
        now: ctx.now,
        governance: ctx.governance.clone(),
    };
    if let Some(ref with) = query.with {
        eval_with_clause(with, &mut local_ctx)?;
    }

    let mut result = if let SetExpr::Select(ref sel) = query.body {
        execute_select_full(
            sel,
            &query.order_by,
            query.limit.as_ref(),
            query.offset.as_ref(),
            &local_ctx,
        )?
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
        res
    };

    if !query.pipe_ops.is_empty() {
        result = apply_pipe_ops(result, &query.pipe_ops, &local_ctx)?;
    }
    Ok(result)
}

fn eval_with_clause(with: &WithClause, ctx: &mut ExecContext<'_>) -> BqResult<()> {
    for cte in &with.ctes {
        let key = cte.name.value.to_ascii_lowercase();
        let mut qr = if with.recursive {
            eval_recursive_cte(cte, ctx)?
        } else {
            execute_query(&cte.query, ctx)?
        };
        if !cte.columns.is_empty() {
            if cte.columns.len() != qr.columns.len() {
                return Err(BqError::query(format!(
                    "CTE '{}' column count mismatch",
                    cte.name.value
                )));
            }
            for (col, alias) in qr.columns.iter_mut().zip(&cte.columns) {
                col.name.clone_from(&alias.value);
            }
        }
        ctx.ctes.insert(key, qr);
    }
    Ok(())
}

fn eval_recursive_cte(cte: &crate::ast::Cte, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let Some((quantifier, base_expr, rec_expr)) = is_union_set_expr(&cte.query.body) else {
        return execute_query(&cte.query, ctx);
    };
    let key = cte.name.value.to_ascii_lowercase();
    let mut accumulated = execute_set_expr(base_expr, ctx)?;
    if quantifier == SetQuantifier::Distinct {
        accumulated.rows = dedup_rows(accumulated.rows)?;
    }
    let mut delta = accumulated.clone();
    let empty_outer = EvalRow::default();
    for _ in 0..MAX_CTE_RECURSION {
        if delta.rows.is_empty() {
            return Ok(accumulated);
        }
        let mut step_ctx = ctx.with_outer(&empty_outer);
        step_ctx.ctes.insert(key.clone(), delta);
        let mut next_delta = execute_set_expr(rec_expr, &step_ctx)?;
        if next_delta.rows.is_empty() {
            return Ok(accumulated);
        }
        if next_delta.columns.len() != accumulated.columns.len() {
            return Err(BqError::query(format!(
                "Recursive CTE '{}' column count mismatch",
                cte.name.value
            )));
        }
        for (dst, src) in next_delta.columns.iter_mut().zip(&accumulated.columns) {
            dst.name.clone_from(&src.name);
        }
        if quantifier == SetQuantifier::Distinct {
            next_delta.rows = filter_new_rows(&accumulated.rows, next_delta.rows)?;
        }
        accumulated.rows.extend(next_delta.rows.clone());
        delta = next_delta;
    }
    Err(BqError::semantic(format!(
        "Recursive CTE '{}' exceeded maximum iteration limit ({MAX_CTE_RECURSION})",
        cte.name.value
    )))
}

fn filter_new_rows(
    existing: &[Vec<BqValue>],
    candidates: Vec<Vec<BqValue>>,
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut fresh = Vec::new();
    for row in candidates {
        let mut dup = false;
        for ex in existing.iter().chain(fresh.iter()) {
            if rows_equal(ex, &row)? {
                dup = true;
                break;
            }
        }
        if !dup {
            fresh.push(row);
        }
    }
    Ok(fresh)
}

fn execute_set_expr(expr: &SetExpr, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    match expr {
        SetExpr::Select(sel) => execute_select(sel, ctx),
        SetExpr::Query(q) => execute_query(q, ctx),
        SetExpr::SetOperation {
            op,
            quantifier,
            by_name,
            left,
            right,
        } => {
            let l = execute_set_expr(left, ctx)?;
            let r = execute_set_expr(right, ctx)?;
            execute_set_op(*op, *quantifier, *by_name, l, r)
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

    let (mut projected, mut qualify_src) = if select_uses_grouping(select) {
        if select_has_windows(select) {
            execute_grouped_with_windows(select, &rows, &schema_row, ctx)?
        } else {
            let grouped = execute_grouped_select(select, &rows, &schema_row, ctx)?;
            (grouped.clone(), grouped)
        }
    } else if select_has_windows(select) {
        let (rewritten, aug_rows) = apply_windows(select, rows, ctx)?;
        let out = project_eval_rows(&rewritten, &aug_rows, ctx)?;
        filter_qualify_with_src(&rewritten, out, &aug_rows, ctx)?
    } else {
        let out = project_eval_rows(select, &rows, ctx)?;
        filter_qualify_with_src(select, out, &rows, ctx)?
    };

    if select.distinct {
        let (deduped_proj, deduped_src) = dedup_eval_rows_with_sources(&projected, &qualify_src)?;
        projected = deduped_proj;
        qualify_src = deduped_src;
    }

    if !order_by.is_empty() {
        apply_order_by_with_source(&mut projected, &qualify_src, order_by, ctx)?;
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
    let mut out_src: Vec<EvalRow> = Vec::with_capacity(projected.len());
    for (idx, prow) in projected.iter().enumerate() {
        let pvals: Vec<BqValue> = prow.cols.iter().map(|c| c.value.clone()).collect();
        let mut dup = false;
        for existing in &out_proj {
            let evals: Vec<BqValue> = existing.cols.iter().map(|c| c.value.clone()).collect();
            if rows_equal(&evals, &pvals)? {
                dup = true;
                break;
            }
        }
        if !dup {
            out_proj.push(prow.clone());
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
        None => {
            let cols = projected
                .first()
                .map(|r| {
                    r.cols
                        .iter()
                        .map(|c| QueryColumn::new(c.name.clone(), c.ty.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let rows = projected
                .into_iter()
                .map(|r| r.cols.into_iter().map(|c| c.value).collect())
                .collect();
            Ok(QueryResult::new(cols, rows))
        }
    }
}
