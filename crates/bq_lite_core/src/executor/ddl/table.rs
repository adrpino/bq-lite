use std::collections::BTreeMap;

use chrono::Utc;

use crate::ast::{
    AlterTableAction, AlterTableStmt, ColumnDef, CreateTableKind, CreateTableStmt, Expr,
    OptionEntry,
};
use crate::catalog::{ColumnSchema, TableKind, TableMeta, TableRef};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, ExecContext, ScriptBindings};
use crate::executor::evaluator::eval_expr;
use crate::executor::query::execute_query;
use crate::executor::result::QueryResult;
use crate::storage::StorageEngine;
use crate::types::coercion::coerce_value;
use crate::types::BqValue;

pub(super) fn exec_create_table(
    stmt: &CreateTableStmt,
    catalog: &mut crate::catalog::Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = if stmt.temp {
        let parts = stmt.name.normalized_parts();
        let tbl = parts
            .last()
            .ok_or_else(|| BqError::invalid_argument("Invalid TEMP TABLE name"))?;
        let default_proj = catalog.default_project().to_owned();
        let _ = catalog.create_dataset(
            &default_proj,
            crate::catalog::DatasetMeta::new("__temp", "US"),
            true,
        );
        TableRef::new(default_proj, "__temp", tbl)
    } else {
        catalog.resolve_table_name(&stmt.name)?
    };

    match stmt.kind {
        CreateTableKind::Like
        | CreateTableKind::Copy
        | CreateTableKind::Clone
        | CreateTableKind::Snapshot => exec_clone_or_like(stmt, &tref, catalog, storage, bindings),
        CreateTableKind::External => super::external_load::exec_create_external_table(
            stmt, &tref, catalog, storage, bindings,
        ),
        CreateTableKind::Table => {
            exec_standard_create_table(stmt, &tref, catalog, storage, bindings)
        }
    }
}

fn exec_standard_create_table(
    stmt: &CreateTableStmt,
    tref: &TableRef,
    catalog: &mut crate::catalog::Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let mut columns = build_columns(&stmt.columns)?;
    let mut initial_rows = Vec::new();
    if let Some(ref query) = stmt.as_query {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let qr = execute_query(query, &ctx)?;
        if columns.is_empty() {
            columns = qr
                .columns
                .iter()
                .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
                .collect();
        } else if columns.len() != qr.columns.len() {
            return Err(BqError::query("CTAS column count mismatch"));
        }
        for row in qr.rows {
            let coerced = row
                .into_iter()
                .zip(columns.iter())
                .map(|(v, col)| coerce_value(v, &col.ty))
                .collect::<BqResult<Vec<_>>>()?;
            initial_rows.push(coerced);
        }
    }
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    let now = Utc::now();
    let meta = TableMeta {
        name: tref.table.clone(),
        kind: TableKind::Table,
        columns,
        partition_by: stmt.partition_by.as_ref().map(format_partition_expr),
        cluster_by: stmt.cluster_by.iter().map(|i| i.value.clone()).collect(),
        view_query: None,
        created_at: now,
        updated_at: now,
        options,
        description: None,
        friendly_name: None,
        labels: BTreeMap::new(),
        view_sql: None,
        expiration_ms: None,
        external_uris: Vec::new(),
        external_format: None,
    };
    let created = catalog.put_table(tref, meta, stmt.or_replace, stmt.if_not_exists)?;
    if created {
        storage.replace_rows(tref, initial_rows);
    }
    Ok(QueryResult::empty())
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Fallback formats arbitrary partition expression cleanly"
)]
fn format_partition_expr(expr: &Expr) -> String {
    match expr {
        Expr::Ident(id) => id.value.clone(),
        Expr::CompoundIdent(parts) => parts
            .iter()
            .map(|p| p.value.as_str())
            .collect::<Vec<_>>()
            .join("."),
        Expr::Function(call) => {
            let fn_name = call.name.normalized_parts().join(".");
            let args: Vec<String> = call
                .args
                .iter()
                .map(|a| format_partition_expr(&a.expr))
                .collect();
            format!("{fn_name}({})", args.join(", "))
        }
        other => format!("{other:?}"),
    }
}

