use crate::ast::{MergeAction, MergeConditionKind, MergeStmt, UpdateAssignment};
use crate::catalog::TableMeta;
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding, ScriptBindings};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::eval_table_factor_public;
use crate::executor::result::QueryResult;
use crate::storage::StorageEngine;
use crate::types::BqValue;

use super::{apply_assignment, finalize_new_row};

pub(super) fn exec_merge(
    stmt: &MergeStmt,
    catalog: &crate::catalog::Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.target)?;
    let meta = catalog.get_table(&tref)?.clone();
    let target_qual = stmt
        .target_alias
        .as_ref()
        .map_or_else(|| meta.name.clone(), |a| a.value.clone());

    let mut out_rows: Vec<Vec<BqValue>> = Vec::with_capacity(storage.read_rows(&tref).len());
    let affected = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let target_rows = storage.read_rows(&tref);
        let source_rows = eval_table_factor_public(&stmt.source, &ctx)?;

        let mut target_matched = vec![false; target_rows.len()];
        let mut source_matched = vec![false; source_rows.len()];
        let mut affected = 0_u64;

        for (t_idx, trow) in target_rows.iter().enumerate() {
            let t_eval = make_target_eval_row(&meta, trow, &target_qual);
            let mut matches = Vec::new();
            for (s_idx, srow) in source_rows.iter().enumerate() {
                let combined = combine_eval(&t_eval, srow);
                if matches!(eval_expr(&stmt.on, &combined, &ctx)?, BqValue::Bool(true)) {
                    matches.push((s_idx, combined));
                }
            }
            if matches.len() > 1 {
                return Err(BqError::invalid_argument(
                    "UPDATE/MERGE must match at most one source row for each target row",
                ));
            }
            if let Some((s_idx, combined)) = matches.into_iter().next() {
                if let Some(slot) = target_matched.get_mut(t_idx) {
                    *slot = true;
                }
                if let Some(slot) = source_matched.get_mut(s_idx) {
                    *slot = true;
                }
                apply_matched_clauses(
                    stmt,
                    &meta,
                    trow,
                    &combined,
                    &ctx,
                    (&mut out_rows, &mut affected),
                )?;
            } else {
                apply_not_matched_by_source(
                    stmt,
                    &meta,
                    trow,
                    &t_eval,
                    &ctx,
                    (&mut out_rows, &mut affected),
                )?;
            }
        }

        for (s_idx, srow) in source_rows.iter().enumerate() {
            if !source_matched.get(s_idx).copied().unwrap_or(false) {
                apply_not_matched_by_target(stmt, &meta, srow, &ctx, &mut out_rows, &mut affected)?;
            }
        }

        affected
    };
    storage.replace_rows(&tref, out_rows);
    Ok(QueryResult::affected(affected))
}

fn apply_matched_clauses(
    stmt: &MergeStmt,
    meta: &TableMeta,
    trow: &[BqValue],
    combined: &EvalRow,
    ctx: &ExecContext<'_>,
    out_state: (&mut Vec<Vec<BqValue>>, &mut u64),
) -> BqResult<()> {
    let (out_rows, affected) = out_state;
    for clause in &stmt.clauses {
        if clause.kind != MergeConditionKind::Matched {
            continue;
        }
        if let Some(ref pred) = clause.predicate {
            if !matches!(eval_expr(pred, combined, ctx)?, BqValue::Bool(true)) {
                continue;
            }
        }
        match &clause.action {
            MergeAction::Update(assignments) => {
                let updated = update_row_with_assignments(meta, trow, assignments, combined, ctx)?;
                out_rows.push(updated);
                *affected += 1;
                return Ok(());
            }
            MergeAction::Delete => {
                *affected += 1;
                return Ok(());
            }
            MergeAction::Insert { .. } => {
                return Err(BqError::query("INSERT is not valid in WHEN MATCHED"));
            }
        }
    }
    out_rows.push(trow.to_vec());
    Ok(())
}

