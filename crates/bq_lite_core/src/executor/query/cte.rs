use std::collections::BTreeSet;

use crate::ast::{
    Cte, Expr, Query, Select, SelectItem, SetExpr, SetOpKind, SetQuantifier, TableFactor,
    TableWithJoins, WithClause,
};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext};
use crate::executor::result::QueryResult;
use crate::types::coercion::{coerce_value, common_supertype};
use crate::types::BqValue;

use super::set_ops::{dedup_rows, rows_equal};
use super::{execute_query, execute_set_expr};

const MAX_CTE_RECURSION: usize = 100;

pub(super) fn eval_with_clause(with: &WithClause, ctx: &mut ExecContext<'_>) -> BqResult<()> {
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
        ctx.ctes.insert(key, qr);
    }
    Ok(())
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

fn eval_recursive_cte(cte: &Cte, ctx: &ExecContext<'_>) -> BqResult<QueryResult> {
    let key = cte.name.value.to_ascii_lowercase();
    let Some((quantifier, by_name, base_expr, rec_expr)) =
        split_recursive_union(&cte.query.body, &key)
    else {
        return execute_query(&cte.query, ctx);
    };
    let mut accumulated = execute_set_expr(base_expr, ctx)?;
    if !cte.columns.is_empty() && cte.columns.len() == accumulated.columns.len() {
        for (col, alias) in accumulated.columns.iter_mut().zip(&cte.columns) {
            col.name.clone_from(&alias.value);
        }
    }
    if quantifier == SetQuantifier::Distinct {
        accumulated.rows = dedup_rows(accumulated.rows)?;
    }
    let mut delta = accumulated.clone();
    let empty_outer = EvalRow::default();
    for iter in 0..MAX_CTE_RECURSION {
        if iter > 0 && delta.rows.is_empty() {
            return Ok(accumulated);
        }
        let mut step_ctx = ctx.with_outer(&empty_outer);
        step_ctx.ctes.insert(key.clone(), delta);
        let raw_next = execute_set_expr(rec_expr, &step_ctx)?;
        if raw_next.rows.is_empty() {
            return Ok(accumulated);
        }
        let mut next_delta =
            align_recursive_step(&cte.name.value, by_name, &accumulated, raw_next)?;
        coerce_recursive_types(&mut accumulated, &mut next_delta)?;
        if quantifier == SetQuantifier::Distinct {
            next_delta.rows = filter_new_rows(&accumulated.rows, next_delta.rows)?;
            if next_delta.rows.is_empty() {
                return Ok(accumulated);
            }
        }
        accumulated.rows.extend(next_delta.rows.clone());
        delta = next_delta;
    }
    Err(BqError::semantic(format!(
        "Recursive CTE '{}' exceeded maximum iteration limit ({MAX_CTE_RECURSION})",
        cte.name.value
    )))
}

fn split_recursive_union<'a>(
    expr: &'a SetExpr,
    cte_key: &str,
) -> Option<(SetQuantifier, bool, &'a SetExpr, &'a SetExpr)> {
    match expr {
        SetExpr::SetOperation {
            op: SetOpKind::Union,
            quantifier,
            by_name,
            left,
            right,
        } if set_expr_references_table(right, cte_key) => {
            Some((*quantifier, *by_name, left.as_ref(), right.as_ref()))
        }
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

fn query_references_table(query: &Query, target_lower: &str) -> bool {
    let mut refs = BTreeSet::new();
    collect_query_table_refs(query, &mut refs);
    refs.contains(target_lower)
}

fn set_expr_references_table(expr: &SetExpr, target_lower: &str) -> bool {
    let mut refs = BTreeSet::new();
    collect_set_expr_table_refs(expr, &mut refs);
    refs.contains(target_lower)
}

fn collect_query_table_refs(query: &Query, out: &mut BTreeSet<String>) {
    if let Some(ref w) = query.with {
        for c in &w.ctes {
            collect_query_table_refs(&c.query, out);
        }
    }
    collect_set_expr_table_refs(&query.body, out);
}

fn collect_set_expr_table_refs(expr: &SetExpr, out: &mut BTreeSet<String>) {
    match expr {
        SetExpr::Select(sel) => collect_select_table_refs(sel, out),
        SetExpr::Query(q) => collect_query_table_refs(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_set_expr_table_refs(left, out);
            collect_set_expr_table_refs(right, out);
        }
        SetExpr::Values(_) => {}
    }
}

fn collect_select_table_refs(sel: &Select, out: &mut BTreeSet<String>) {
    for twj in &sel.from {
        collect_twj_table_refs(twj, out);
    }
    for item in &sel.projection {
        if let SelectItem::Expr { expr, .. } = item {
            collect_expr_table_refs(expr, out);
        }
    }
    if let Some(ref w) = sel.where_clause {
        collect_expr_table_refs(w, out);
    }
    if let Some(ref h) = sel.having {
        collect_expr_table_refs(h, out);
    }
}

fn collect_twj_table_refs(twj: &TableWithJoins, out: &mut BTreeSet<String>) {
    collect_factor_table_refs(&twj.relation, out);
    for j in &twj.joins {
        collect_factor_table_refs(&j.relation, out);
    }
}

fn collect_factor_table_refs(factor: &TableFactor, out: &mut BTreeSet<String>) {
    match factor {
        TableFactor::Table { name, .. } => {
            let parts = name.normalized_parts();
            if let [single] = parts.as_slice() {
                out.insert(single.to_ascii_lowercase());
            }
        }
        TableFactor::Derived { query, .. } => collect_query_table_refs(query, out),
        TableFactor::NestedJoin(twj) => collect_twj_table_refs(twj, out),
        TableFactor::Pivot { source, .. } | TableFactor::Unpivot { source, .. } => {
            collect_factor_table_refs(source, out);
        }
        TableFactor::Unnest { expr, .. } => collect_expr_table_refs(expr, out),
        TableFactor::TableFunction { args, .. } => {
            for a in args {
                collect_expr_table_refs(&a.expr, out);
            }
        }
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only subquery-bearing expression variants can contain CTE table references"
)]
fn collect_expr_table_refs(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Subquery(q) | Expr::ArraySubquery(q) | Expr::Exists(q) => {
            collect_query_table_refs(q, out);
        }
        Expr::In {
            expr: inner,
            target,
            ..
        } => {
            collect_expr_table_refs(inner, out);
            if let crate::ast::InTarget::Subquery(q) = target {
                collect_query_table_refs(q, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_table_refs(left, out);
            collect_expr_table_refs(right, out);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Grouped(inner)
        | Expr::Cast { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::FieldAccess { expr: inner, .. } => collect_expr_table_refs(inner, out),
        Expr::Function(call) => {
            for a in &call.args {
                collect_expr_table_refs(&a.expr, out);
            }
        }
        _ => {}
    }
}
