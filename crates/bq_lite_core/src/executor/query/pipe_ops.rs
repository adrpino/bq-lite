use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{
    Cte, Expr, GroupByClause, GroupByItem, Ident, Join, Literal, OrderByExpr, PipeGroupByItem,
    PipeOrderedItem, PivotSpec, Query, Select, SelectItem, SetCorrespondence, SetOpKind,
    SetQuantifier, UnpivotSpec,
};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::evaluator::{eval_expr, values_distinct_eq};
use crate::types::BqValue;

use super::agg::execute_grouped_select_with_order;
use super::exec_util::hash_value_slice;
use super::execute_query;
use super::governance::apply_tablesample;
use super::joins::eval_join_with_schema;
use super::pipe::{strip_synthetic_win_cols, PipeState};
use super::pivot::{apply_pivot, apply_unpivot};
use super::schema::null_out_eval_row;
use super::set_ops::execute_set_op;
use super::sort::apply_order_by;
use super::window::{apply_windows_to_exprs, expr_has_window};

pub(super) fn pipe_aggregate(
    state: &PipeState,
    aggs: &[PipeOrderedItem],
    group_by: &[PipeGroupByItem],
    group_and_order_by: bool,
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let mut projection = Vec::with_capacity(group_by.len() + aggs.len());
    let mut gb_items = Vec::with_capacity(group_by.len());
    let mut order_by = Vec::new();

    for (idx, gb) in group_by.iter().enumerate() {
        gb_items.push(gb.item.clone());
        let GroupByItem::Expr(ref gb_expr) = gb.item else {
            return Err(BqError::query(
                "Pipe AGGREGATE GROUP BY currently supports expression items",
            ));
        };
        projection.push(SelectItem::Expr {
            expr: gb_expr.clone(),
            alias: gb.alias.clone(),
        });
        if group_and_order_by || gb.order.is_some() {
            let dir = gb.order;
            let ord = i64::try_from(idx + 1).unwrap_or(i64::MAX);
            order_by.push(OrderByExpr {
                expr: Expr::Literal(Literal::Int(ord)),
                asc: Some(dir.is_none_or(|d| d.asc)),
                nulls_first: dir.and_then(|d| d.nulls_first),
            });
        }
    }

    let gb_len = group_by.len();
    for (idx, agg) in aggs.iter().enumerate() {
        projection.push(agg.item.clone());
        if let Some(dir) = agg.order {
            let ord = i64::try_from(gb_len + idx + 1).unwrap_or(i64::MAX);
            order_by.push(OrderByExpr {
                expr: Expr::Literal(Literal::Int(ord)),
                asc: Some(dir.asc),
                nulls_first: dir.nulls_first,
            });
        }
    }

    let dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection,
        from: Vec::new(),
        where_clause: None,
        group_by: if gb_items.is_empty() {
            None
        } else {
            Some(GroupByClause::Items(gb_items))
        },
        having: None,
        qualify: None,
        window: Vec::new(),
    };
    let (mut out_rows, _, _) =
        execute_grouped_select_with_order(&dummy_sel, &order_by, &state.rows, &state.schema, ctx)?;
    if !order_by.is_empty() {
        apply_order_by(&mut out_rows, &order_by, ctx)?;
    }
    let out_schema = out_rows.first().map(null_out_eval_row).unwrap_or_default();
    Ok(PipeState {
        rows: out_rows,
        schema: out_schema,
    })
}

pub(super) fn pipe_order_by(
    mut state: PipeState,
    order: &[OrderByExpr],
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let has_win = order.iter().any(|ob| expr_has_window(&ob.expr));
    if has_win {
        let mut exprs: Vec<Expr> = order.iter().map(|ob| ob.expr.clone()).collect();
        apply_windows_to_exprs(&mut exprs, &[], &mut state.rows, ctx)?;
        let rewritten: Vec<OrderByExpr> = order
            .iter()
            .zip(exprs)
            .map(|(ob, expr)| OrderByExpr {
                expr,
                asc: ob.asc,
                nulls_first: ob.nulls_first,
            })
            .collect();
        apply_order_by(&mut state.rows, &rewritten, ctx)?;
        strip_synthetic_win_cols(&mut state.rows);
    } else {
        apply_order_by(&mut state.rows, order, ctx)?;
    }
    Ok(state)
}

pub(super) fn pipe_limit(
    mut state: PipeState,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let empty = EvalRow::default();
    if let Some(off_expr) = offset {
        let BqValue::Int64(n) = eval_expr(off_expr, &empty, ctx)? else {
            return Err(BqError::query("OFFSET must evaluate to INT64"));
        };
        if n < 0 {
            return Err(BqError::query("OFFSET must be non-negative"));
        }
        let skip = usize::try_from(n)
            .unwrap_or(usize::MAX)
            .min(state.rows.len());
        state.rows.drain(..skip);
    }
    if let Some(lim_expr) = limit {
        match eval_expr(lim_expr, &empty, ctx)? {
            BqValue::Null => {}
            BqValue::Int64(n) => {
                if n < 0 {
                    return Err(BqError::query("LIMIT must be non-negative"));
                }
                let take = usize::try_from(n).unwrap_or(usize::MAX);
                state.rows.truncate(take);
            }
            BqValue::Bool(_)
            | BqValue::Float64(_)
            | BqValue::Numeric(_)
            | BqValue::BigNumeric(_)
            | BqValue::String(_)
            | BqValue::Bytes(_)
            | BqValue::Date(_)
            | BqValue::Datetime(_)
            | BqValue::Time(_)
            | BqValue::Timestamp(_)
            | BqValue::Interval(_)
            | BqValue::Json(_)
            | BqValue::Geography(_)
            | BqValue::Range(_)
            | BqValue::Array(_)
            | BqValue::Struct(_) => return Err(BqError::query("LIMIT must evaluate to INT64")),
        }
    }
    Ok(state)
}

