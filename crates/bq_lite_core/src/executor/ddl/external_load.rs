use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;

use crate::ast::{AlterSchemaStmt, AlterViewStmt, CreateTableStmt, LoadDataStmt};
use crate::catalog::{Catalog, ColumnSchema, TableKind, TableMeta, TableRef};
use crate::error::{BqError, BqResult};
use crate::executor::context::{ExecContext, ScriptBindings};
use crate::executor::query::execute_query;
use crate::executor::result::QueryResult;
use crate::io::autodetect::{autodetect_csv_schema, autodetect_ndjson_schema};
use crate::io::csv_json::{load_csv_with_options, load_ndjson, CsvLoadOptions};
use crate::io::parquet_codec::load_parquet;
use crate::storage::StorageEngine;
use crate::types::BqValue;

use super::table::{build_columns, eval_options};

#[derive(Clone, Copy)]
enum SupportedFormat {
    Csv,
    Json,
    Parquet,
}

fn parse_supported_format(format: &str, kind_label: &str) -> BqResult<SupportedFormat> {
    let upper = format.to_ascii_uppercase();
    match upper.as_str() {
        "CSV" => Ok(SupportedFormat::Csv),
        "NEWLINE_DELIMITED_JSON" | "JSON" => Ok(SupportedFormat::Json),
        "PARQUET" => Ok(SupportedFormat::Parquet),
        _ => Err(BqError::unimplemented(format!(
            "{kind_label} with format '{upper}' are not supported locally"
        ))),
    }
}

pub(super) fn exec_create_external_table(
    stmt: &CreateTableStmt,
    tref: &TableRef,
    catalog: &mut Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let mut columns = build_columns(&stmt.columns)?;
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    let format = extract_opt_string(&options, "format").unwrap_or_else(|| "CSV".to_owned());
    let _ = parse_supported_format(&format, "External tables")?;
    let uris = extract_opt_uris(&options)?;
    if uris.is_empty() {
        return Err(BqError::invalid_argument(
            "CREATE EXTERNAL TABLE requires uris option",
        ));
    }
    for uri in &uris {
        let _ = resolve_uri_local(uri)?;
    }
    if columns.is_empty() {
        let (detected, _) =
            load_files_with_schema(&uris, &format, &columns, &options, "External tables")?;
        columns = detected;
    }
    let now = Utc::now();
    let meta = TableMeta {
        name: tref.table.clone(),
        kind: TableKind::External,
        columns,
        partition_by: None,
        cluster_by: Vec::new(),
        view_query: None,
        created_at: now,
        updated_at: now,
        options,
        description: None,
        friendly_name: None,
        labels: BTreeMap::new(),
        view_sql: None,
        expiration_ms: None,
        external_uris: uris,
        external_format: Some(format),
    };
    catalog.put_table(tref, meta, stmt.or_replace, stmt.if_not_exists)?;
    Ok(QueryResult::empty())
}

type LoadedExternalData = (Vec<ColumnSchema>, Vec<Vec<BqValue>>);

pub(in crate::executor) fn load_external_rows(meta: &TableMeta) -> BqResult<LoadedExternalData> {
    let format = meta.external_format.as_deref().unwrap_or("CSV");
    load_files_with_schema(
        &meta.external_uris,
        format,
        &meta.columns,
        &meta.options,
        "External tables",
    )
}

pub(super) fn exec_load_data(
    stmt: &LoadDataStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.table)?;
    let options = eval_options(&stmt.options, catalog, storage, bindings)?;
    let format = extract_opt_string(&options, "format").unwrap_or_else(|| "CSV".to_owned());
    let uris = extract_opt_uris(&options)?;
    if uris.is_empty() {
        return Err(BqError::invalid_argument("LOAD DATA requires uris option"));
    }

    let existing_cols = catalog
        .get_table(&tref)
        .ok()
        .map(|m| m.columns.clone())
        .unwrap_or_default();
    let declared_cols = if stmt.columns.is_empty() {
        existing_cols
    } else {
        build_columns(&stmt.columns)?
    };

    let (cols, rows) =
        load_files_with_schema(&uris, &format, &declared_cols, &options, "LOAD DATA jobs")?;
    if catalog.get_table(&tref).is_err() {
        let now = Utc::now();
        let meta = TableMeta {
            name: tref.table.clone(),
            kind: TableKind::Table,
            columns: cols,
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
        };
        catalog.put_table(&tref, meta, false, false)?;
    }
    let count = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    if stmt.overwrite {
        storage.replace_rows(&tref, rows);
    } else {
        storage.insert_rows(&tref, rows)?;
    }
    Ok(QueryResult::affected(count))
}

