use crate::catalog::{Catalog, JobInfoProvider, TableRef};
use crate::executor::context::EvalRow;
use crate::storage::StorageEngine;
use crate::types::{BqType, BqValue};

use super::binding;

pub(super) fn build_schemata_options(project: &str, qual: &str, catalog: &Catalog) -> Vec<EvalRow> {
    let Some(proj) = catalog.get_project(project) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ds in proj.datasets.values() {
        if ds.name == "__temp" {
            continue;
        }
        for (k, v) in &ds.options {
            out.push(EvalRow::new(vec![
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
                    "option_name",
                    BqType::String,
                    BqValue::String(k.clone()),
                ),
                binding(
                    qual,
                    "option_value",
                    BqType::String,
                    BqValue::String(v.to_string()),
                ),
            ]));
        }
    }
    out
}

pub(super) fn build_table_options(
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
            for (k, v) in &tbl.options {
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
                        "option_name",
                        BqType::String,
                        BqValue::String(k.clone()),
                    ),
                    binding(
                        qual,
                        "option_value",
                        BqType::String,
                        BqValue::String(v.to_string()),
                    ),
                ]));
            }
        }
    }
    out
}

pub(super) fn build_column_field_paths(
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
            let prefix = (project, ds.name.as_str(), tbl.name.as_str(), qual);
            for col in &tbl.columns {
                collect_field_paths(prefix, &col.name, &col.ty, &mut out);
            }
        }
    }
    out
}

fn collect_field_paths(
    (project, ds, tbl, qual): (&str, &str, &str, &str),
    path: &str,
    ty: &BqType,
    out: &mut Vec<EvalRow>,
) {
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
            BqValue::String(ds.to_owned()),
        ),
        binding(
            qual,
            "table_name",
            BqType::String,
            BqValue::String(tbl.to_owned()),
        ),
        binding(
            qual,
            "column_name",
            BqType::String,
            BqValue::String(path.split('.').next().unwrap_or(path).to_owned()),
        ),
        binding(
            qual,
            "field_path",
            BqType::String,
            BqValue::String(path.to_owned()),
        ),
        binding(
            qual,
            "data_type",
            BqType::String,
            BqValue::String(ty.to_string()),
        ),
    ]));
    let inner = match ty {
        BqType::Array(el) => el.as_ref(),
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Bytes
        | BqType::Date
        | BqType::Datetime
        | BqType::Time
        | BqType::Timestamp
        | BqType::Interval
        | BqType::Json
        | BqType::Geography
        | BqType::Range(_)
        | BqType::Struct(_) => ty,
    };
    if let BqType::Struct(fields) = inner {
        for (i, sf) in fields.iter().enumerate() {
            let fname = sf.name.clone().unwrap_or_else(|| format!("f{i}_"));
            collect_field_paths(
                (project, ds, tbl, qual),
                &format!("{path}.{fname}"),
                &sf.ty,
                out,
            );
        }
    }
}

pub(super) fn build_routine_options(
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
            let opts = match rmeta.as_ref() {
                crate::catalog::RoutineMeta::Function(cf) => &cf.options,
                crate::catalog::RoutineMeta::Procedure(cp) => &cp.options,
            };
            for opt in opts {
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
                        "specific_name",
                        BqType::String,
                        BqValue::String(rname.clone()),
                    ),
                    binding(
                        qual,
                        "option_name",
                        BqType::String,
                        BqValue::String(opt.name.value.clone()),
                    ),
                ]));
            }
        }
    }
    out
}

pub(super) fn build_parameters(
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
            let params = match rmeta.as_ref() {
                crate::catalog::RoutineMeta::Function(cf) => &cf.params,
                crate::catalog::RoutineMeta::Procedure(cp) => &cp.params,
            };
            for (idx, p) in params.iter().enumerate() {
                let ord = i64::try_from(idx + 1).unwrap_or(i64::MAX);
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
                        "specific_name",
                        BqType::String,
                        BqValue::String(rname.clone()),
                    ),
                    binding(qual, "ordinal_position", BqType::Int64, BqValue::Int64(ord)),
                    binding(
                        qual,
                        "parameter_name",
                        BqType::String,
                        BqValue::String(p.name.value.clone()),
                    ),
                ]));
            }
        }
    }
    out
}

pub(super) fn build_partitions(
    project: &str,
    scope_ds: Option<&str>,
    qual: &str,
    catalog: &Catalog,
    storage: &StorageEngine,
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
            let tref = TableRef::new(project, &ds.name, &tbl.name);
            let rows_len = i64::try_from(storage.read_rows(&tref).len()).unwrap_or(i64::MAX);
            let logical_bytes = rows_len.saturating_mul(64);
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
                    "partition_id",
                    BqType::String,
                    BqValue::String("__NULL__".to_owned()),
                ),
                binding(qual, "total_rows", BqType::Int64, BqValue::Int64(rows_len)),
                binding(
                    qual,
                    "total_logical_bytes",
                    BqType::Int64,
                    BqValue::Int64(logical_bytes),
                ),
            ]));
        }
    }
    out
}

pub(super) fn build_row_access_policies(
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
        for (tbl_name, pols) in &ds.policies {
            for pol in pols {
                let grantees = pol
                    .grantees
                    .iter()
                    .map(|g| BqValue::String(g.clone()))
                    .collect();
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
                        BqValue::String(tbl_name.clone()),
                    ),
                    binding(
                        qual,
                        "policy_name",
                        BqType::String,
                        BqValue::String(pol.name.clone()),
                    ),
                    binding(
                        qual,
                        "grantee_list",
                        BqType::array(BqType::String).unwrap_or(BqType::String),
                        BqValue::Array(grantees),
                    ),
                ]));
            }
        }
    }
    out
}

pub(super) fn build_jobs(
    project: &str,
    qual: &str,
    provider: Option<&dyn JobInfoProvider>,
) -> Vec<EvalRow> {
    let Some(prov) = provider else {
        return Vec::new();
    };
    prov.list_jobs(project)
        .into_iter()
        .map(|j| {
            EvalRow::new(vec![
                binding(
                    qual,
                    "project_id",
                    BqType::String,
                    BqValue::String(j.project_id),
                ),
                binding(qual, "job_id", BqType::String, BqValue::String(j.job_id)),
                binding(
                    qual,
                    "job_type",
                    BqType::String,
                    BqValue::String(j.job_type),
                ),
                binding(qual, "state", BqType::String, BqValue::String(j.state)),
                binding(
                    qual,
                    "query",
                    BqType::String,
                    j.query.map_or(BqValue::Null, BqValue::String),
                ),
                binding(
                    qual,
                    "creation_time",
                    BqType::Timestamp,
                    BqValue::Timestamp(j.creation_time),
                ),
                binding(
                    qual,
                    "total_bytes_processed",
                    BqType::Int64,
                    BqValue::Int64(j.total_bytes_processed),
                ),
                binding(
                    qual,
                    "error_result",
                    BqType::String,
                    j.error_message.map_or(BqValue::Null, BqValue::String),
                ),
            ])
        })
        .collect()
}
