use std::collections::BTreeSet;

use crate::ast::{Cte, Expr, Query, SetExpr, SetOpKind, SetQuantifier, WithClause};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::result::QueryResult;
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

use super::cte_refs::{
    collect_query_outer_refs, collect_query_table_refs, query_references_table,
    set_expr_references_table,
};
use super::set_ops::{dedup_rows, rows_equal};
use super::{execute_query, execute_set_expr};

const MAX_CTE_RECURSION: usize = 100;

pub(super) fn eval_with_clause(
    with: &WithClause,
    enclosing_query: Option<&Query>,
    ctx: &mut ExecContext<'_>,
) -> BqResult<()> {
    let needed = enclosing_query
        .filter(|_| !with.recursive)
        .map(|q| compute_referenced_ctes(with, q));
    let order = if with.recursive {
        topo_order_ctes(&with.ctes)
    } else {
        (0..with.ctes.len()).collect()
    };
    for idx in order {
        let Some(cte) = with.ctes.get(idx) else {
            continue;
        };
        let key = cte.name.value.to_ascii_lowercase();
        if needed.as_ref().is_some_and(|set| !set.contains(&key)) {
            continue;
        }
        let is_self_rec = with.recursive && query_references_table(&cte.query, &key);
        let mut qr = if is_self_rec {
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
        ctx.ctes.insert(key, std::sync::Arc::new(qr));
    }
    Ok(())
}

fn compute_referenced_ctes(with: &WithClause, query: &Query) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    collect_query_outer_refs(query, &mut reachable);
    // Transitively pull in CTEs referenced by reachable CTEs (reverse order handles chaining).
    for cte in with.ctes.iter().rev() {
        let key = cte.name.value.to_ascii_lowercase();
        if reachable.contains(&key) {
            collect_query_table_refs(&cte.query, &mut reachable);
        }
    }
    reachable
}

fn topo_order_ctes(ctes: &[Cte]) -> Vec<usize> {
    let names: Vec<String> = ctes
        .iter()
        .map(|c| c.name.value.to_ascii_lowercase())
        .collect();
    let mut deps: Vec<BTreeSet<usize>> = Vec::with_capacity(ctes.len());
    for (i, cte) in ctes.iter().enumerate() {
        let mut refs = BTreeSet::new();
        collect_query_table_refs(&cte.query, &mut refs);
        let mut d = BTreeSet::new();
        for (j, other_name) in names.iter().enumerate() {
            if i != j && refs.contains(other_name) {
                d.insert(j);
            }
        }
        deps.push(d);
    }
    let mut visited = vec![false; ctes.len()];
    let mut out = Vec::with_capacity(ctes.len());
    for _ in 0..ctes.len() {
        let next = (0..ctes.len()).find(|&i| {
            !visited.get(i).copied().unwrap_or(true)
                && deps
                    .get(i)
                    .is_none_or(|ds| ds.iter().all(|&d| visited.get(d).copied().unwrap_or(false)))
        });
        let pick = next.unwrap_or_else(|| {
            (0..ctes.len())
                .find(|&i| !visited.get(i).copied().unwrap_or(true))
                .unwrap_or(0)
        });
        if let Some(slot) = visited.get_mut(pick) {
            *slot = true;
        }
        out.push(pick);
    }
    out
}

struct RecursiveParts<'a> {
    quantifier: SetQuantifier,
    by_name: bool,
    base_qr: QueryResult,
    rec_term: RecTerm<'a>,
}

struct DepthBounds {
    alias: Option<String>,
    min_depth: Option<i64>,
    max_depth: Option<i64>,
}