fn load_files_with_schema(
    uris: &[String],
    format: &str,
    existing_cols: &[ColumnSchema],
    options: &BTreeMap<String, BqValue>,
    kind_label: &str,
) -> BqResult<LoadedExternalData> {
    let mut cols = existing_cols.to_vec();
    let mut all_rows = Vec::new();
    let skip_opt = match options.get("skip_leading_rows") {
        Some(BqValue::Int64(n)) => usize::try_from((*n).max(0)).unwrap_or(0),
        _ => 0,
    };
    let delim = extract_opt_string(options, "field_delimiter")
        .and_then(|s| s.as_bytes().first().copied())
        .unwrap_or(b',');

    let parsed_fmt = parse_supported_format(format, kind_label)?;

    for uri in uris {
        let path = resolve_uri_local(uri)?;
        let bytes = std::fs::read(&path)
            .map_err(|e| BqError::not_found(format!("Failed to read '{uri}': {e}")))?;
        match parsed_fmt {
            SupportedFormat::Csv => {
                let mut opts = CsvLoadOptions {
                    skip_leading_rows: skip_opt,
                    field_delimiter: delim,
                    ..CsvLoadOptions::default()
                };
                if cols.is_empty() {
                    let (detected, skip) = autodetect_csv_schema(&bytes, &opts)?;
                    cols = detected;
                    opts.skip_leading_rows = skip;
                }
                all_rows.extend(load_csv_with_options(&bytes, &cols, &opts)?);
            }
            SupportedFormat::Json => {
                if cols.is_empty() {
                    cols = autodetect_ndjson_schema(&bytes)?;
                }
                all_rows.extend(load_ndjson(&bytes, &cols)?);
            }
            SupportedFormat::Parquet => {
                let (pq_cols, pq_rows) = load_parquet(&bytes)?;
                if cols.is_empty() {
                    cols = pq_cols;
                }
                all_rows.extend(pq_rows);
            }
        }
    }
    Ok((cols, all_rows))
}

fn resolve_uri_local(uri: &str) -> BqResult<PathBuf> {
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Err(BqError::unimplemented(format!(
            "Remote HTTP/HTTPS external URI '{uri}' is not supported locally"
        )));
    }
    let expanded = uri.replace('*', "000000000000");
    if let Some(rel) = expanded.strip_prefix("gs://") {
        if rel.contains("..") || rel.is_empty() {
            return Err(BqError::invalid_argument(format!("Invalid URI: '{uri}'")));
        }
        let base = std::env::var("BQ_LITE_GCS_DIR")
            .map_or_else(|_| std::env::temp_dir().join("bq-lite-gcs"), PathBuf::from);
        return Ok(base.join(rel));
    }
    Ok(PathBuf::from(
        expanded.strip_prefix("file://").unwrap_or(&expanded),
    ))
}

fn extract_opt_string(options: &BTreeMap<String, BqValue>, key: &str) -> Option<String> {
    match options.get(key) {
        Some(BqValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "Only STRING entries are valid inside uris option"
)]
fn extract_opt_uris(options: &BTreeMap<String, BqValue>) -> BqResult<Vec<String>> {
    match options.get("uris") {
        Some(BqValue::Array(items)) => items
            .iter()
            .map(|v| match v {
                BqValue::String(s) => Ok(s.clone()),
                _ => Err(BqError::invalid_argument("uris entries must be STRING")),
            })
            .collect(),
        Some(BqValue::String(s)) => Ok(vec![s.clone()]),
        _ => Ok(Vec::new()),
    }
}

pub(super) fn exec_alter_schema(
    stmt: &AlterSchemaStmt,
    catalog: &mut Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let (proj, ds_name) = catalog.resolve_dataset_name(&stmt.name)?;
    let evaluated = eval_options(&stmt.options, catalog, storage, bindings)?;
    let Ok(ds) = catalog.get_dataset_mut(&proj, &ds_name) else {
        if stmt.if_exists {
            return Ok(QueryResult::empty());
        }
        return Err(BqError::not_found(format!(
            "Not found: Dataset {proj}:{ds_name}"
        )));
    };
    if let Some(BqValue::String(loc)) = evaluated.get("location") {
        ds.location.clone_from(loc);
    }
    if let Some(BqValue::Bool(ci)) = evaluated.get("is_case_insensitive") {
        ds.is_case_insensitive = *ci;
    }
    if let Some(BqValue::String(desc)) = evaluated.get("description") {
        ds.description = Some(desc.clone());
    }
    if let Some(BqValue::String(fname)) = evaluated.get("friendly_name") {
        ds.friendly_name = Some(fname.clone());
    }
    ds.options.extend(evaluated);
    Ok(QueryResult::empty())
}

pub(super) fn exec_alter_view(
    stmt: &AlterViewStmt,
    catalog: &mut Catalog,
    storage: &StorageEngine,
    bindings: ScriptBindings<'_>,
) -> BqResult<QueryResult> {
    let tref = catalog.resolve_table_name(&stmt.name)?;
    let Ok(existing) = catalog.get_table(&tref) else {
        if stmt.if_exists {
            return Ok(QueryResult::empty());
        }
        return Err(BqError::not_found(format!(
            "Not found: View {}:{}.{}",
            tref.project, tref.dataset, tref.table
        )));
    };
    let mut updated = existing.clone();
    if !stmt.options.is_empty() {
        let evaluated = eval_options(&stmt.options, catalog, storage, bindings)?;
        if let Some(BqValue::String(desc)) = evaluated.get("description") {
            updated.description = Some(desc.clone());
        }
        updated.options.extend(evaluated);
    }
    if let Some(ref q) = stmt.query {
        let ctx = ExecContext::with_bindings(catalog, storage, bindings);
        let qr = execute_query(q, &ctx)?;
        updated.columns = qr
            .columns
            .iter()
            .map(|c| ColumnSchema::new(&c.name, c.ty.clone()))
            .collect();
        updated.view_query = Some(q.clone());
        updated.view_sql.clone_from(&stmt.view_sql);
    }
    updated.updated_at = Utc::now();
    catalog.put_table(&tref, updated, true, false)?;
    Ok(QueryResult::empty())
}
