use std::sync::Arc;

use crate::ast::{Expr, Ident, NamedWindowDef, PipeOp, Query, Select, SelectItem, SetExpr};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding};
use crate::executor::evaluator::{eval_expr, infer_expr_col_name};
use crate::executor::result::{QueryColumn, QueryResult};
use crate::types::BqValue;

use super::from::eval_from_clause_with_schema;
use super::pipe_ops::{
    pipe_aggregate, pipe_as, pipe_distinct, pipe_join, pipe_limit, pipe_order_by, pipe_pivot,
    pipe_set_op, pipe_tablesample, pipe_unpivot, pipe_with,
};
use super::project::{has_untyped_null, project_eval_rows};
use super::schema::{infer_select_columns, null_out_eval_row};
use super::sort::{apply_limit_offset, apply_order_by};
use super::type_infer::infer_expr_type;
use super::window::{apply_windows, apply_windows_to_exprs, expr_has_window, select_has_windows};
use super::{execute_select_full, execute_set_expr};

pub(super) struct PipeState {
    pub rows: Vec<EvalRow>,
    pub schema: EvalRow,
}

impl PipeState {
    pub(super) fn from_result(qr: &QueryResult) -> Self {
        let rows = EvalRow::from_query_result(None, qr);
        let schema = rows.first().map_or_else(
            || {
                EvalRow::new(
                    qr.columns
                        .iter()
                        .map(|c| RowBinding {
                            qualifier: None,
                            name: Arc::from(c.name.as_str()),
                            ty: c.ty.clone(),
                            value: BqValue::Null,
                        })
                        .collect(),
                )
            },
            null_out_eval_row,
        );
        Self { rows, schema }
    }