fn apply_not_matched_by_source(
    stmt: &MergeStmt,
    meta: &TableMeta,
    trow: &[BqValue],
    t_eval: &EvalRow,
    ctx: &ExecContext<'_>,
    out_state: (&mut Vec<Vec<BqValue>>, &mut u64),
) -> BqResult<()> {
    let (out_rows, affected) = out_state;
    for clause in &stmt.clauses {
        if clause.kind != MergeConditionKind::NotMatchedBySource {
            continue;
        }
        if let Some(ref pred) = clause.predicate {
            if !matches!(eval_expr(pred, t_eval, ctx)?, BqValue::Bool(true)) {
                continue;
            }
        }
        match &clause.action {
            MergeAction::Update(assignments) => {
                let updated = update_row_with_assignments(meta, trow, assignments, t_eval, ctx)?;
                out_rows.push(updated);
                *affected += 1;
                return Ok(());
            }
            MergeAction::Delete => {
                *affected += 1;
                return Ok(());
            }
            MergeAction::Insert { .. } => {
                return Err(BqError::query(
                    "INSERT is not valid in WHEN NOT MATCHED BY SOURCE",
                ));
            }
        }
    }
    out_rows.push(trow.to_vec());
    Ok(())
}

fn apply_not_matched_by_target(
    stmt: &MergeStmt,
    meta: &TableMeta,
    srow: &EvalRow,
    ctx: &ExecContext<'_>,
    out_rows: &mut Vec<Vec<BqValue>>,
    affected: &mut u64,
) -> BqResult<()> {
    for clause in &stmt.clauses {
        if clause.kind != MergeConditionKind::NotMatchedByTarget {
            continue;
        }
        if let Some(ref pred) = clause.predicate {
            if !matches!(eval_expr(pred, srow, ctx)?, BqValue::Bool(true)) {
                continue;
            }
        }
        if let MergeAction::Insert { columns, values } = &clause.action {
            let new_row = if let Some(exprs) = values {
                let evaluated = exprs
                    .iter()
                    .map(|e| eval_expr(e, srow, ctx))
                    .collect::<BqResult<Vec<_>>>()?;
                finalize_new_row(meta, columns, evaluated, ctx)?
            } else {
                let raw: Vec<BqValue> = srow.cols.iter().map(|c| c.value.clone()).collect();
                finalize_new_row(meta, columns, raw, ctx)?
            };
            out_rows.push(new_row);
            *affected += 1;
            return Ok(());
        }
        return Err(BqError::query(
            "UPDATE/DELETE is not valid in WHEN NOT MATCHED BY TARGET",
        ));
    }
    Ok(())
}

fn update_row_with_assignments(
    meta: &TableMeta,
    trow: &[BqValue],
    assignments: &[UpdateAssignment],
    eval_row: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    let mut updated = trow.to_vec();
    for assign in assignments {
        let val = eval_expr(&assign.value, eval_row, ctx)?;
        apply_assignment(meta, &mut updated, &assign.path, val)?;
    }
    Ok(updated)
}

fn make_target_eval_row(meta: &TableMeta, row: &[BqValue], qual: &str) -> EvalRow {
    let qual_arc: Option<std::sync::Arc<str>> = Some(std::sync::Arc::from(qual));
    EvalRow::new(
        meta.columns
            .iter()
            .zip(row)
            .map(|(col, val)| RowBinding {
                qualifier: qual_arc.clone(),
                name: std::sync::Arc::from(col.name.as_str()),
                ty: col.ty.clone(),
                value: val.clone(),
            })
            .collect(),
    )
}

fn combine_eval(a: &EvalRow, b: &EvalRow) -> EvalRow {
    let mut cols = Vec::with_capacity(a.cols.len() + b.cols.len());
    cols.extend(a.cols.iter().cloned());
    cols.extend(b.cols.iter().cloned());
    EvalRow::new(cols)
}