fn eval_recursive_cte(cte: &Cte, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let key = cte.name.value.to_ascii_lowercase();
    let empty_outer = EvalRow::default();
    let mut inner_ctx = ctx.with_outer(&empty_outer);
    if let Some(ref inner_with) = cte.query.with {
        eval_with_clause(inner_with, None, &mut inner_ctx)?;
    }
    let Some(parts) = split_recursive_query(&cte.query, &key, &inner_ctx)? else {
        return execute_query(&cte.query, ctx);
    };
    let bounds = resolve_depth_bounds(cte, &inner_ctx)?;
    let mut accumulated = parts.base_qr;
    if !cte.columns.is_empty() && cte.columns.len() == accumulated.columns.len() {
        for (col, alias) in accumulated.columns.iter_mut().zip(&cte.columns) {
            col.name.clone_from(&alias.value);
        }
    }
    if parts.quantifier == SetQuantifier::Distinct {
        accumulated.rows = dedup_rows(accumulated.rows)?;
    }
    let mut row_depths = vec![0_i64; accumulated.rows.len()];
    let mut delta = accumulated.clone();
    let iter_limit = bounds
        .max_depth
        .and_then(|m| usize::try_from(m).ok())
        .unwrap_or(MAX_CTE_RECURSION);
    for iter in 0..iter_limit {
        if iter > 0 && delta.rows.is_empty() {
            return Ok(apply_depth_filter(accumulated, &row_depths, &bounds));
        }
        let mut step_ctx = inner_ctx.with_outer(&empty_outer);
        step_ctx
            .ctes
            .insert(key.clone(), std::sync::Arc::new(delta));
        let raw_next = match parts.rec_term {
            RecTerm::SetExpr(se) => execute_set_expr(se, &step_ctx)?,
            RecTerm::Query(q) => execute_query(q, &step_ctx)?,
        };
        if raw_next.rows.is_empty() {
            return Ok(apply_depth_filter(accumulated, &row_depths, &bounds));
        }
        let mut next_delta =
            align_recursive_step(&cte.name.value, parts.by_name, &accumulated, raw_next)?;
        coerce_recursive_types(&mut accumulated, &mut next_delta)?;
        if parts.quantifier == SetQuantifier::Distinct {
            next_delta.rows = filter_new_rows(&accumulated.rows, next_delta.rows)?;
            if next_delta.rows.is_empty() {
                return Ok(apply_depth_filter(accumulated, &row_depths, &bounds));
            }
        }
        let next_depth = i64::try_from(iter + 1).unwrap_or(i64::MAX);
        row_depths.extend(std::iter::repeat_n(next_depth, next_delta.rows.len()));
        accumulated.rows.extend(next_delta.rows.clone());
        delta = next_delta;
    }
    if bounds.max_depth.is_some() {
        return Ok(apply_depth_filter(accumulated, &row_depths, &bounds));
    }
    Err(BqError::semantic(format!(
        "Recursive CTE '{}' exceeded maximum iteration limit ({MAX_CTE_RECURSION})",
        cte.name.value
    )))
}

enum RecTerm<'a> {
    SetExpr(&'a SetExpr),
    Query(&'a Query),
}

fn split_recursive_query<'a>(
    query: &'a Query,
    cte_key: &str,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<RecursiveParts<'a>>> {
    if let Some((quantifier, by_name, base_expr, rec_expr)) =
        split_recursive_union(&query.body, cte_key)
    {
        let base_qr = execute_set_expr(base_expr, ctx)?;
        return Ok(Some(RecursiveParts {
            quantifier,
            by_name,
            base_qr,
            rec_term: RecTerm::SetExpr(rec_expr),
        }));
    }
    if let Some(crate::ast::PipeOp::SetOp {
        op: SetOpKind::Union,
        quantifier,
        correspondence,
        queries,
    }) = query.pipe_ops.last()
    {
        if let [rec_q] = queries.as_slice() {
            if query_references_table(rec_q, cte_key) {
                let base_q = Query {
                    with: None,
                    body: query.body.clone(),
                    order_by: query.order_by.clone(),
                    limit: query.limit.clone(),
                    offset: query.offset.clone(),
                    pipe_ops: query
                        .pipe_ops
                        .get(..query.pipe_ops.len().saturating_sub(1))
                        .unwrap_or(&[])
                        .to_vec(),
                };
                let base_qr = execute_query(&base_q, ctx)?;
                let by_name = !matches!(correspondence, crate::ast::SetCorrespondence::Positional);
                return Ok(Some(RecursiveParts {
                    quantifier: *quantifier,
                    by_name,
                    base_qr,
                    rec_term: RecTerm::Query(rec_q),
                }));
            }
        }
    }
    Ok(None)
}

fn resolve_depth_bounds(cte: &Cte, ctx: &ExecContext<'_>) -> BqResult<DepthBounds> {
    let Some(ref ds) = cte.depth_spec else {
        return Ok(DepthBounds {
            alias: None,
            min_depth: None,
            max_depth: None,
        });
    };
    let empty = EvalRow::default();
    let eval_i64 = |e: &Expr| -> BqResult<i64> {
        let BqValue::Int64(n) = crate::executor::evaluator::eval_expr(e, &empty, ctx)? else {
            return Err(BqError::query("WITH DEPTH bounds must be INT64"));
        };
        Ok(n)
    };
    let min_depth = ds.min_depth.as_deref().map(eval_i64).transpose()?;
    let max_depth = ds.max_depth.as_deref().map(eval_i64).transpose()?;
    let alias = ds.alias.as_ref().map(|a| a.value.clone());
    Ok(DepthBounds {
        alias,
        min_depth,
        max_depth,
    })
}

