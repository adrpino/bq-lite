mod assign;
mod merge;
mod pk;

use crate::ast::{
    DeleteStmt, Ident, InsertSource, InsertStmt, Statement, TruncateStmt, UpdateStmt,
};
use crate::catalog::{Catalog, TableMeta};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding, ScriptBindings};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::{eval_from_clause_public, execute_query};
use crate::executor::result::QueryResult;
use crate::storage::StorageEngine;
use crate::types::coercion::coerce_value;
use crate::types::BqValue;

use assign::{
    apply_assignment, build_returning_result, build_returning_result_with_actions,
    check_assert_rows_modified, eval_update_val, is_default_sentinel, schema_eval_row, AssignCtx,
};
use merge::exec_merge;
use pk::{apply_pk_insert, verify_pk_uniqueness};

pub(in crate::executor) fn execute_dml(
    stmt: &Statement,
    catalog: &Catalog,
    storage: &mut StorageEngine,
) -> BqResult<Option<QueryResult>> {
    execute_dml_with_bindings(stmt, catalog, storage, ScriptBindings::default())
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Non-DML Statement variants fall through to None"
)]
pub(in crate::executor) fn execute_dml_with_bindings(
    stmt: &Statement,
    catalog: &Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<Option<QueryResult>> {
    match stmt {
        Statement::Insert(ins) => exec_insert(ins, catalog, storage, bindings).map(Some),
        Statement::Update(upd) => exec_update(upd, catalog, storage, bindings).map(Some),
        Statement::Delete(del) => exec_delete(del, catalog, storage, bindings).map(Some),
        Statement::Merge(m) => exec_merge(m, catalog, storage, bindings).map(Some),
        Statement::Truncate(tr) => exec_truncate(tr, catalog, storage).map(Some),
        _ => Ok(None),
    }
}

fn exec_insert(
    stmt: &InsertStmt,
    catalog: &Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table)?;
    let meta = catalog.get_table(&tref)?.clone();
    let outcome = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let raw_rows: Vec<Vec<BqValue>> = match &stmt.source {
            InsertSource::Values(rows_expr) => {
                let empty = EvalRow::default();
                let mut out = Vec::with_capacity(rows_expr.len());
                for row_e in rows_expr {
                    let row_values = row_e
                        .iter()
                        .enumerate()
                        .map(|(col_pos, e)| {
                            eval_insert_val(&meta, &stmt.columns, col_pos, e, &empty, &ctx)
                        })
                        .collect::<BqResult<Vec<_>>>()?;
                    out.push(row_values);
                }
                out
            }
            InsertSource::Query(q) => execute_query(q, &ctx)?.rows,
        };
        let mut out = Vec::with_capacity(raw_rows.len());
        for raw in raw_rows {
            out.push(finalize_new_row(&meta, &stmt.columns, raw, &ctx)?);
        }
        let existing = if meta.primary_key.is_empty() {
            Vec::new()
        } else {
            storage.read_rows(&tref)
        };
        let outcome = apply_pk_insert(&meta, stmt.mode, &stmt.columns, existing, out)?;
        check_assert_rows_modified(stmt.assert_rows_modified.as_ref(), outcome.affected, &ctx)?;
        outcome
    };

    if let Some(replaced) = outcome.replaced_table {
        storage.replace_rows(&tref, replaced);
    } else if !outcome.append_rows.is_empty() {
        storage.insert_rows(&tref, outcome.append_rows)?;
    }

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(outcome.affected)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            let schema = schema_eval_row(&meta, &meta.name);
            let eval_rows: Vec<EvalRow> = outcome
                .modified
                .iter()
                .map(|(r, _)| row_to_eval(&meta, r, &meta.name))
                .collect();
            let actions: Vec<&str> = outcome.modified.iter().map(|(_, a)| *a).collect();
            build_returning_result_with_actions(&schema, &eval_rows, returning, &actions, &ctx)
        },
    )
}

fn eval_insert_val(
    meta: &TableMeta,
    target_cols: &[Ident],
    col_pos: usize,
    expr: &crate::ast::Expr,
    empty: &EvalRow,
    ctx: &ExecContext<'_>,
) -> BqResult<BqValue> {
    if !is_default_sentinel(expr) {
        return eval_expr(expr, empty, ctx);
    }
    let col = if target_cols.is_empty() {
        meta.columns.get(col_pos)
    } else {
        target_cols
            .get(col_pos)
            .and_then(|id| meta.column_index(&id.value))
            .and_then(|idx| meta.columns.get(idx))
    };
    col.and_then(|c| c.default_expr.as_ref())
        .map_or(Ok(BqValue::Null), |def| eval_expr(def, empty, ctx))
}