    pub(super) fn into_result(self) -> QueryResult {
        if self.rows.is_empty() {
            let cols = self
                .schema
                .cols
                .into_iter()
                .map(|c| QueryColumn::new(c.name.as_ref().to_owned(), c.ty))
                .collect();
            return QueryResult::new(cols, Vec::new());
        }
        let mut cols: Vec<QueryColumn> = self
            .rows
            .first()
            .map(|r| {
                r.cols
                    .iter()
                    .map(|c| QueryColumn::new(c.name.as_ref().to_owned(), c.ty.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (idx, col) in cols.iter_mut().enumerate() {
            if has_untyped_null(&col.ty) {
                if let Some(found) = self
                    .rows
                    .iter()
                    .filter_map(|r| r.cols.get(idx))
                    .find(|c| !has_untyped_null(&c.ty))
                {
                    col.ty = found.ty.clone();
                } else if let Some(sc) = self.schema.cols.get(idx) {
                    if !has_untyped_null(&sc.ty) {
                        col.ty = sc.ty.clone();
                    }
                }
            }
        }
        let out_rows = self
            .rows
            .into_iter()
            .map(|r| r.cols.into_iter().map(|c| c.value).collect())
            .collect();
        QueryResult::new(cols, out_rows)
    }
}

pub(super) fn execute_query_with_pipes(
    query: &Query,
    ctx: &mut ExecContext<'_>,
) -> BqResult<QueryResult> {
    let mut state = init_pipe_state(query, ctx)?;
    for op in &query.pipe_ops {
        state = apply_single_pipe_op(state, op, ctx)?;
    }
    Ok(state.into_result())
}

fn init_pipe_state(query: &Query, ctx: &ExecContext<'_>) -> BqResult<PipeState> {
    if let SetExpr::Select(ref sel) = query.body {
        if is_bare_from_select(sel, query) {
            let (rows, schema) = eval_from_clause_with_schema(&sel.from, ctx)?;
            return Ok(PipeState { rows, schema });
        }
        let qr = execute_select_full(
            sel,
            &query.order_by,
            query.limit.as_ref(),
            query.offset.as_ref(),
            ctx,
        )?;
        return Ok(PipeState::from_result(&qr));
    }
    let mut qr = execute_set_expr(&query.body, ctx)?;
    if !query.order_by.is_empty() || query.limit.is_some() || query.offset.is_some() {
        let mut eval_rows = EvalRow::from_query_result(None, &qr);
        if !query.order_by.is_empty() {
            apply_order_by(&mut eval_rows, &query.order_by, ctx)?;
        }
        apply_limit_offset(
            &mut eval_rows,
            query.limit.as_ref(),
            query.offset.as_ref(),
            ctx,
        )?;
        qr.rows = eval_rows
            .into_iter()
            .map(|r| r.cols.into_iter().map(|c| c.value).collect())
            .collect();
    }
    Ok(PipeState::from_result(&qr))
}

fn is_bare_from_select(sel: &Select, query: &Query) -> bool {
    !sel.distinct
        && sel.as_kind.is_none()
        && !sel.from.is_empty()
        && sel.where_clause.is_none()
        && sel.group_by.is_none()
        && sel.having.is_none()
        && sel.qualify.is_none()
        && sel.window.is_empty()
        && query.order_by.is_empty()
        && query.limit.is_none()
        && query.offset.is_none()
        && matches!(
            sel.projection.as_slice(),
            [SelectItem::Wildcard {
                qualifier: None,
                except,
                replace,
            }] if except.is_empty() && replace.is_empty()
        )
}

fn apply_single_pipe_op(
    state: PipeState,
    op: &PipeOp,
    ctx: &mut ExecContext<'_>,
) -> BqResult<PipeState> {
    match op {
        PipeOp::Where(pred) => pipe_where(state, pred, ctx),
        PipeOp::Select { items, window } => pipe_select(state, items, window, ctx),
        PipeOp::Extend { items, window } | PipeOp::Window { items, window } => {
            pipe_extend(state, items, window, ctx)
        }
        PipeOp::Set(assignments) => pipe_set(state, assignments, ctx),
        PipeOp::Drop(cols) => Ok(pipe_drop(state, cols)),
        PipeOp::Rename(pairs) => pipe_rename(state, pairs),
        PipeOp::Aggregate {
            aggs,
            group_by,
            group_and_order_by,
        } => pipe_aggregate(&state, aggs, group_by, *group_and_order_by, ctx),
        PipeOp::OrderBy(order) => pipe_order_by(state, order, ctx),
        PipeOp::Limit { limit, offset } => {
            pipe_limit(state, limit.as_deref(), offset.as_deref(), ctx)
        }
        PipeOp::As(alias) => Ok(pipe_as(state, alias)),
        PipeOp::Distinct => pipe_distinct(state),
        PipeOp::Join(join) => pipe_join(&state, join, ctx),
        PipeOp::SetOp {
            op: set_op,
            quantifier,
            correspondence,
            queries,
        } => pipe_set_op(state, *set_op, *quantifier, *correspondence, queries, ctx),
        PipeOp::Pivot { spec, alias } => pipe_pivot(state, spec, alias.as_ref(), ctx),
        PipeOp::Unpivot { spec, alias } => pipe_unpivot(state, spec, alias.as_ref()),
        PipeOp::TableSample { amount, is_rows } => pipe_tablesample(state, amount, *is_rows, ctx),
        PipeOp::With(ctes) => pipe_with(state, ctes, ctx),
    }
}

pub(super) fn strip_synthetic_win_cols(rows: &mut [EvalRow]) {
    for row in rows {
        row.cols.retain(|c| !c.name.starts_with("__win_"));
    }
}

fn pipe_where(mut state: PipeState, pred: &Expr, ctx: &ExecContext<'_>) -> BqResult<PipeState> {
    let mut pred_expr = pred.clone();
    let has_win = expr_has_window(&pred_expr);
    if has_win {
        apply_windows_to_exprs(
            std::slice::from_mut(&mut pred_expr),
            &[],
            &mut state.rows,
            ctx,
        )?;
    }
    let mut kept = Vec::new();
    for row in state.rows {
        if matches!(eval_expr(&pred_expr, &row, ctx)?, BqValue::Bool(true)) {
            kept.push(row);
        }
    }
    if has_win {
        strip_synthetic_win_cols(&mut kept);
    }
    Ok(PipeState {
        rows: kept,
        schema: state.schema,
    })
}

fn pipe_select(
    state: PipeState,
    items: &[SelectItem],
    window: &[NamedWindowDef],
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let mut dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection: items.to_vec(),
        from: Vec::new(),
        where_clause: None,
        group_by: None,
        having: None,
        qualify: None,
        window: window.to_vec(),
    };
    let rows_for_proj = if select_has_windows(&dummy_sel) {
        let (rewritten, augmented) = apply_windows(&dummy_sel, state.rows, ctx)?;
        dummy_sel = rewritten;
        augmented
    } else {
        state.rows
    };
    let out_rows = project_eval_rows(&dummy_sel, &rows_for_proj, ctx)?;
    let out_schema = out_rows.first().map_or_else(
        || {
            let cols = infer_select_columns(&dummy_sel, &state.schema, ctx);
            EvalRow::new(
                cols.into_iter()
                    .map(|c| RowBinding {
                        qualifier: None,
                        name: Arc::from(c.name),
                        ty: c.ty,
                        value: BqValue::Null,
                    })
                    .collect(),
            )
        },
        null_out_eval_row,
    );
    Ok(PipeState {
        rows: out_rows,
        schema: out_schema,
    })
}

fn pipe_extend(
    mut state: PipeState,
    items: &[SelectItem],
    window: &[NamedWindowDef],
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let mut exprs: Vec<Expr> = Vec::with_capacity(items.len());
    let mut names: Vec<Arc<str>> = Vec::with_capacity(items.len());
    let mut anon_idx = 0_usize;
    for item in items {
        let SelectItem::Expr { expr, alias } = item else {
            return Err(BqError::query("EXTEND/WINDOW expects expression items"));
        };
        let name = alias
            .as_ref()
            .map(|a| a.value.clone())
            .or_else(|| infer_expr_col_name(expr))
            .unwrap_or_else(|| {
                let n = format!("f{anon_idx}_");
                anon_idx += 1;
                n
            });
        exprs.push(expr.as_ref().clone());
        names.push(Arc::from(name));
    }
    let has_win = exprs.iter().any(expr_has_window);
    if has_win {
        apply_windows_to_exprs(&mut exprs, window, &mut state.rows, ctx)?;
    }
    for row in &mut state.rows {
        let mut additions = Vec::with_capacity(exprs.len());
        for (expr, name) in exprs.iter().zip(names.iter()) {
            let val = eval_expr(expr, row, ctx)?;
            let ty = val.infer_type()?;
            additions.push(RowBinding {
                qualifier: None,
                name: name.clone(),
                ty,
                value: val,
            });
        }
        row.cols.extend(additions);
    }
    if has_win {
        strip_synthetic_win_cols(&mut state.rows);
    }
    for (expr, name) in exprs.iter().zip(names.iter()) {
        let ty = infer_expr_type(expr, &state.schema, ctx);
        state.schema.cols.push(RowBinding {
            qualifier: None,
            name: name.clone(),
            ty,
            value: BqValue::Null,
        });
    }
    Ok(state)
}

fn pipe_set(
    mut state: PipeState,
    assignments: &[(Ident, Expr)],
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let mut exprs: Vec<Expr> = assignments.iter().map(|(_, e)| e.clone()).collect();
    let has_win = exprs.iter().any(expr_has_window);
    if has_win {
        apply_windows_to_exprs(&mut exprs, &[], &mut state.rows, ctx)?;
    }
    for row in &mut state.rows {
        for ((col, _), expr) in assignments.iter().zip(exprs.iter()) {
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
    if has_win {
        strip_synthetic_win_cols(&mut state.rows);
    }
    if let Some(first) = state.rows.first() {
        state.schema = null_out_eval_row(first);
    }
    Ok(state)
}

fn pipe_drop(mut state: PipeState, drops: &[Ident]) -> PipeState {
    let keep = |c: &RowBinding| !drops.iter().any(|d| d.value.eq_ignore_ascii_case(&c.name));
    for row in &mut state.rows {
        row.cols.retain(&keep);
    }
    state.schema.cols.retain(keep);
    state
}

fn pipe_rename(mut state: PipeState, pairs: &[(Ident, Ident)]) -> BqResult<PipeState> {
    let mut targets: Vec<(usize, Arc<str>)> = Vec::with_capacity(pairs.len());
    for (old, new) in pairs {
        let pos = state
            .schema
            .cols
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&old.value))
            .ok_or_else(|| BqError::query(format!("Column '{}' not found in RENAME", old.value)))?;
        targets.push((pos, Arc::from(new.value.as_str())));
    }
    for (pos, new_name) in &targets {
        if let Some(col) = state.schema.cols.get_mut(*pos) {
            col.name = new_name.clone();
        }
    }
    for row in &mut state.rows {
        for (pos, new_name) in &targets {
            if let Some(col) = row.cols.get_mut(*pos) {
                col.name = new_name.clone();
            }
        }
    }
    Ok(state)
}
