mod merge;

use crate::ast::{
    DeleteStmt, Ident, InsertSource, InsertStmt, Select, SelectItem, Statement, TruncateStmt,
    UpdateStmt,
};
use crate::catalog::{Catalog, TableMeta};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, RowBinding, ScriptBindings};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::{eval_from_clause_public, execute_query, project_eval_rows_public};
use crate::executor::result::{QueryColumn, QueryResult};
use crate::storage::StorageEngine;
use crate::types::coercion::coerce_value;
use crate::types::BqValue;

use merge::exec_merge;

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
    let prepared = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let raw_rows: Vec<Vec<BqValue>> = match &stmt.source {
            InsertSource::Values(rows_expr) => {
                let empty = EvalRow::default();
                let mut out = Vec::with_capacity(rows_expr.len());
                for row_e in rows_expr {
                    let row_values = row_e
                        .iter()
                        .map(|e| eval_expr(e, &empty, &ctx))
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
        out
    };

    let count = u64::try_from(prepared.len()).unwrap_or(u64::MAX);
    storage.insert_rows(&tref, prepared.clone())?;

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(count)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            build_returning_result(&meta, None, prepared, returning, &ctx)
        },
    )
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

    let (out_rows, returned_rows, affected) = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let from_rows = if stmt.from.is_empty() {
            Vec::new()
        } else {
            eval_from_clause_public(&stmt.from, &ctx)?
        };
        let rows = storage.read_rows(&tref);
        let mut out_rows = Vec::with_capacity(rows.len());
        let mut returned_rows = Vec::new();
        let mut affected = 0_u64;

        for row in rows {
            let base_eval = row_to_eval(&meta, &row, &qual);
            let matched_eval =
                find_update_match(&base_eval, &from_rows, stmt.where_clause.as_ref(), &ctx)?;
            if let Some(eval_row) = matched_eval {
                let mut updated = row;
                for assign in &stmt.assignments {
                    let val = eval_expr(&assign.value, &eval_row, &ctx)?;
                    apply_assignment(&meta, &mut updated, &assign.path, val)?;
                }
                returned_rows.push(updated.clone());
                out_rows.push(updated);
                affected += 1;
            } else {
                out_rows.push(row);
            }
        }
        (out_rows, returned_rows, affected)
    };
    storage.replace_rows(&tref, out_rows);

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(affected)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            build_returning_result(&meta, Some(&qual), returned_rows, returning, &ctx)
        },
    )
}

