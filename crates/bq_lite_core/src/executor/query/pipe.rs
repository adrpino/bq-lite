use crate::ast::{PipeOp, Select, SelectItem};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, infer_expr_col_name};
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::BqValue;

use super::agg::execute_grouped_select;
use super::project_eval_rows;
use super::sort::{apply_limit_offset, apply_order_by};

pub(super) fn apply_pipe_ops(
    mut qr: QueryResult,
    ops: &[PipeOp],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    for op in ops {
        qr = apply_single_pipe_op(qr, op, ctx)?;
    }
    Ok(qr)
}

fn apply_single_pipe_op(
    qr: QueryResult,
    op: &PipeOp,
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    match op {
        PipeOp::Where(pred) => pipe_where(qr, pred, ctx),
        PipeOp::Select(items) => pipe_select(&qr, items, ctx),
        PipeOp::Extend(items) => pipe_extend(&qr, items, ctx),
        PipeOp::Set(assignments) => pipe_set(&qr, assignments, ctx),
        PipeOp::Drop(cols) => Ok(pipe_drop(qr, cols)),
        PipeOp::Rename(pairs) => pipe_rename(qr, pairs),
        PipeOp::Aggregate { aggs, group_by } => pipe_aggregate(&qr, aggs, group_by, ctx),
        PipeOp::OrderBy(order) => {
            let mut rows = EvalRow::from_query_result(None, &qr);
            apply_order_by(&mut rows, order, ctx)?;
            Ok(eval_rows_to_result(&qr.columns, rows))
        }
        PipeOp::Limit { limit, offset } => {
            let mut rows = EvalRow::from_query_result(None, &qr);
            apply_limit_offset(&mut rows, Some(limit), offset.as_deref(), ctx)?;
            Ok(eval_rows_to_result(&qr.columns, rows))
        }
        PipeOp::As(_) => Ok(qr),
    }
}

fn pipe_where(
    qr: QueryResult,
    pred: &crate::ast::Expr,
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let eval_rows = EvalRow::from_query_result(None, &qr);
    let mut kept = Vec::new();
    for (eval_row, raw_row) in eval_rows.iter().zip(qr.rows) {
        if matches!(eval_expr(pred, eval_row, ctx)?, BqValue::Bool(true)) {
            kept.push(raw_row);
        }
    }
    Ok(QueryResult::new(qr.columns, kept))
}

fn pipe_select(
    qr: &QueryResult,
    items: &[SelectItem],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let eval_rows = EvalRow::from_query_result(None, qr);
    let dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection: items.to_vec(),
        from: Vec::new(),
        where_clause: None,
        group_by: None,
        having: None,
        qualify: None,
        window: Vec::new(),
    };
    let out_rows = project_eval_rows(&dummy_sel, &eval_rows, ctx)?;
    if out_rows.is_empty() {
        let schema_row = EvalRow::new(
            qr.columns
                .iter()
                .map(|c| RowBinding {
                    qualifier: None,
                    name: std::sync::Arc::from(c.name.as_str()),
                    ty: c.ty.clone(),
                    value: BqValue::Null,
                })
                .collect(),
        );
        let cols = super::schema::infer_select_columns(&dummy_sel, &schema_row, ctx);
        return Ok(QueryResult::new(cols, Vec::new()));
    }
    Ok(eval_rows_to_result_inferred(out_rows))
}

fn pipe_extend(
    qr: &QueryResult,
    items: &[SelectItem],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let mut eval_rows = EvalRow::from_query_result(None, qr);
    for row in &mut eval_rows {
        for item in items {
            let SelectItem::Expr { expr, alias } = item else {
                return Err(BqError::query("EXTEND expects expression items"));
            };
            let val = eval_expr(expr, row, ctx)?;
            let ty = val.infer_type()?;
            let name = alias
                .as_ref()
                .map(|a| a.value.clone())
                .or_else(|| infer_expr_col_name(expr))
                .ok_or_else(|| BqError::query("EXTEND expression must have a column alias"))?;
            row.cols.push(RowBinding {
                qualifier: None,
                name: std::sync::Arc::from(name),
                ty,
                value: val,
            });
        }
    }
    if eval_rows.is_empty() {
        let mut cols = qr.columns.clone();
        let schema_row = EvalRow::new(
            qr.columns
                .iter()
                .map(|c| RowBinding {
                    qualifier: None,
                    name: std::sync::Arc::from(c.name.as_str()),
                    ty: c.ty.clone(),
                    value: BqValue::Null,
                })
                .collect(),
        );
        for item in items {
            if let SelectItem::Expr { expr, alias } = item {
                let name = alias
                    .as_ref()
                    .map(|a| a.value.clone())
                    .or_else(|| infer_expr_col_name(expr))
                    .ok_or_else(|| BqError::query("EXTEND expression must have a column alias"))?;
                cols.push(QueryColumn::new(
                    name,
                    super::type_infer::infer_expr_type(expr, &schema_row, ctx),
                ));
            }
        }
        return Ok(QueryResult::new(cols, Vec::new()));
    }
    Ok(eval_rows_to_result_inferred(eval_rows))
}

