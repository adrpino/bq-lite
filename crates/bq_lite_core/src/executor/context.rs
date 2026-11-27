use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::catalog::{Catalog, JobInfoProvider};
use crate::error::{BqError, BqResult};
use crate::storage::StorageEngine;
use crate::types::{BqStructFieldValue, BqType, BqValue};

use super::result::QueryResult;

/// A single column binding in the current evaluation row.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowBinding {
    pub qualifier: Option<Arc<str>>,
    pub name: Arc<str>,
    pub ty: BqType,
    pub value: BqValue,
}

/// Evaluation row holding visible column bindings plus optional pseudo-columns (`_PARTITIONTIME`).
#[derive(Debug, Clone, Default)]
pub(super) struct EvalRow {
    pub cols: Vec<RowBinding>,
    pub pseudo_cols: Vec<RowBinding>,
}

impl EvalRow {
    #[must_use]
    pub(super) const fn new(cols: Vec<RowBinding>) -> Self {
        Self {
            cols,
            pseudo_cols: Vec::new(),
        }
    }

    #[must_use]
    pub(super) const fn with_pseudo(cols: Vec<RowBinding>, pseudo_cols: Vec<RowBinding>) -> Self {
        Self { cols, pseudo_cols }
    }

    #[must_use]
    pub(super) fn from_query_result(alias: Option<&str>, qr: &QueryResult) -> Vec<Self> {
        let qual: Option<Arc<str>> = alias.map(Arc::from);
        let col_names: Vec<Arc<str>> = qr
            .columns
            .iter()
            .map(|c| Arc::from(c.name.as_str()))
            .collect();
        qr.rows
            .iter()
            .map(|row| {
                let cols = qr
                    .columns
                    .iter()
                    .zip(col_names.iter())
                    .zip(row.iter())
                    .map(|((col, cname), val)| RowBinding {
                        qualifier: qual.clone(),
                        name: cname.clone(),
                        ty: col.ty.clone(),
                        value: val.clone(),
                    })
                    .collect();
                Self::new(cols)
            })
            .collect()
    }

    pub(super) fn resolve_ident(&self, name: &str) -> BqResult<Option<&RowBinding>> {
        let mut found: Option<&RowBinding> = None;
        for col in &self.cols {
            if col.name.eq_ignore_ascii_case(name) {
                if found.is_some() {
                    return Err(BqError::query(format!("Column name {name} is ambiguous")));
                }
                found = Some(col);
            }
        }
        if found.is_some() {
            return Ok(found);
        }
        for col in &self.pseudo_cols {
            if col.name.eq_ignore_ascii_case(name) {
                return Ok(Some(col));
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_qualified(&self, qualifier: &str, name: &str) -> Option<&RowBinding> {
        self.cols.iter().chain(self.pseudo_cols.iter()).find(|c| {
            c.qualifier
                .as_deref()
                .is_some_and(|q| q.eq_ignore_ascii_case(qualifier))
                && c.name.eq_ignore_ascii_case(name)
        })
    }

    #[must_use]
    pub(super) fn to_struct_value(&self) -> BqValue {
        BqValue::Struct(
            self.cols
                .iter()
                .map(|c| BqStructFieldValue {
                    name: Some(c.name.as_ref().to_owned()),
                    value: c.value.clone(),
                })
                .collect(),
        )
    }
}

/// Borrowed script variables and query parameters passed into DDL/DML execution.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::executor) struct ScriptBindings<'a> {
    pub vars: Option<&'a BTreeMap<String, BqValue>>,
    pub params: Option<&'a BTreeMap<String, BqValue>>,
    pub positional: &'a [BqValue],
}

/// Session governance and metadata provider configuration shared between [`crate::Engine`] and [`ExecContext`].
#[derive(Debug, Clone)]
pub(in crate::executor) struct GovernanceConfig {
    pub session_user: String,
    pub session_roles: Vec<String>,
    pub enforce_column_masking: bool,
    pub masking_policies: BTreeMap<String, Option<BqValue>>,
    pub job_provider: Option<Arc<dyn JobInfoProvider>>,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self {
            session_user: "local@bq-lite.invalid".to_owned(),
            session_roles: Vec::new(),
            enforce_column_masking: false,
            masking_policies: BTreeMap::new(),
            job_provider: None,
        }
    }
}

/// Execution context passed down through queries and expressions.
#[derive(Debug)]
pub(super) struct ExecContext<'a> {
    pub catalog: &'a Catalog,
    pub storage: &'a StorageEngine,
    pub ctes: BTreeMap<String, Arc<QueryResult>>,
    pub params: BTreeMap<String, BqValue>,
    pub positional_params: Vec<BqValue>,
    pub vars: BTreeMap<String, BqValue>,
    pub sys_vars: BTreeMap<String, BqValue>,
    pub outer_rows: Vec<&'a EvalRow>,
    pub now: DateTime<Utc>,
    pub governance: GovernanceConfig,
}

impl<'a> ExecContext<'a> {
    #[must_use]
    pub(super) fn new(catalog: &'a Catalog, storage: &'a StorageEngine) -> Self {
        Self {
            catalog,
            storage,
            ctes: BTreeMap::new(),
            params: BTreeMap::new(),
            positional_params: Vec::new(),
            vars: BTreeMap::new(),
            sys_vars: BTreeMap::new(),
            outer_rows: Vec::new(),
            now: Utc::now(),
            governance: GovernanceConfig::default(),
        }
    }