fn exec_clone_or_like(
    stmt: &CreateTableStmt,
    tref: &TableRef,
    catalog: &mut crate::catalog::Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let src_name = stmt
        .source_table
        .as_ref()
        .ok_or_else(|| BqError::invalid_argument("Missing source table"))?;
    let src_ref = catalog.resolve_table_name(src_name)?;
    let mut meta = catalog.get_table(&src_ref)?.clone();
    meta.name.clone_from(&tref.table);
    meta.kind = if stmt.kind == CreateTableKind::Snapshot {
        TableKind::Snapshot
    } else {
        TableKind::Table
    };
    meta.updated_at = Utc::now();
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "FOR SYSTEM_TIME AS OF expects TIMESTAMP"
    )]
    let as_of = if let Some(ref st_expr) = stmt.system_time {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        match eval_expr(st_expr, &EvalRow::default(), &ctx)? {
            BqValue::Timestamp(ts) => Some(ts),
            _ => return Err(BqError::query("FOR SYSTEM_TIME AS OF requires a TIMESTAMP")),
        }
    } else {
        None
    };
    let created = catalog.put_table(tref, meta, stmt.or_replace, stmt.if_not_exists)?;
    if created {
        if stmt.kind == CreateTableKind::Like {
            storage.create_table(tref);
        } else {
            storage.clone_table_data(&src_ref, tref, as_of)?;
        }
    }
    Ok(QueryResult::empty())
}

pub(super) fn exec_alter_table(
    stmt: &AlterTableStmt,
    catalog: &mut crate::catalog::Catalog,
    storage: &mut StorageEngine,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.name)?;
    let Ok(existing) = catalog.get_table(&tref) else {
        if stmt.if_exists {
            return Ok(QueryResult::empty());
        }
        return Err(BqError::not_found(format!(
            "Not found: Table {}:{}.{}",
            tref.project, tref.dataset, tref.table
        )));
    };
    let mut meta = existing.clone();
    let mut rows = storage.read_rows(&tref);
    let mut current_ref = tref.clone();

    for action in &stmt.actions {
        apply_alter_action(
            action,
            &mut meta,
            &mut rows,
            &mut current_ref,
            catalog,
            storage,
        )?;
    }
    meta.updated_at = Utc::now();
    storage.replace_rows(&current_ref, rows);
    catalog.put_table(&current_ref, meta, true, false)?;
    Ok(QueryResult::empty())
}

fn apply_alter_action(
    action: &AlterTableAction,
    meta: &mut TableMeta,
    rows: &mut [Vec<BqValue>],
    current_ref: &mut TableRef,
    catalog: &mut crate::catalog::Catalog,
    storage: &mut StorageEngine,
) -> BqResult<()> {
    match action {
        AlterTableAction::AddColumn {
            if_not_exists,
            column,
        } => alter_add_column(meta, rows, *if_not_exists, column, catalog, storage)?,
        AlterTableAction::DropColumn { if_exists, name } => {
            alter_drop_column(meta, rows, *if_exists, &name.value)?;
        }
        AlterTableAction::RenameColumn { from, to } => {
            let idx = meta
                .column_index(&from.value)
                .ok_or_else(|| BqError::not_found(format!("Column '{}' not found", from.value)))?;
            if let Some(col) = meta.columns.get_mut(idx) {
                col.name.clone_from(&to.value);
            }
        }
        AlterTableAction::RenameTo(new_name) => {
            let parts = new_name.normalized_parts();
            let new_tbl = parts
                .last()
                .ok_or_else(|| BqError::invalid_argument("Invalid RENAME TO name"))?;
            let new_ref = TableRef::new(&current_ref.project, &current_ref.dataset, new_tbl);
            storage.drop_table(current_ref);
            catalog.drop_table(current_ref, false)?;
            meta.name.clone_from(new_tbl);
            *current_ref = new_ref;
        }
        AlterTableAction::SetOptions(opts) => {
            let evaluated = eval_options(opts, catalog, storage, ScriptBindings::default())?;
            meta.options.extend(evaluated);
        }
        AlterTableAction::AlterColumnSetDataType { name, ty } => {
            alter_column_data_type(meta, rows, &name.value, ty)?;
        }
        AlterTableAction::AlterColumnSetDefault { name, expr } => {
            let idx = find_col_idx(meta, &name.value)?;
            if let Some(col) = meta.columns.get_mut(idx) {
                col.default_expr = Some(expr.clone());
            }
        }
        AlterTableAction::AlterColumnDropDefault { name } => {
            let idx = find_col_idx(meta, &name.value)?;
            if let Some(col) = meta.columns.get_mut(idx) {
                col.default_expr = None;
            }
        }
        AlterTableAction::AlterColumnDropNotNull { name } => {
            let idx = find_col_idx(meta, &name.value)?;
            if let Some(col) = meta.columns.get_mut(idx) {
                col.nullable = true;
            }
        }
        AlterTableAction::AlterColumnSetOptions { name, options } => {
            let idx = find_col_idx(meta, &name.value)?;
            let evaluated = eval_options(options, catalog, storage, ScriptBindings::default())?;
            if let Some(col) = meta.columns.get_mut(idx) {
                apply_column_options(col, &evaluated);
            }
        }
    }
    Ok(())
}

