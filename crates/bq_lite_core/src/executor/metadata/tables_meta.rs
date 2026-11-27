use crate::ast::ObjectName;
use crate::catalog::{Catalog, TableKind, TableMeta, TableRef};
use crate::error::{BqError, BqResult};
use crate::executor::context::EvalRow;
use crate::storage::StorageEngine;
use crate::types::{BqType, BqValue};

use super::binding;

struct TablesMetaTarget {
    project: String,
    dataset: String,
    is_summary: bool,
    table_name: String,
}

fn resolve_tables_meta_target(
    name: &ObjectName,
    catalog: &Catalog,
) -> BqResult<Option<TablesMetaTarget>> {
    let parts = name.normalized_parts();
    let Some(last) = parts.last() else {
        return Ok(None);
    };
    let is_summary = match last.as_str() {
        "__TABLES__" => false,
        "__TABLES_SUMMARY__" => true,
        _ => return Ok(None),
    };
    let (project, dataset) = match parts.as_slice() {
        [_] => {
            let ds = catalog
                .default_dataset()
                .ok_or_else(|| BqError::not_found(format!("No default dataset set for {last}")))?;
            (catalog.default_project().to_owned(), ds.to_owned())
        }
        [ds, _] => (catalog.default_project().to_owned(), ds.clone()),
        [proj, ds, _] => (proj.clone(), ds.clone()),
        _ => {
            return Err(BqError::invalid_argument(format!(
                "Invalid {last} reference"
            )))
        }
    };
    Ok(Some(TablesMetaTarget {
        project,
        dataset,
        is_summary,
        table_name: last.clone(),
    }))
}

pub(in crate::executor) fn try_infer_tables_meta_schema(
    name: &ObjectName,
    alias: Option<&str>,
    catalog: &Catalog,
) -> BqResult<Option<EvalRow>> {
    let Some(target) = resolve_tables_meta_target(name, catalog)? else {
        return Ok(None);
    };
    let _ = catalog.get_dataset(&target.project, &target.dataset)?;
    let qual = alias.unwrap_or(target.table_name.as_str());
    Ok(Some(tables_meta_schema_row(qual, target.is_summary)))
}

fn tables_meta_schema_row(qual: &str, is_summary: bool) -> EvalRow {
    let mut cols = vec![
        binding(qual, "project_id", BqType::String, BqValue::Null),
        binding(qual, "dataset_id", BqType::String, BqValue::Null),
        binding(qual, "table_id", BqType::String, BqValue::Null),
        binding(qual, "creation_time", BqType::Int64, BqValue::Null),
    ];
    if !is_summary {
        cols.push(binding(
            qual,
            "last_modified_time",
            BqType::Int64,
            BqValue::Null,
        ));
        cols.push(binding(qual, "row_count", BqType::Int64, BqValue::Null));
        cols.push(binding(qual, "size_bytes", BqType::Int64, BqValue::Null));
    }
    cols.push(binding(qual, "type", BqType::Int64, BqValue::Null));
    EvalRow::new(cols)
}

pub(super) fn try_eval_tables_meta(
    name: &ObjectName,
    alias: Option<&str>,
    catalog: &Catalog,
    storage: &StorageEngine,
) -> BqResult<Option<Vec<EvalRow>>> {
    let Some(target) = resolve_tables_meta_target(name, catalog)? else {
        return Ok(None);
    };
    let ds_meta = catalog.get_dataset(&target.project, &target.dataset)?;
    let qual = alias.unwrap_or(target.table_name.as_str());
    let rows = ds_meta
        .tables
        .values()
        .map(|tbl| {
            build_tables_meta_row(
                qual,
                &target.project,
                &ds_meta.name,
                tbl,
                target.is_summary,
                storage,
            )
        })
        .collect();
    Ok(Some(rows))
}

fn build_tables_meta_row(
    qual: &str,
    project: &str,
    dataset: &str,
    tbl: &TableMeta,
    is_summary: bool,
    storage: &StorageEngine,
) -> EvalRow {
    let kind_code: i64 = match tbl.kind {
        TableKind::Table | TableKind::Snapshot => 1,
        TableKind::View | TableKind::MaterializedView => 2,
        TableKind::External => 3,
    };
    let mut cols = vec![
        binding(
            qual,
            "project_id",
            BqType::String,
            BqValue::String(project.to_owned()),
        ),
        binding(
            qual,
            "dataset_id",
            BqType::String,
            BqValue::String(dataset.to_owned()),
        ),
        binding(
            qual,
            "table_id",
            BqType::String,
            BqValue::String(tbl.name.clone()),
        ),
        binding(
            qual,
            "creation_time",
            BqType::Int64,
            BqValue::Int64(tbl.created_at.timestamp_millis()),
        ),
    ];
    if !is_summary {
        let tref = TableRef::new(project, dataset, &tbl.name);
        let row_count = i64::try_from(storage.read_rows(&tref).len()).unwrap_or(i64::MAX);
        let size_bytes = row_count.saturating_mul(64);
        cols.push(binding(
            qual,
            "last_modified_time",
            BqType::Int64,
            BqValue::Int64(tbl.updated_at.timestamp_millis()),
        ));
        cols.push(binding(
            qual,
            "row_count",
            BqType::Int64,
            BqValue::Int64(row_count),
        ));
        cols.push(binding(
            qual,
            "size_bytes",
            BqType::Int64,
            BqValue::Int64(size_bytes),
        ));
    }
    cols.push(binding(
        qual,
        "type",
        BqType::Int64,
        BqValue::Int64(kind_code),
    ));
    EvalRow::new(cols)
}