fn apply_depth_filter(mut qr: QueryResult, depths: &[i64], bounds: &DepthBounds) -> QueryResult {
    if bounds.alias.is_none() && bounds.min_depth.is_none() && bounds.max_depth.is_none() {
        return qr;
    }
    if let Some(ref col_name) = bounds.alias {
        qr.columns.push(crate::executor::result::QueryColumn::new(
            col_name.clone(),
            crate::types::BqType::Int64,
        ));
    }
    let mut filtered = Vec::with_capacity(qr.rows.len());
    for (mut row, &d) in qr.rows.into_iter().zip(depths.iter()) {
        if bounds.min_depth.is_some_and(|lo| d < lo) || bounds.max_depth.is_some_and(|hi| d > hi) {
            continue;
        }
        if bounds.alias.is_some() {
            row.push(BqValue::Int64(d));
        }
        filtered.push(row);
    }
    qr.rows = filtered;
    qr
}

fn split_recursive_union<'a>(
    expr: &'a SetExpr,
    cte_key: &str,
) -> Option<(SetQuantifier, bool, &'a SetExpr, &'a SetExpr)> {
    match expr {
        SetExpr::SetOperation {
            op: SetOpKind::Union,
            quantifier,
            correspondence,
            left,
            right,
        } if set_expr_references_table(right, cte_key) => Some((
            *quantifier,
            !matches!(correspondence, crate::ast::SetCorrespondence::Positional),
            left.as_ref(),
            right.as_ref(),
        )),
        SetExpr::Query(q) => split_recursive_union(&q.body, cte_key),
        SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::SetOperation { .. } => None,
    }
}

fn align_recursive_step(
    cte_name: &str,
    by_name: bool,
    accumulated: &QueryResult,
    mut next: QueryResult,
) -> BqResult<QueryResult> {
    if next.columns.len() != accumulated.columns.len() {
        return Err(BqError::query(format!(
            "Recursive CTE '{cte_name}' column count mismatch"
        )));
    }
    if by_name {
        let mut perm = Vec::with_capacity(accumulated.columns.len());
        for acol in &accumulated.columns {
            let pos = next
                .columns
                .iter()
                .position(|ncol| ncol.name.eq_ignore_ascii_case(&acol.name))
                .ok_or_else(|| {
                    BqError::query(format!(
                        "Column '{}' not found in recursive term of CTE '{cte_name}'",
                        acol.name
                    ))
                })?;
            perm.push(pos);
        }
        let reordered_cols = perm
            .iter()
            .filter_map(|&i| next.columns.get(i).cloned())
            .collect();
        let reordered_rows = next
            .rows
            .into_iter()
            .map(|r| {
                perm.iter()
                    .map(|&i| r.get(i).cloned().unwrap_or(BqValue::Null))
                    .collect()
            })
            .collect();
        return Ok(QueryResult::new(reordered_cols, reordered_rows));
    }
    for (dst, src) in next.columns.iter_mut().zip(&accumulated.columns) {
        dst.name.clone_from(&src.name);
    }
    Ok(next)
}

fn coerce_recursive_types(acc: &mut QueryResult, next: &mut QueryResult) -> BqResult<()> {
    for (col_idx, (acol, ncol)) in acc
        .columns
        .iter_mut()
        .zip(next.columns.iter_mut())
        .enumerate()
    {
        if acol.ty == ncol.ty {
            continue;
        }
        let super_ty = common_supertype(&[acol.ty.clone(), ncol.ty.clone()])?;
        if acol.ty != super_ty {
            for row in &mut acc.rows {
                if let Some(cell) = row.get_mut(col_idx) {
                    let v = std::mem::replace(cell, BqValue::Null);
                    *cell = coerce_value(v, &super_ty)?;
                }
            }
            acol.ty = super_ty.clone();
        }
        if ncol.ty != super_ty {
            for row in &mut next.rows {
                if let Some(cell) = row.get_mut(col_idx) {
                    let v = std::mem::replace(cell, BqValue::Null);
                    *cell = coerce_value(v, &super_ty)?;
                }
            }
            ncol.ty = super_ty;
        }
    }
    Ok(())
}

fn filter_new_rows(
    existing: &[Vec<BqValue>],
    candidates: Vec<Vec<BqValue>>,
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut index: std::collections::HashMap<u64, Vec<(bool, usize)>> =
        std::collections::HashMap::with_capacity(existing.len() + candidates.len());
    for (i, ex) in existing.iter().enumerate() {
        index
            .entry(super::exec_util::hash_value_slice(ex))
            .or_default()
            .push((true, i));
    }
    let mut fresh: Vec<Vec<BqValue>> = Vec::new();
    for row in candidates {
        let h = super::exec_util::hash_value_slice(&row);
        let cands = index.entry(h).or_default();
        let mut dup = false;
        for &(is_ex, idx) in cands.iter() {
            let target = if is_ex {
                existing.get(idx)
            } else {
                fresh.get(idx)
            };
            if let Some(ex) = target {
                if rows_equal(ex, &row)? {
                    dup = true;
                    break;
                }
            }
        }
        if !dup {
            let new_idx = fresh.len();
            fresh.push(row);
            cands.push((false, new_idx));
        }
    }
    Ok(fresh)
}