fn apply_column_options(col: &mut ColumnSchema, evaluated: &BTreeMap<String, BqValue>) {
    if let Some(BqValue::String(desc)) = evaluated.get("description") {
        col.description = Some(desc.clone());
    }
    if let Some(tag) = extract_policy_tag(evaluated) {
        col.policy_tag = Some(tag);
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only STRING or ARRAY<STRING> can set policy_tag"
)]
fn extract_policy_tag(evaluated: &BTreeMap<String, BqValue>) -> Option<String> {
    if let Some(BqValue::String(s)) = evaluated.get("policy_tag") {
        return Some(s.clone());
    }
    match evaluated.get("policy_tags") {
        Some(BqValue::String(s)) => Some(s.clone()),
        Some(BqValue::Array(items)) => items.first().and_then(|v| match v {
            BqValue::String(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn find_col_idx(meta: &TableMeta, name: &str) -> BqResult<usize> {
    meta.column_index(name)
        .ok_or_else(|| BqError::not_found(format!("Column '{name}' not found")))
}

fn alter_add_column(
    meta: &mut TableMeta,
    rows: &mut [Vec<BqValue>],
    if_not_exists: bool,
    column: &ColumnDef,
    catalog: &crate::catalog::Catalog,
    storage: &StorageEngine,
) -> BqResult<()> {
    if meta.column_index(&column.name.value).is_some() {
        if if_not_exists {
            return Ok(());
        }
        return Err(BqError::already_exists(format!(
            "Column '{}' already exists",
            column.name.value
        )));
    }
    let col_schema = column_def_to_schema(column)?;
    let fill = match &col_schema.default_expr {
        Some(e) => eval_expr(e, &EvalRow::default(), &ExecContext::new(catalog, storage))?,
        None => BqValue::Null,
    };
    meta.columns.push(col_schema);
    for row in rows.iter_mut() {
        row.push(fill.clone());
    }
    Ok(())
}

fn alter_drop_column(
    meta: &mut TableMeta,
    rows: &mut [Vec<BqValue>],
    if_exists: bool,
    name: &str,
) -> BqResult<()> {
    let Some(idx) = meta.column_index(name) else {
        if if_exists {
            return Ok(());
        }
        return Err(BqError::not_found(format!("Column '{name}' not found")));
    };
    meta.columns.remove(idx);
    for row in rows.iter_mut() {
        if idx < row.len() {
            row.remove(idx);
        }
    }
    Ok(())
}

fn alter_column_data_type(
    meta: &mut TableMeta,
    rows: &mut [Vec<BqValue>],
    name: &str,
    ty: &crate::ast::DataTypeAst,
) -> BqResult<()> {
    let idx = find_col_idx(meta, name)?;
    let new_ty = ty.to_bq_type()?;
    if let Some(col) = meta.columns.get_mut(idx) {
        col.ty = new_ty.clone();
    }
    for row in rows.iter_mut() {
        if let Some(slot) = row.get_mut(idx) {
            *slot = coerce_value(slot.clone(), &new_ty)?;
        }
    }
    Ok(())
}

pub(super) fn build_columns(defs: &[ColumnDef]) -> BqResult<Vec<ColumnSchema>> {
    defs.iter().map(column_def_to_schema).collect()
}

fn column_def_to_schema(def: &ColumnDef) -> BqResult<ColumnSchema> {
    let ty = def.ty.to_bq_type()?;
    let mut policy_tag = None;
    for opt in &def.options {
        if opt.name.value.eq_ignore_ascii_case("policy_tag")
            || opt.name.value.eq_ignore_ascii_case("policy_tags")
        {
            if let Expr::Literal(crate::ast::Literal::String(s)) = &opt.value {
                policy_tag = Some(s.clone());
            }
        }
    }
    Ok(ColumnSchema {
        name: def.name.value.clone(),
        ty,
        nullable: !def.not_null,
        default_expr: def.default_expr.clone(),
        description: None,
        policy_tag,
    })
}

pub(super) fn eval_options(
    entries: &[OptionEntry],
    catalog: &crate::catalog::Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<BTreeMap<String, BqValue>> {
    let ctx = ExecContext::with_bindings(catalog, storage, bindings);
    let empty = EvalRow::default();
    let mut map = BTreeMap::new();
    for entry in entries {
        let val = eval_expr(&entry.value, &empty, &ctx)?;
        map.insert(entry.name.value.to_ascii_lowercase(), val);
    }
    Ok(map)
}
