mod extra;
mod tables_meta;

pub(in crate::executor) use tables_meta::try_infer_tables_meta_schema;

use crate::ast::ObjectName;
use crate::catalog::{Catalog, JobInfoProvider, TableKind};
use crate::error::{BqError, BqResult};
use crate::executor::context::{EvalRow, RowBinding};
use crate::storage::StorageEngine;
use crate::types::{BqType, BqValue};

use extra::{
    build_column_field_paths, build_jobs, build_partitions, build_routine_options,
    build_row_access_policies, build_schemata_options, build_table_options,
};

/// Resolves `INFORMATION_SCHEMA.*` queries (dataset-scoped or region-scoped `region-us.INFORMATION_SCHEMA.*`).
pub(in crate::executor) fn try_eval_information_schema(
    name: &ObjectName,
    alias: Option<&str>,
    catalog: &Catalog,
    storage: &StorageEngine,
    job_provider: Option<&dyn JobInfoProvider>,
) -> BqResult<Option<Vec<EvalRow>>> {
    if let Some(meta_rows) = tables_meta::try_eval_tables_meta(name, alias, catalog, storage)? {
        return Ok(Some(meta_rows));
    }
    let parts = name.normalized_parts();
    let info_idx = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("INFORMATION_SCHEMA"));
    let Some(idx) = info_idx else {
        return Ok(None);
    };
    let view_name = parts
        .get(idx + 1)
        .ok_or_else(|| BqError::query("Missing INFORMATION_SCHEMA view name"))?;
    let prefix = parts.get(..idx).unwrap_or(&[]);
    let (project, scope_dataset) = resolve_scope(prefix, catalog);
    let qual = alias.unwrap_or(view_name.as_str());

    let rows = match view_name.to_ascii_uppercase().as_str() {
        "SCHEMATA" => build_schemata(&project, qual, catalog),
        "SCHEMATA_OPTIONS" => build_schemata_options(&project, qual, catalog),
        "TABLES" => build_tables(&project, scope_dataset.as_deref(), qual, catalog),
        "TABLE_OPTIONS" => build_table_options(&project, scope_dataset.as_deref(), qual, catalog),
        "COLUMNS" => build_columns(&project, scope_dataset.as_deref(), qual, catalog),
        "COLUMN_FIELD_PATHS" => {
            build_column_field_paths(&project, scope_dataset.as_deref(), qual, catalog)
        }
        "VIEWS" => build_views(&project, scope_dataset.as_deref(), qual, catalog),
        "ROUTINES" => build_routines(&project, scope_dataset.as_deref(), qual, catalog),
        "ROUTINE_OPTIONS" => {
            build_routine_options(&project, scope_dataset.as_deref(), qual, catalog)
        }
        "PARAMETERS" => extra::build_parameters(&project, scope_dataset.as_deref(), qual, catalog),
        "PARTITIONS" => {
            build_partitions(&project, scope_dataset.as_deref(), qual, catalog, storage)
        }
        "ROW_ACCESS_POLICIES" => {
            build_row_access_policies(&project, scope_dataset.as_deref(), qual, catalog)
        }
        "JOBS" | "JOBS_BY_PROJECT" => build_jobs(&project, qual, job_provider),
        _ => {
            return Err(BqError::unimplemented(format!(
                "INFORMATION_SCHEMA.{view_name} is not implemented"
            )))
        }
    };
    Ok(Some(rows))
}

fn resolve_scope(prefix: &[String], catalog: &Catalog) -> (String, Option<String>) {
    match prefix {
        [] => (catalog.default_project().to_owned(), None),
        [one] if one.to_ascii_lowercase().starts_with("region-") => {
            (catalog.default_project().to_owned(), None)
        }
        [ds] => (catalog.default_project().to_owned(), Some(ds.clone())),
        [proj, ds] if ds.to_ascii_lowercase().starts_with("region-") => (proj.clone(), None),
        [proj, ds] => (proj.clone(), Some(ds.clone())),
        [proj, ..] => (proj.clone(), None),
    }
}