    #[must_use]
    pub(super) fn with_bindings(
        catalog: &'a Catalog,
        storage: &'a StorageEngine,
        bindings: ScriptBindings<'_>,
    ) -> Self {
        let mut ctx = Self::new(catalog, storage);
        if let Some(v) = bindings.vars {
            ctx.vars.clone_from(v);
        }
        if let Some(p) = bindings.params {
            ctx.params.clone_from(p);
        }
        ctx.positional_params = bindings.positional.to_vec();
        ctx
    }

    #[must_use]
    pub(super) fn resolve_param(&self, name: &str) -> Option<&BqValue> {
        self.params.get(name).or_else(|| {
            self.params
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    #[must_use]
    pub(super) fn with_outer<'b>(&'b self, outer: &'b EvalRow) -> ExecContext<'b> {
        let mut outer_rows = self.outer_rows.clone();
        outer_rows.push(outer);
        ExecContext {
            catalog: self.catalog,
            storage: self.storage,
            ctes: self.ctes.clone(),
            params: self.params.clone(),
            positional_params: self.positional_params.clone(),
            vars: self.vars.clone(),
            sys_vars: self.sys_vars.clone(),
            outer_rows,
            now: self.now,
            governance: self.governance.clone(),
        }
    }
}

#[cfg(test)]
mod memory_guard_tests {
    use super::*;
    use crate::catalog::{ColumnSchema, DatasetMeta, TableKind, TableMeta, TableRef};
    use crate::executor::query::{eval_from_clause_public, project_eval_rows_public};
    use crate::executor::result::QueryColumn;
    use crate::parser::parse_query;

    #[test]
    fn hot_row_types_stay_within_size_budgets() {
        assert!(
            size_of::<BqValue>() <= 40,
            "BqValue bloated to {} bytes (budget <= 40)",
            size_of::<BqValue>()
        );
        assert!(
            size_of::<RowBinding>() <= 112,
            "RowBinding bloated to {} bytes (budget <= 112)",
            size_of::<RowBinding>()
        );
    }

    #[test]
    fn row_bindings_share_arc_str_metadata_and_ctes_share_arc() {
        let mut catalog = Catalog::new("local");
        catalog
            .create_dataset("local", DatasetMeta::new("ds", "US"), true)
            .unwrap();
        let tref = TableRef::new("local", "ds", "t");
        let now = Utc::now();
        let meta = TableMeta {
            name: "t".to_owned(),
            kind: TableKind::Table,
            columns: vec![
                ColumnSchema::new("id", BqType::Int64),
                ColumnSchema::new("label", BqType::String),
            ],
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
        catalog.put_table(&tref, meta, true, false).unwrap();
        let mut storage = StorageEngine::new();
        storage.create_table(&tref);
        storage
            .insert_rows(
                &tref,
                vec![
                    vec![BqValue::Int64(1), BqValue::String("a".to_owned())],
                    vec![BqValue::Int64(2), BqValue::String("b".to_owned())],
                ],
            )
            .unwrap();

        let mut ctx = ExecContext::new(&catalog, &storage);
        let qr = QueryResult::new(
            vec![QueryColumn::new("id", BqType::Int64)],
            vec![vec![BqValue::Int64(1)], vec![BqValue::Int64(2)]],
        );

        // 1. EvalRow::from_query_result shares Arc<str> name and qualifier across rows.
        let from_qr = EvalRow::from_query_result(Some("q"), &qr);
        assert!(Arc::ptr_eq(
            &from_qr[0].cols[0].name,
            &from_qr[1].cols[0].name
        ));
        assert!(Arc::ptr_eq(
            from_qr[0].cols[0].qualifier.as_ref().unwrap(),
            from_qr[1].cols[0].qualifier.as_ref().unwrap()
        ));

        // 2. Concrete table scan & projection share Arc<str> across rows.
        let parsed = parse_query("SELECT id, label AS lbl FROM ds.t").unwrap();
        let crate::ast::SetExpr::Select(sel) = &parsed.body else {
            panic!("expected SELECT");
        };
        let scanned = eval_from_clause_public(&sel.from, &ctx).unwrap();
        assert!(Arc::ptr_eq(
            &scanned[0].cols[0].name,
            &scanned[1].cols[0].name
        ));
        assert!(Arc::ptr_eq(
            scanned[0].cols[0].qualifier.as_ref().unwrap(),
            scanned[1].cols[0].qualifier.as_ref().unwrap()
        ));
        let projected = project_eval_rows_public(sel, &scanned, &ctx).unwrap();
        assert!(Arc::ptr_eq(
            &projected[0].cols[1].name,
            &projected[1].cols[1].name
        ));

        // 3. ExecContext::with_outer shares Arc<QueryResult> CTEs without deep-cloning.
        ctx.ctes.insert("big".to_owned(), Arc::new(qr));
        let sub = ctx.with_outer(&scanned[0]);
        assert!(Arc::ptr_eq(
            ctx.ctes.get("big").unwrap(),
            sub.ctes.get("big").unwrap()
        ));
    }
}
