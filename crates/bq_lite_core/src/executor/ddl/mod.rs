mod external_load;
mod table;

use chrono::Utc;

use crate::ast::{
    CreateFunctionStmt, CreateProcedureStmt, CreateRowAccessPolicyStmt, CreateSchemaStmt,
    CreateViewStmt, DropObjectKind, DropStmt, ExportDataStmt, Statement,
};
use crate::catalog::{
    Catalog, ColumnSchema, DatasetMeta, RoutineMeta, RowAccessPolicyMeta, TableKind, TableMeta,
    TableRef,
};
use crate::error::{BqError, BqResult};
use crate::executor::context::{ExecContext, ScriptBindings};
use crate::executor::query::execute_query;
use crate::executor::result::QueryResult;
use crate::storage::StorageEngine;
use crate::types::BqValue;

pub(in crate::executor) use external_load::load_external_rows;
use external_load::{exec_alter_schema, exec_alter_view, exec_load_data};
use table::{eval_options, exec_alter_table, exec_create_table};

pub(in crate::executor) fn execute_ddl(
    stmt: &Statement,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
) -> BqResult<Option<QueryResult>> {
    execute_ddl_with_bindings(stmt, catalog, storage, ScriptBindings::default())
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Non-DDL Statement variants fall through to None"
)]
pub(in crate::executor) fn execute_ddl_with_bindings(
    stmt: &Statement,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<Option<QueryResult>> {
    match stmt {
        Statement::CreateSchema(cs) => exec_create_schema(cs, catalog, storage, bindings).map(Some),
        Statement::AlterSchema(als) => exec_alter_schema(als, catalog, storage, bindings).map(Some),
        Statement::CreateTable(ct) => exec_create_table(ct, catalog, storage, bindings).map(Some),
        Statement::AlterTable(at) => exec_alter_table(at, catalog, storage).map(Some),
        Statement::CreateView(cv) => exec_create_view(cv, catalog, storage, bindings).map(Some),
        Statement::AlterView(av) => exec_alter_view(av, catalog, storage, bindings).map(Some),
        Statement::CreateFunction(cf) => exec_create_function(cf, catalog, storage).map(Some),
        Statement::CreateProcedure(cp) => exec_create_procedure(cp, catalog, storage).map(Some),
        Statement::CreateRowAccessPolicy(rap) => exec_create_policy(rap, catalog).map(Some),
        Statement::ExportData(ed) => exec_export_data(ed, catalog, storage, bindings).map(Some),
        Statement::LoadData(ld) => exec_load_data(ld, catalog, storage, bindings).map(Some),
        Statement::Drop(ds) => exec_drop(ds, catalog, storage).map(Some),
        _ => Ok(None),
    }
}

fn exec_create_schema(
    stmt: &CreateSchemaStmt,
    catalog: &mut Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let (project, ds_name) = catalog.resolve_dataset_name(&stmt.name)?;
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Only STRING option value sets location"
    )]
    let location = options
        .get("location")
        .and_then(|v| match v {
            BqValue::String(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "US".to_owned());
    let is_case_insensitive = matches!(
        options.get("is_case_insensitive"),
        Some(BqValue::Bool(true))
    );
    let mut ds = DatasetMeta::new(ds_name, location);
    ds.is_case_insensitive = is_case_insensitive;
    ds.options = options;
    catalog.create_dataset(&project, ds, stmt.if_not_exists)?;
    Ok(QueryResult::empty())
}

fn exec_create_view(
    stmt: &CreateViewStmt,
    catalog: &mut Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.name)?;
    let ctx = ExecContext::with_bindings(catalog, storage, bindings);
    let qr = execute_query(&stmt.query, &ctx)?;
    let mut columns: Vec<ColumnSchema> = qr
        .columns
        .iter()
        .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
        .collect();
    if !stmt.columns.is_empty() {
        if stmt.columns.len() != columns.len() {
            return Err(BqError::query("VIEW column count mismatch"));
        }
        for (col, alias) in columns.iter_mut().zip(&stmt.columns) {
            col.name.clone_from(&alias.value);
        }
    }
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    let now = Utc::now();
    let meta = TableMeta {
        name: tref.table.clone(),
        kind: if stmt.materialized {
            TableKind::MaterializedView
        } else {
            TableKind::View
        },
        columns,
        partition_by: None,
        cluster_by: Vec::new(),
        view_query: Some(stmt.query.clone()),
        created_at: now,
        updated_at: now,
        options,
        description: None,
        friendly_name: None,
        labels: std::collections::BTreeMap::new(),
        view_sql: stmt.view_sql.clone(),
        expiration_ms: None,
        external_uris: Vec::new(),
        external_format: None,
    };
    catalog.put_table(&tref, meta, stmt.or_replace, stmt.if_not_exists)?;
    Ok(QueryResult::empty())
}

fn exec_create_function(
    stmt: &CreateFunctionStmt,
    catalog: &mut Catalog,
    _storage: &StorageEngine,
) -> BqResult<QueryResult> {
    let tref = if stmt.temp {
        let parts = stmt.name.normalized_parts();
        let fn_name = parts
            .last()
            .ok_or_else(|| BqError::invalid_argument("Invalid TEMP FUNCTION name"))?;
        let default_proj = catalog.default_project().to_owned();
        let _ = catalog.create_dataset(&default_proj, DatasetMeta::new("__temp", "US"), true);
        TableRef::new(default_proj, "__temp", fn_name)
    } else {
        catalog.resolve_table_name(&stmt.name)?
    };
    catalog.put_routine(
        &tref,
        RoutineMeta::Function(Box::new(stmt.clone())),
        stmt.or_replace,
        stmt.if_not_exists,
    )?;
    Ok(QueryResult::empty())
}