pub(super) fn pipe_as(mut state: PipeState, alias: &Ident) -> PipeState {
    let qual: Option<Arc<str>> = Some(Arc::from(alias.value.as_str()));
    for row in &mut state.rows {
        for col in &mut row.cols {
            col.qualifier.clone_from(&qual);
        }
        row.pseudo_cols.clear();
    }
    for col in &mut state.schema.cols {
        col.qualifier.clone_from(&qual);
    }
    state.schema.pseudo_cols.clear();
    state
}

pub(super) fn pipe_distinct(state: PipeState) -> BqResult<PipeState> {
    let mut out: Vec<EvalRow> = Vec::with_capacity(state.rows.len());
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut vals_buf: Vec<BqValue> = Vec::new();
    for row in state.rows {
        vals_buf.clear();
        vals_buf.extend(row.cols.iter().map(|c| c.value.clone()));
        let h = hash_value_slice(&vals_buf);
        let entry = buckets.entry(h).or_default();
        let mut dup = false;
        for &existing_idx in entry.iter() {
            if let Some(existing) = out.get(existing_idx) {
                if existing.cols.len() == row.cols.len() {
                    let mut all_eq = true;
                    for (a, b) in existing.cols.iter().zip(row.cols.iter()) {
                        if !values_distinct_eq(&a.value, &b.value)? {
                            all_eq = false;
                            break;
                        }
                    }
                    if all_eq {
                        dup = true;
                        break;
                    }
                }
            }
        }
        if !dup {
            entry.push(out.len());
            out.push(row);
        }
    }
    Ok(PipeState {
        rows: out,
        schema: state.schema,
    })
}

pub(super) fn pipe_join(
    state: &PipeState,
    join: &Join,
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let (rows, schema) = eval_join_with_schema(&state.rows, &state.schema, join, ctx)?;
    Ok(PipeState { rows, schema })
}

pub(super) fn pipe_set_op(
    state: PipeState,
    op: SetOpKind,
    quantifier: SetQuantifier,
    correspondence: SetCorrespondence,
    queries: &[Query],
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let mut acc = state.into_result();
    for q in queries {
        let right = execute_query(q, ctx)?;
        acc = execute_set_op(op, quantifier, correspondence, acc, right)?;
    }
    Ok(PipeState::from_result(&acc))
}

pub(super) fn pipe_pivot(
    state: PipeState,
    spec: &PivotSpec,
    alias: Option<&Ident>,
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    let rows = apply_pivot(state.rows, spec, alias.map(|a| a.value.as_str()), ctx)?;
    let schema = rows.first().map(null_out_eval_row).unwrap_or_default();
    Ok(PipeState { rows, schema })
}

pub(super) fn pipe_unpivot(
    state: PipeState,
    spec: &UnpivotSpec,
    alias: Option<&Ident>,
) -> BqResult<PipeState> {
    let rows = apply_unpivot(state.rows, spec, alias.map(|a| a.value.as_str()))?;
    let schema = rows.first().map(null_out_eval_row).unwrap_or_default();
    Ok(PipeState { rows, schema })
}

pub(super) fn pipe_tablesample(
    mut state: PipeState,
    amount: &Expr,
    is_rows: bool,
    ctx: &ExecContext<'_>,
) -> BqResult<PipeState> {
    if is_rows {
        let BqValue::Int64(n) = eval_expr(amount, &EvalRow::default(), ctx)? else {
            return Err(BqError::query("TABLESAMPLE ROWS expects INT64"));
        };
        if n < 0 {
            return Err(BqError::semantic("TABLESAMPLE ROWS must be non-negative"));
        }
        let keep = usize::try_from(n)
            .unwrap_or(usize::MAX)
            .min(state.rows.len());
        state.rows.truncate(keep);
        return Ok(state);
    }
    let rows = apply_tablesample(state.rows, amount, ctx)?;
    Ok(PipeState {
        rows,
        schema: state.schema,
    })
}

pub(super) fn pipe_with(
    state: PipeState,
    ctes: &[Cte],
    ctx: &mut ExecContext<'_>,
) -> BqResult<PipeState> {
    for cte in ctes {
        let mut qr = execute_query(&cte.query, ctx)?;
        if !cte.columns.is_empty() {
            if cte.columns.len() != qr.columns.len() {
                return Err(BqError::query(format!(
                    "CTE '{}' column alias count mismatch",
                    cte.name.value
                )));
            }
            for (col, alias) in qr.columns.iter_mut().zip(&cte.columns) {
                col.name.clone_from(&alias.value);
            }
        }
        ctx.ctes
            .insert(cte.name.value.to_ascii_lowercase(), Arc::new(qr));
    }
    Ok(state)
}