pub(super) fn binding(qual: &str, name: &str, ty: BqType, value: BqValue) -> RowBinding {
    RowBinding {
        qualifier: Some(qual.to_owned()),
        name: name.to_owned(),
        ty,
        value,
    }
}

fn build_schemata(project: &str, qual: &str, catalog: &Catalog) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    proj.datasets
        .values()
        .filter(|ds| ds.name != "__temp")
        .map(|ds| {
            EvalRow::new(vec![
                binding(
                    qual,
                    "catalog_name",
                    BqType::String,
                    BqValue::String(project.to_owned()),
                ),
                binding(
                    qual,
                    "schema_name",
                    BqType::String,
                    BqValue::String(ds.name.clone()),
                ),
                binding(
                    qual,
                    "location",
                    BqType::String,
                    BqValue::String(ds.location.clone()),
                ),
            ])
        })
        .collect()
}

fn build_tables(
    project: &str,
    scope_ds: Option<&str>,
    qual: &str,
    catalog: &Catalog,
) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ds in proj.datasets.values() {
        if ds.name == "__temp" || scope_ds.is_some_and(|s| s != ds.name) {
            continue;
        }
        for tbl in ds.tables.values() {
            let tbl_type = match tbl.kind {
                TableKind::Table => "BASE TABLE",
                TableKind::View => "VIEW",
                TableKind::MaterializedView => "MATERIALIZED VIEW",
                TableKind::Snapshot => "SNAPSHOT",
                TableKind::External => "EXTERNAL",
            };
            let ddl_str = format_table_ddl(&ds.name, tbl);
            out.push(EvalRow::new(vec![
                binding(
                    qual,
                    "table_catalog",
                    BqType::String,
                    BqValue::String(project.to_owned()),
                ),
                binding(
                    qual,
                    "table_schema",
                    BqType::String,
                    BqValue::String(ds.name.clone()),
                ),
                binding(
                    qual,
                    "table_name",
                    BqType::String,
                    BqValue::String(tbl.name.clone()),
                ),
                binding(
                    qual,
                    "table_type",
                    BqType::String,
                    BqValue::String(tbl_type.to_owned()),
                ),
                binding(
                    qual,
                    "creation_time",
                    BqType::Timestamp,
                    BqValue::Timestamp(tbl.created_at),
                ),
                binding(qual, "ddl", BqType::String, BqValue::String(ddl_str)),
            ]));
        }
    }
    out
}

fn build_columns(
    project: &str,
    scope_ds: Option<&str>,
    qual: &str,
    catalog: &Catalog,
) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ds in proj.datasets.values() {
        if ds.name == "__temp" || scope_ds.is_some_and(|s| s != ds.name) {
            continue;
        }
        for tbl in ds.tables.values() {
            for (pos, col) in tbl.columns.iter().enumerate() {
                out.push(build_column_row(project, &ds.name, tbl, pos, col, qual));
            }
        }
    }
    out
}

fn build_column_row(
    project: &str,
    ds_name: &str,
    tbl: &crate::catalog::TableMeta,
    pos: usize,
    col: &crate::catalog::ColumnSchema,
    qual: &str,
) -> EvalRow {
    let ord = i64::try_from(pos + 1).unwrap_or(i64::MAX);
    let is_null = if col.nullable { "YES" } else { "NO" };
    let is_part = if tbl
        .partition_by
        .as_deref()
        .is_some_and(|p| p.eq_ignore_ascii_case(&col.name))
    {
        "YES"
    } else {
        "NO"
    };
    let cluster_ord = tbl
        .cluster_by
        .iter()
        .position(|c| c.eq_ignore_ascii_case(&col.name))
        .map_or(BqValue::Null, |i| {
            BqValue::Int64(i64::try_from(i + 1).unwrap_or(i64::MAX))
        });
    let col_def = col
        .default_expr
        .as_ref()
        .map_or(BqValue::Null, |e| BqValue::String(format!("{e:?}")));
    EvalRow::new(vec![
        binding(
            qual,
            "table_catalog",
            BqType::String,
            BqValue::String(project.to_owned()),
        ),
        binding(
            qual,
            "table_schema",
            BqType::String,
            BqValue::String(ds_name.to_owned()),
        ),
        binding(
            qual,
            "table_name",
            BqType::String,
            BqValue::String(tbl.name.clone()),
        ),
        binding(
            qual,
            "column_name",
            BqType::String,
            BqValue::String(col.name.clone()),
        ),
        binding(qual, "ordinal_position", BqType::Int64, BqValue::Int64(ord)),
        binding(
            qual,
            "is_nullable",
            BqType::String,
            BqValue::String(is_null.to_owned()),
        ),
        binding(
            qual,
            "data_type",
            BqType::String,
            BqValue::String(col.ty.to_string()),
        ),
        binding(
            qual,
            "is_partitioning_column",
            BqType::String,
            BqValue::String(is_part.to_owned()),
        ),
        binding(
            qual,
            "clustering_ordinal_position",
            BqType::Int64,
            cluster_ord,
        ),
        binding(qual, "column_default", BqType::String, col_def),
    ])
}

