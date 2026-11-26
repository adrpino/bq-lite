use std::collections::BTreeMap;

use bq_lite_core::catalog::{ColumnSchema, TableKind, TableMeta, TableRef};
use bq_lite_core::parser::parse_script;
use bq_lite_core::{BqError, BqResult, BqValue, Engine, QueryResult};
use chrono::Utc;
use serde_json::Value as JsonValue;

use crate::state::{JobRecord, JobStatus, ServerState};

pub(super) fn run_sql_on_engine_ref(
    eng: &mut Engine,
    sql: &str,
    default_ds: Option<String>,
    named: &BTreeMap<String, BqValue>,
    pos: &[BqValue],
) -> BqResult<QueryResult> {
    let prev_ds = eng.catalog().default_dataset().map(str::to_owned);
    let stmts = parse_script(sql)?;
    if stmts.len() == 1 && matches!(stmts.first(), Some(bq_lite_core::ast::Statement::Query(_))) {
        if default_ds.is_some() {
            eng.catalog_mut().set_default_dataset(default_ds);
        }
        let res = eng.query_with_params(sql, named, pos);
        eng.catalog_mut().set_default_dataset(prev_ds);
        return res;
    }

    // Execute multi-statement scripts and DDL/DML atomically on an isolated fork.
    let mut fork = eng.fork_isolated();
    if default_ds.is_some() {
        fork.catalog_mut().set_default_dataset(default_ds);
    }
    let res = fork.exec_script_with_params(sql, named, pos);
    fork.catalog_mut().set_default_dataset(prev_ds);
    match res {
        Ok(qr) => {
            *eng = fork;
            Ok(qr)
        }
        Err(err) => Err(err),
    }
}

pub(super) fn dry_run_sql(
    state: &ServerState,
    sql: &str,
    default_ds: Option<String>,
    named: &BTreeMap<String, BqValue>,
    pos: &[BqValue],
) -> BqResult<QueryResult> {
    let mut fork = state.engine.read().fork_isolated();
    run_sql_on_engine_ref(&mut fork, sql, default_ds, named, pos)
}

pub(super) fn execute_copy_job(
    state: &ServerState,
    project_id: &str,
    job_id: String,
    location: String,
    copy_cfg: &JsonValue,
) -> BqResult<JobRecord> {
    let dest_ref = parse_table_ref(
        copy_cfg
            .get("destinationTable")
            .ok_or_else(|| BqError::invalid_argument("Copy job missing destinationTable"))?,
        project_id,
    )?;
    let mut sources = Vec::new();
    if let Some(src) = copy_cfg.get("sourceTable") {
        sources.push(parse_table_ref(src, project_id)?);
    }
    if let Some(arr) = copy_cfg.get("sourceTables").and_then(JsonValue::as_array) {
        for item in arr {
            sources.push(parse_table_ref(item, project_id)?);
        }
    }
    if sources.is_empty() {
        return Err(BqError::invalid_argument(
            "Copy job requires sourceTable or sourceTables",
        ));
    }
    let write_disp = copy_cfg
        .get("writeDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("WRITE_EMPTY");
    let create_disp = copy_cfg
        .get("createDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("CREATE_IF_NEEDED");

    let mut eng = state.engine.write();
    let first_ref = sources
        .first()
        .ok_or_else(|| BqError::invalid_argument("Missing source table"))?;
    let first_meta = eng.catalog().get_table(first_ref)?.clone();
    let mut all_rows = Vec::new();
    for sref in &sources {
        let _ = eng.catalog().get_table(sref)?;
        all_rows.extend(eng.storage().read_rows(sref));
    }

    write_rows_with_dispositions(
        &mut eng,
        &dest_ref,
        &first_meta.columns,
        all_rows,
        write_disp,
        create_disp,
    )?;
    drop(eng);
    state.persist_if_enabled();

    Ok(JobRecord {
        job_id,
        project_id: project_id.to_owned(),
        location,
        state: JobStatus::Done,
        created_at: Utc::now(),
        result: Some(QueryResult::empty()),
        error: None,
        total_bytes_processed: 0,
        query: None,
        configuration: Some(serde_json::json!({ "copy": copy_cfg })),
        load_output_rows: None,
    })
}

pub(super) fn apply_query_destination(
    eng: &mut Engine,
    default_project: &str,
    query_cfg: &JsonValue,
    qr: &QueryResult,
) -> BqResult<()> {
    let Some(dest_json) = query_cfg.get("destinationTable") else {
        return Ok(());
    };
    let dest_ref = parse_table_ref(dest_json, default_project)?;
    let write_disp = query_cfg
        .get("writeDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("WRITE_EMPTY");
    let create_disp = query_cfg
        .get("createDisposition")
        .and_then(JsonValue::as_str)
        .unwrap_or("CREATE_IF_NEEDED");
    let cols: Vec<ColumnSchema> = qr
        .columns
        .iter()
        .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
        .collect();
    write_rows_with_dispositions(
        eng,
        &dest_ref,
        &cols,
        qr.rows.clone(),
        write_disp,
        create_disp,
    )
}

pub(super) fn write_rows_with_dispositions(
    eng: &mut Engine,
    dest_ref: &TableRef,
    columns: &[ColumnSchema],
    rows: Vec<Vec<BqValue>>,
    write_disp: &str,
    create_disp: &str,
) -> BqResult<()> {
    let exists = eng.catalog().get_table(dest_ref).is_ok();
    if !exists {
        if create_disp.eq_ignore_ascii_case("CREATE_NEVER") {
            return Err(BqError::not_found(format!(
                "Not found: Table {}:{}.{}",
                dest_ref.project, dest_ref.dataset, dest_ref.table
            )));
        }
        let now = Utc::now();
        eng.create_table(
            dest_ref,
            TableMeta {
                name: dest_ref.table.clone(),
                kind: TableKind::Table,
                columns: columns.to_vec(),
                partition_by: None,
                cluster_by: Vec::new(),
                view_query: None,
                created_at: now,
                updated_at: now,
                options: BTreeMap::new(),
                description: None,
                friendly_name: None,
                labels: BTreeMap::new(),
                view_sql: None,
                expiration_ms: None,
                external_uris: Vec::new(),
                external_format: None,
            },
        )?;
    }

    let canonical = eng.catalog().canonical_table_ref(dest_ref);
    match write_disp.to_ascii_uppercase().as_str() {
        "WRITE_TRUNCATE" => {
            eng.storage_mut().replace_rows(&canonical, rows);
            Ok(())
        }
        "WRITE_EMPTY" => {
            if !eng.storage().read_rows(&canonical).is_empty() {
                return Err(BqError::already_exists(
                    "Destination table already has rows with WRITE_EMPTY",
                ));
            }
            eng.insert_rows(&canonical, rows)
        }
        _ => eng.insert_rows(&canonical, rows),
    }
}

pub(super) fn parse_table_ref(val: &JsonValue, default_proj: &str) -> BqResult<TableRef> {
    let ds = val
        .get("datasetId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Missing tableReference.datasetId"))?;
    let tbl = val
        .get("tableId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BqError::invalid_argument("Missing tableReference.tableId"))?;
    let proj = val
        .get("projectId")
        .and_then(JsonValue::as_str)
        .unwrap_or(default_proj);
    Ok(TableRef::new(proj, ds, tbl))
}