fn find_update_match(
    base: &EvalRow,
    from_rows: &[EvalRow],
    where_clause: Option<&crate::ast::Expr>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<EvalRow>> {
    if from_rows.is_empty() {
        let matched = match where_clause {
            Some(pred) => matches!(eval_expr(pred, base, ctx)?, BqValue::Bool(true)),
            None => true,
        };
        return Ok(if matched { Some(base.clone()) } else { None });
    }
    for frow in from_rows {
        let mut combined_cols = base.cols.clone();
        combined_cols.extend(frow.cols.iter().cloned());
        let combined = EvalRow::new(combined_cols);
        let matched = match where_clause {
            Some(pred) => matches!(eval_expr(pred, &combined, ctx)?, BqValue::Bool(true)),
            None => true,
        };
        if matched {
            return Ok(Some(combined));
        }
    }
    Ok(None)
}

pub(super) fn apply_assignment(
    meta: &TableMeta,
    row: &mut [BqValue],
    path: &[Ident],
    val: BqValue,
) -> BqResult<()> {
    let [first, rest @ ..] = path else {
        return Err(BqError::query("Empty assignment target"));
    };
    let (col_ident, field_path) = if meta.column_index(&first.value).is_some() {
        (first, rest)
    } else if let Some(second) = rest.first() {
        (second, rest.get(1..).unwrap_or(&[]))
    } else {
        return Err(BqError::not_found(format!(
            "Column '{}' not found",
            first.value
        )));
    };
    let idx = meta
        .column_index(&col_ident.value)
        .ok_or_else(|| BqError::not_found(format!("Column '{}' not found", col_ident.value)))?;
    let col_schema = meta
        .columns
        .get(idx)
        .ok_or_else(|| BqError::internal("column idx"))?;
    if field_path.is_empty() {
        let coerced = coerce_value(val, &col_schema.ty)?;
        if !col_schema.nullable && coerced.is_null() {
            return Err(BqError::invalid_argument(format!(
                "Column '{}' cannot be NULL",
                col_schema.name
            )));
        }
        if let Some(slot) = row.get_mut(idx) {
            *slot = coerced;
        }
        return Ok(());
    }
    if let Some(slot) = row.get_mut(idx) {
        update_nested_struct(slot, field_path, val)?;
    }
    Ok(())
}

fn update_nested_struct(target: &mut BqValue, path: &[Ident], val: BqValue) -> BqResult<()> {
    let [head, tail @ ..] = path else {
        *target = val;
        return Ok(());
    };
    let BqValue::Struct(fields) = target else {
        return Err(BqError::query(format!(
            "Cannot update nested field '{}' on non-STRUCT",
            head.value
        )));
    };
    let field = fields
        .iter_mut()
        .find(|f| {
            f.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&head.value))
        })
        .ok_or_else(|| BqError::not_found(format!("STRUCT field '{}' not found", head.value)))?;
    if tail.is_empty() {
        field.value = val;
        Ok(())
    } else {
        update_nested_struct(&mut field.value, tail, val)
    }
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

    let (kept, deleted, affected) = {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let rows = storage.read_rows(&tref);
        let mut kept = Vec::with_capacity(rows.len());
        let mut deleted = Vec::new();
        let mut affected = 0_u64;

        for row in rows {
            let eval_row = row_to_eval(&meta, &row, &qual);
            let match_del = match stmt.where_clause {
                Some(ref pred) => matches!(eval_expr(pred, &eval_row, &ctx)?, BqValue::Bool(true)),
                None => true,
            };
            if match_del {
                deleted.push(row);
                affected += 1;
            } else {
                kept.push(row);
            }
        }
        (kept, deleted, affected)
    };
    storage.replace_rows(&tref, kept);

    stmt.returning.as_ref().map_or_else(
        || Ok(QueryResult::affected(affected)),
        |returning| {
            let ctx = ExecContext::with_bindings(catalog, storage, bindings);
            build_returning_result(&meta, Some(&qual), deleted, returning, &ctx)
        },
    )
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
    EvalRow::new(
        meta.columns
            .iter()
            .zip(row)
            .map(|(col, val)| RowBinding {
                qualifier: Some(qual.to_owned()),
                name: col.name.clone(),
                ty: col.ty.clone(),
                value: val.clone(),
            })
            .collect(),
    )
}

fn build_returning_result(
    meta: &TableMeta,
    qual: Option<&str>,
    rows: Vec<Vec<BqValue>>,
    returning: &[SelectItem],
    ctx: &ExecContext<'_>,
) -> BqResult<QueryResult> {
    let q = qual.unwrap_or(&meta.name);
    let eval_rows: Vec<EvalRow> = rows.into_iter().map(|r| row_to_eval(meta, &r, q)).collect();
    let dummy_sel = Select {
        distinct: false,
        as_kind: None,
        projection: returning.to_vec(),
        from: Vec::new(),
        where_clause: None,
        group_by: None,
        having: None,
        qualify: None,
        window: Vec::new(),
    };
    let projected = project_eval_rows_public(&dummy_sel, &eval_rows, ctx)?;
    let cols = projected
        .first()
        .map(|r| {
            r.cols
                .iter()
                .map(|c| QueryColumn::new(c.name.clone(), c.ty.clone()))
                .collect()
        })
        .unwrap_or_default();
    let out_rows = projected
        .into_iter()
        .map(|r| r.cols.into_iter().map(|c| c.value).collect())
        .collect();
    let mut qr = QueryResult::new(cols, out_rows);
    qr.rows_affected = u64::try_from(qr.rows.len()).unwrap_or(u64::MAX);
    Ok(qr)
}