fn build_views(
    project: &str,
    scope_ds: Option<&str>,
    qual: &str,
    catalog: &Catalog,
) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ds in proj.datasets.values() {
        if ds.name == "__temp" || scope_ds.is_some_and(|s| s != ds.name) {
            continue;
        }
        for tbl in ds.tables.values() {
            if matches!(tbl.kind, TableKind::View | TableKind::MaterializedView) {
                let vdef = tbl.view_sql.clone().unwrap_or_default();
                out.push(EvalRow::new(vec![
                    binding(
                        qual,
                        "table_catalog",
                        BqType::String,
                        BqValue::String(project.to_owned()),
                    ),
                    binding(
                        qual,
                        "table_schema",
                        BqType::String,
                        BqValue::String(ds.name.clone()),
                    ),
                    binding(
                        qual,
                        "table_name",
                        BqType::String,
                        BqValue::String(tbl.name.clone()),
                    ),
                    binding(
                        qual,
                        "view_definition",
                        BqType::String,
                        BqValue::String(vdef),
                    ),
                ]));
            }
        }
    }
    out
}

fn build_routines(
    project: &str,
    scope_ds: Option<&str>,
    qual: &str,
    catalog: &Catalog,
) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ds in proj.datasets.values() {
        if ds.name == "__temp" || scope_ds.is_some_and(|s| s != ds.name) {
            continue;
        }
        for (rname, rmeta) in &ds.routines {
            let rtype = match rmeta.as_ref() {
                crate::catalog::RoutineMeta::Function(cf) => match cf.kind {
                    crate::ast::FunctionKind::Scalar => "FUNCTION",
                    crate::ast::FunctionKind::Table => "TABLE FUNCTION",
                },
                crate::catalog::RoutineMeta::Procedure(_) => "PROCEDURE",
            };
            out.push(EvalRow::new(vec![
                binding(
                    qual,
                    "specific_catalog",
                    BqType::String,
                    BqValue::String(project.to_owned()),
                ),
                binding(
                    qual,
                    "specific_schema",
                    BqType::String,
                    BqValue::String(ds.name.clone()),
                ),
                binding(
                    qual,
                    "routine_name",
                    BqType::String,
                    BqValue::String(rname.clone()),
                ),
                binding(
                    qual,
                    "routine_type",
                    BqType::String,
                    BqValue::String(rtype.to_owned()),
                ),
            ]));
        }
    }
    out
}

fn format_table_ddl(ds: &str, tbl: &crate::catalog::TableMeta) -> String {
    if matches!(tbl.kind, TableKind::View | TableKind::MaterializedView) {
        let kw = if tbl.kind == TableKind::MaterializedView {
            "MATERIALIZED VIEW"
        } else {
            "VIEW"
        };
        let body = tbl.view_sql.as_deref().unwrap_or("SELECT 1");
        return format!("CREATE {kw} `{ds}`.`{}` AS {body};", tbl.name);
    }
    let cols: Vec<String> = tbl
        .columns
        .iter()
        .map(|c| {
            let nn = if c.nullable { "" } else { " NOT NULL" };
            format!("{} {}{nn}", c.name, c.ty)
        })
        .collect();
    format!("CREATE TABLE `{ds}`.`{}` ({});", tbl.name, cols.join(", "))
}