pub(super) fn finalize_new_row(
    meta: &TableMeta,
    target_cols: &[Ident],
    values: Vec<BqValue>,
    ctx: &ExecContext<'_>,
) -> BqResult<Vec<BqValue>> {
    let mut row = vec![BqValue::Null; meta.columns.len()];
    if target_cols.is_empty() {
        if values.len() != meta.columns.len() {
            return Err(BqError::query(format!(
                "INSERT column count mismatch: expected {}, got {}",
                meta.columns.len(),
                values.len()
            )));
        }
        for (idx, (col, val)) in meta.columns.iter().zip(values).enumerate() {
            let coerced = coerce_value(val, &col.ty)?;
            if !col.nullable && coerced.is_null() {
                return Err(BqError::invalid_argument(format!(
                    "Column '{}' cannot be NULL",
                    col.name
                )));
            }
            if let Some(slot) = row.get_mut(idx) {
                *slot = coerced;
            }
        }
        return Ok(row);
    }

    if values.len() != target_cols.len() {
        return Err(BqError::query("INSERT column/value count mismatch"));
    }
    let mut provided = vec![false; meta.columns.len()];
    for (ident, val) in target_cols.iter().zip(values) {
        let idx = meta
            .column_index(&ident.value)
            .ok_or_else(|| BqError::not_found(format!("Column '{}' not found", ident.value)))?;
        let col = meta
            .columns
            .get(idx)
            .ok_or_else(|| BqError::internal("column idx"))?;
        let coerced = coerce_value(val, &col.ty)?;
        if let Some(slot) = row.get_mut(idx) {
            *slot = coerced;
        }
        if let Some(flag) = provided.get_mut(idx) {
            *flag = true;
        }
    }

    let empty = EvalRow::default();
    for (idx, col) in meta.columns.iter().enumerate() {
        if !provided.get(idx).copied().unwrap_or(false) {
            if let Some(ref def_expr) = col.default_expr {
                let def_val = coerce_value(eval_expr(def_expr, &empty, ctx)?, &col.ty)?;
                if let Some(slot) = row.get_mut(idx) {
                    *slot = def_val;
                }
            }
        }
        if !col.nullable && row.get(idx).is_none_or(BqValue::is_null) {
            return Err(BqError::invalid_argument(format!(
                "Required column '{}' cannot be NULL",
                col.name
            )));
        }
    }
    Ok(row)
}

fn exec_update(
    stmt: &UpdateStmt,
    catalog: &Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table)?;
    let meta = catalog.get_table(&tref)?.clone();
    let qual = stmt
        .alias
        .as_ref()
        .map_or_else(|| meta.name.clone(), |a| a.value.clone());
    let has_from = !stmt.from.is_empty();

    let (out_rows, returned_evals, affected) = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let from_rows = if has_from {
            eval_from_clause_public(&stmt.from, &ctx)?
        } else {
            Vec::new()
        };
        let rows = storage.read_rows(&tref);
        let mut out_rows = Vec::with_capacity(rows.len());
        let mut returned_evals = Vec::new();
        let mut affected = 0_u64;

        for row in rows {
            let base_eval = row_to_eval(&meta, &row, &qual);
            let matched = find_update_match(
                &base_eval,
                has_from,
                &from_rows,
                stmt.where_clause.as_ref(),
                &ctx,
            )?;
            if let Some((eval_row, matched_from)) = matched {
                let mut updated = row;
                let mut actx = AssignCtx::new(&eval_row, &ctx);
                for assign in &stmt.assignments {
                    let val = eval_update_val(&meta, &assign.path, &assign.value, &eval_row, &ctx)?;
                    apply_assignment(&meta, &mut updated, &assign.path, val, &mut actx)?;
                }
                let mut ret_row = row_to_eval(&meta, &updated, &qual);
                if let Some(frow) = matched_from {
                    ret_row.cols.extend(frow.cols);
                }
                returned_evals.push(ret_row);
                out_rows.push(updated);
                affected += 1;
            } else {
                out_rows.push(row);
            }
        }
        verify_pk_uniqueness(&meta, &out_rows)?;
        check_assert_rows_modified(stmt.assert_rows_modified.as_ref(), affected, &ctx)?;
        (out_rows, returned_evals, affected)
    };
    storage.replace_rows(&tref, out_rows);

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(affected)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            let schema = schema_eval_row(&meta, &qual);
            build_returning_result(&schema, &returned_evals, returning, "UPDATE", &ctx)
        },
    )
}

