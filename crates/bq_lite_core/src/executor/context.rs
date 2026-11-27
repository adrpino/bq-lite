use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::catalog::{Catalog, JobInfoProvider};
use crate::storage::StorageEngine;
use crate::types::BqValue;

use super::result::QueryResult;

pub(super) use super::row::{ColMeta, EvalRow, RowBinding, RowRef, RowSchema};

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
    use crate::types::BqType;

    #[test]
    fn hot_row_types_stay_within_size_budgets() {
        assert!(
            size_of::<BqValue>() <= 40,
            "BqValue bloated to {} bytes (budget <= 40)",
            size_of::<BqValue>()
        );
        assert!(
            size_of::<EvalRow>() <= 64,
            "EvalRow bloated to {} bytes (budget <= 64)",
            size_of::<EvalRow>()
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
            primary_key: Vec::new(),
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

        // 1. EvalRow::from_query_result shares Arc<RowSchema> across rows.
        let from_qr = EvalRow::from_query_result(Some("q"), &qr);
        assert!(Arc::ptr_eq(&from_qr[0].schema, &from_qr[1].schema));

        // 2. Concrete table scan & projection share Arc<RowSchema> across rows.
        let parsed = parse_query("SELECT id, label AS lbl FROM ds.t").unwrap();
        let crate::ast::SetExpr::Select(sel) = &parsed.body else {
            panic!("expected SELECT");
        };
        let scanned = eval_from_clause_public(&sel.from, &ctx).unwrap();
        assert!(Arc::ptr_eq(&scanned[0].schema, &scanned[1].schema));
        let projected = project_eval_rows_public(sel, &scanned, &ctx).unwrap();
        assert!(Arc::ptr_eq(&projected[0].schema, &projected[1].schema));

        // 3. ExecContext::with_outer shares Arc<QueryResult> CTEs without deep-cloning.
        ctx.ctes.insert("big".to_owned(), Arc::new(qr));
        let sub = ctx.with_outer(&scanned[0]);
        assert!(Arc::ptr_eq(
            ctx.ctes.get("big").unwrap(),
            sub.ctes.get("big").unwrap()
        ));
    }
}