fn exec_create_procedure(
    stmt: &CreateProcedureStmt,
    catalog: &mut Catalog,
    _storage: &StorageEngine,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.name)?;
    catalog.put_routine(
        &tref,
        RoutineMeta::Procedure(Box::new(stmt.clone())),
        stmt.or_replace,
        stmt.if_not_exists,
    )?;
    Ok(QueryResult::empty())
}

fn exec_create_policy(
    stmt: &CreateRowAccessPolicyStmt,
    catalog: &mut Catalog,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table_name)?;
    let policy = RowAccessPolicyMeta {
        name: stmt.policy_name.value.clone(),
        grantees: stmt.grantees.clone(),
        filter_expr: stmt.filter_expr.clone(),
    };
    catalog.put_policy(&tref, policy, stmt.or_replace, stmt.if_not_exists)?;
    Ok(QueryResult::empty())
}

fn exec_export_data(
    stmt: &ExportDataStmt,
    catalog: &Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    let uri = match options.get("uri") {
        Some(BqValue::String(s)) => s.clone(),
        _ => return Err(BqError::invalid_argument("EXPORT DATA requires uri option")),
    };
    let format = match options.get("format") {
        Some(BqValue::String(s)) => s.to_ascii_uppercase(),
        _ => {
            return Err(BqError::invalid_argument(
                "EXPORT DATA requires format option",
            ))
        }
    };
    let ctx = ExecContext::with_bindings(catalog, storage, bindings);
    let qr = execute_query(&stmt.query, &ctx)?;
    let cols: Vec<ColumnSchema> = qr
        .columns
        .iter()
        .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
        .collect();
    let payload = match format.as_str() {
        "JSON" | "NEWLINE_DELIMITED_JSON" => crate::io::csv_json::export_ndjson(&cols, &qr.rows)?,
        "CSV" => crate::io::csv_json::export_csv(&cols, &qr.rows)?,
        "PARQUET" => crate::io::parquet_codec::export_parquet(&qr)?,
        other => {
            return Err(BqError::unimplemented(format!(
                "EXPORT DATA format '{other}' is not implemented"
            )))
        }
    };
    let expanded = uri.replace('*', "000000000000");
    let dest = expanded.strip_prefix("gs://").map_or_else(
        || std::path::PathBuf::from(expanded.strip_prefix("file://").unwrap_or(&expanded)),
        |rel| {
            let base = std::env::var("BQ_LITE_GCS_DIR").map_or_else(
                |_| std::env::temp_dir().join("bq-lite-gcs"),
                std::path::PathBuf::from,
            );
            base.join(rel)
        },
    );
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BqError::internal(format!("Failed to create export directory: {e}"))
            })?;
        }
    }
    std::fs::write(&dest, payload)
        .map_err(|e| BqError::internal(format!("Failed to write EXPORT DATA file: {e}")))?;
    Ok(QueryResult::empty())
}

fn exec_drop(
    stmt: &DropStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
) -> BqResult<QueryResult> {
    match stmt.kind {
        DropObjectKind::Schema => {
            let (proj, ds) = catalog.resolve_dataset_name(&stmt.name)?;
            catalog.drop_dataset(&proj, &ds, stmt.if_exists, stmt.cascade)?;
            storage.drop_dataset(&proj, &ds);
        }
        DropObjectKind::Table
        | DropObjectKind::ExternalTable
        | DropObjectKind::SnapshotTable
        | DropObjectKind::View
        | DropObjectKind::MaterializedView => {
            let tref = catalog.resolve_table_name(&stmt.name)?;
            catalog.drop_table(&tref, stmt.if_exists)?;
            storage.drop_table(&tref);
        }
        DropObjectKind::Function | DropObjectKind::TableFunction | DropObjectKind::Procedure => {
            let tref = catalog.resolve_table_name(&stmt.name)?;
            let ds = catalog.get_dataset_mut(&tref.project, &tref.dataset);
            if let Ok(ds_meta) = ds {
                if ds_meta.routines.remove(&tref.table).is_none() && !stmt.if_exists {
                    return Err(BqError::not_found(format!(
                        "Not found: Routine {}:{}.{}",
                        tref.project, tref.dataset, tref.table
                    )));
                }
            } else if !stmt.if_exists {
                return Err(BqError::not_found("Dataset not found for DROP routine"));
            }
        }
        DropObjectKind::RowAccessPolicy => {
            let tbl_name = stmt
                .on_table
                .as_ref()
                .ok_or_else(|| BqError::invalid_argument("Missing ON table in DROP POLICY"))?;
            let tref = catalog.resolve_table_name(tbl_name)?;
            let pol_name = stmt.name.normalized_parts().join(".");
            let ds_meta = catalog.get_dataset_mut(&tref.project, &tref.dataset)?;
            let list = ds_meta.policies.entry(tref.table).or_default();
            let before = list.len();
            list.retain(|p| p.name != pol_name);
            if list.len() == before && !stmt.if_exists {
                return Err(BqError::not_found(format!("Policy '{pol_name}' not found")));
            }
        }
    }
    Ok(QueryResult::empty())
}