fn find_update_match(
    base: &EvalRow,
    has_from: bool,
    from_rows: &[EvalRow],
    where_clause: Option<&crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<(EvalRow, Option<EvalRow>)>> {
    if !has_from {
        let matched = match where_clause {
            Some(pred) => matches!(eval_expr(pred, base, ctx)?, BqValue::Bool(true)),
            None => true,
        };
        return Ok(if matched {
            Some((base.clone(), None))
        } else {
            None
        });
    }
    let mut found: Option<(EvalRow, Option<EvalRow>)> = None;
    for frow in from_rows {
        let mut combined_cols = base.cols.clone();
        combined_cols.extend(frow.cols.iter().cloned());
        let combined = EvalRow::new(combined_cols);
        let matched = match where_clause {
            Some(pred) => matches!(eval_expr(pred, &combined, ctx)?, BqValue::Bool(true)),
            None => true,
        };
        if matched {
            if found.is_some() {
                return Err(BqError::semantic(
                    "UPDATE with join requires that each row of the table being updated correspond with at most one joined row that matches the WHERE clause",
                ));
            }
            found = Some((combined, Some(frow.clone())));
        }
    }
    Ok(found)
}

fn exec_delete(
    stmt: &DeleteStmt,
    catalog: &Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table)?;
    let meta = catalog.get_table(&tref)?.clone();
    let qual = stmt
        .alias
        .as_ref()
        .map_or_else(|| meta.name.clone(), |a| a.value.clone());
    let has_using = !stmt.using.is_empty();

    let (kept, deleted_evals, affected) = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let using_rows = if has_using {
            eval_from_clause_public(&stmt.using, &ctx)?
        } else {
            Vec::new()
        };
        let rows = storage.read_rows(&tref);
        let mut kept = Vec::with_capacity(rows.len());
        let mut deleted_evals = Vec::new();
        let mut affected = 0_u64;

        for row in rows {
            let eval_row = row_to_eval(&meta, &row, &qual);
            let match_del = matches_delete_row(
                &eval_row,
                has_using,
                &using_rows,
                stmt.where_clause.as_ref(),
                &ctx,
            )?;
            if match_del {
                deleted_evals.push(eval_row);
                affected += 1;
            } else {
                kept.push(row);
            }
        }
        check_assert_rows_modified(stmt.assert_rows_modified.as_ref(), affected, &ctx)?;
        (kept, deleted_evals, affected)
    };
    storage.replace_rows(&tref, kept);

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(affected)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            let schema = schema_eval_row(&meta, &qual);
            build_returning_result(&schema, &deleted_evals, returning, "DELETE", &ctx)
        },
    )
}

fn matches_delete_row(
    base: &EvalRow,
    has_using: bool,
    using_rows: &[EvalRow],
    where_clause: Option<&crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<bool> {
    if !has_using {
        return Ok(match where_clause {
            Some(pred) => matches!(eval_expr(pred, base, ctx)?, BqValue::Bool(true)),
            None => true,
        });
    }
    for urow in using_rows {
        let mut combined_cols = base.cols.clone();
        combined_cols.extend(urow.cols.iter().cloned());
        let combined = EvalRow::new(combined_cols);
        let matched = match where_clause {
            Some(pred) => matches!(eval_expr(pred, &combined, ctx)?, BqValue::Bool(true)),
            None => true,
        };
        if matched {
            return Ok(true);
        }
    }
    Ok(false)
}

fn exec_truncate(
    stmt: &TruncateStmt,
    catalog: &Catalog,
    storage: &mut StorageEngine,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table)?;
    let _ = catalog.get_table(&tref)?;
    let count = u64::try_from(storage.read_rows(&tref).len()).unwrap_or(u64::MAX);
    storage.replace_rows(&tref, Vec::new());
    Ok(QueryResult::affected(count))
}

fn row_to_eval(meta: &TableMeta, row: &[BqValue], qual: &str) -> EvalRow {
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