fn pipe_set(
    qr: &QueryResult,
    assignments: &[(crate::ast::Ident, crate::ast::Expr)],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let mut eval_rows = EvalRow::from_query_result(None, qr);
    for row in &mut eval_rows {
        for (col, expr) in assignments {
            let val = eval_expr(expr, row, ctx)?;
            let ty = val.infer_type()?;
            let pos = row
                .cols
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&col.value))
                .ok_or_else(|| {
                    BqError::query(format!("Column '{}' not found in SET", col.value))
                })?;
            if let Some(slot) = row.cols.get_mut(pos) {
                slot.ty = ty;
                slot.value = val;
            }
        }
    }
    if eval_rows.is_empty() {
        return Ok(qr.clone());
    }
    Ok(eval_rows_to_result_inferred(eval_rows))
}

fn pipe_drop(mut qr: QueryResult, drops: &[crate::ast::Ident]) -> QueryResult {
    let keep: Vec<usize> = qr
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| !drops.iter().any(|d| d.value.eq_ignore_ascii_case(&c.name)))
        .map(|(i, _)| i)
        .collect();
    let cols = keep
        .iter()
        .filter_map(|&i| qr.columns.get(i).cloned())
        .collect();
    let rows = qr
        .rows
        .drain(..)
        .map(|r| keep.iter().filter_map(|&i| r.get(i).cloned()).collect())
        .collect();
    QueryResult::new(cols, rows)
}

fn pipe_rename(
    mut qr: QueryResult,
    pairs: &[(crate::ast::Ident, crate::ast::Ident)],
) -> BqResult<QueryResult> {
    for (old, new) in pairs {
        let pos = qr
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&old.value))
            .ok_or_else(|| BqError::query(format!("Column '{}' not found in RENAME", old.value)))?;
        if let Some(col) = qr.columns.get_mut(pos) {
            col.name.clone_from(&new.value);
        }
    }
    Ok(qr)
}

fn pipe_aggregate(
    qr: &QueryResult,
    aggs: &[SelectItem],
    group_by: &[crate::ast::GroupByItem],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let eval_rows = EvalRow::from_query_result(None, qr);
    let dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection: aggs.to_vec(),
        from: Vec::new(),
        where_clause: None,
        group_by: if group_by.is_empty() {
            None
        } else {
            Some(crate::ast::GroupByClause::Items(group_by.to_vec()))
        },
        having: None,
        qualify: None,
        window: Vec::new(),
    };
    let out_rows = execute_grouped_select(&dummy_sel, &eval_rows, &EvalRow::default(), ctx)?;
    Ok(eval_rows_to_result_inferred(out_rows))
}

fn eval_rows_to_result(cols: &[QueryColumn], rows: Vec<EvalRow>) -> QueryResult {
    let out_rows = rows
        .into_iter()
        .map(|r| r.cols.into_iter().map(|c| c.value).collect())
        .collect();
    QueryResult::new(cols.to_vec(), out_rows)
}

fn eval_rows_to_result_inferred(rows: Vec<EvalRow>) -> QueryResult {
    let cols = rows
        .first()
        .map(|r| {
            r.cols
                .iter()
                .map(|c| QueryColumn::new(c.name.as_ref().to_owned(), c.ty.clone()))
                .collect()
        })
        .unwrap_or_default();
    let out_rows = rows
        .into_iter()
        .map(|r| r.cols.into_iter().map(|c| c.value).collect())
        .collect();
    QueryResult::new(cols, out_rows)
}
