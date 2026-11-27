use std::collections::BTreeMap;
use std::sync::Arc;

use crate::ast::Statement;
use crate::catalog::{Catalog, DatasetMeta, JobInfoProvider, TableMeta, TableRef};
use crate::error::BqResult;
use crate::parser::{parse_query, parse_script, parse_statement};
use crate::storage::{Row, StorageEngine};
use crate::types::BqValue;

use super::context::{ExecContext, GovernanceConfig};
use super::ddl::execute_ddl;
use super::dml::execute_dml;
use super::query::execute_query;
use super::result::QueryResult;
use super::script::{execute_script_stmts, execute_script_stmts_with_governance};

/// Embedded `BigQuery` (`GoogleSQL`) engine with copy-on-write isolation.
#[derive(Debug, Clone)]
pub struct Engine {
    catalog: Catalog,
    storage: StorageEngine,
    governance: GovernanceConfig,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        Self::with_project("local")
    }

    #[must_use]
    pub fn with_project(default_project: impl Into<String>) -> Self {
        Self {
            catalog: Catalog::new(default_project),
            storage: StorageEngine::new(),
            governance: GovernanceConfig::default(),
        }
    }

    pub fn set_session_user(&mut self, user: impl Into<String>) {
        self.governance.session_user = user.into();
    }

    pub fn set_session_roles(&mut self, roles: Vec<String>) {
        self.governance.session_roles = roles;
    }

    pub fn set_enforce_column_masking(&mut self, enforce: bool) {
        self.governance.enforce_column_masking = enforce;
    }

    pub fn set_masking_policy(
        &mut self,
        policy_tag: impl Into<String>,
        mask_value: Option<BqValue>,
    ) {
        self.governance
            .masking_policies
            .insert(policy_tag.into(), mask_value);
    }

    pub fn set_job_provider(&mut self, provider: Arc<dyn JobInfoProvider>) {
        self.governance.job_provider = Some(provider);
    }

    /// Creates an O(1) copy-on-write isolated fork for parallel tests or ephemeral branching.
    #[must_use]
    pub fn fork_isolated(&self) -> Self {
        self.clone()
    }

    #[cfg(feature = "persistence")]
    pub fn open_persistent(
        path: &std::path::Path,
        default_project: impl Into<String>,
    ) -> BqResult<Self> {
        let proj = default_project.into();
        match crate::storage::persist::load_snapshot(path)? {
            Some((catalog, storage)) => Ok(Self {
                catalog,
                storage,
                governance: GovernanceConfig::default(),
            }),
            None => Ok(Self::with_project(proj)),
        }
    }

    #[cfg(feature = "persistence")]
    pub fn save_persistent(&self, path: &std::path::Path) -> BqResult<()> {
        crate::storage::persist::save_snapshot(path, &self.catalog, &self.storage)
    }

    #[cfg(feature = "persistence")]
    pub fn save_persistent_incremental(
        &self,
        path: &std::path::Path,
        cache: &mut crate::storage::persist::PersistCache,
    ) -> BqResult<()> {
        crate::storage::persist::save_snapshot_incremental(
            path,
            &self.catalog,
            &self.storage,
            cache,
        )
    }

    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub const fn catalog_mut(&mut self) -> &mut Catalog {
        &mut self.catalog
    }

    #[must_use]
    pub const fn storage(&self) -> &StorageEngine {
        &self.storage
    }

    pub const fn storage_mut(&mut self) -> &mut StorageEngine {
        &mut self.storage
    }

    pub fn create_dataset(&mut self, project: &str, dataset: &str) -> BqResult<()> {
        self.catalog
            .create_dataset(project, DatasetMeta::new(dataset, "US"), false)
    }

    pub fn create_table(&mut self, tref: &TableRef, meta: TableMeta) -> BqResult<()> {
        let canonical = self.catalog.canonical_table_ref(tref);
        self.catalog.put_table(&canonical, meta, false, false)?;
        self.storage.create_table(&canonical);
        Ok(())
    }

    pub fn insert_rows(&mut self, tref: &TableRef, rows: Vec<Row>) -> BqResult<()> {
        let canonical = self.catalog.canonical_table_ref(tref);
        let _ = self.catalog.get_table(&canonical)?;
        self.storage.insert_rows(&canonical, rows)
    }

    pub fn insert_rows_at(
        &mut self,
        tref: &TableRef,
        rows: Vec<Row>,
        at: chrono::DateTime<chrono::Utc>,
    ) -> BqResult<()> {
        let canonical = self.catalog.canonical_table_ref(tref);
        let _ = self.catalog.get_table(&canonical)?;
        self.storage.insert_rows_at(&canonical, rows, at)
    }

    /// Projects and filters an in-memory [`QueryResult`] using `GoogleSQL` column names and `WHERE` predicate.
    pub fn project_and_filter_result(
        &self,
        qr: &QueryResult,
        selected_fields: &[String],
        row_restriction: &str,
    ) -> BqResult<QueryResult> {
        let mut ctx = ExecContext::new(&self.catalog, &self.storage);
        ctx.ctes.insert("__src".to_owned(), Arc::new(qr.clone()));
        let cols_sql = if selected_fields.is_empty() {
            "*".to_owned()
        } else {
            selected_fields
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let where_sql = if row_restriction.trim().is_empty() {
            String::new()
        } else {
            format!(" WHERE {row_restriction}")
        };
        let sql = format!("SELECT {cols_sql} FROM __src{where_sql}");
        let q = parse_query(&sql)?;
        execute_query(&q, &ctx)
    }

    /// Executes a read-only `GoogleSQL` query (`SELECT`, `WITH`, `FROM`).
    pub fn query(&self, sql: &str) -> BqResult<QueryResult> {
        self.query_with_params(sql, &BTreeMap::new(), &[])
    }

    /// Executes a read-only `GoogleSQL` query with named (`@p`) and positional (`?`) parameters.
    pub fn query_with_params(
        &self,
        sql: &str,
        params: &BTreeMap<String, BqValue>,
        positional: &[BqValue],
    ) -> BqResult<QueryResult> {
        let q = parse_query(sql)?;
        let mut ctx = ExecContext::new(&self.catalog, &self.storage);
        ctx.params = params.clone();
        ctx.positional_params = positional.to_vec();
        ctx.governance = self.governance.clone();
        execute_query(&q, &ctx)
    }

    /// Executes a single `GoogleSQL` statement.
    pub fn exec(&mut self, sql: &str) -> BqResult<QueryResult> {
        let stmt = parse_statement(sql)?;
        if let Statement::Query(q) = stmt {
            let mut ctx = ExecContext::new(&self.catalog, &self.storage);
            ctx.governance = self.governance.clone();
            return execute_query(&q, &ctx);
        }
        if let Some(res) = execute_ddl(&stmt, &mut self.catalog, &mut self.storage)? {
            return Ok(res);
        }
        if let Some(res) = execute_dml(&stmt, &self.catalog, &mut self.storage)? {
            return Ok(res);
        }
        execute_script_stmts(&[stmt], &mut self.catalog, &mut self.storage)
    }

    /// Drops all temporary tables and routines stored in the ephemeral `__temp` dataset.
    pub fn clear_temp_objects(&mut self) {
        let default_proj = self.catalog.default_project().to_owned();
        let _ = self
            .catalog
            .drop_dataset(&default_proj, "__temp", true, true);
        self.storage.drop_dataset(&default_proj, "__temp");
    }

    /// Executes a multi-statement `GoogleSQL` script (`DECLARE`, `IF`, `LOOP`, `BEGIN...EXCEPTION`, etc.).
    pub fn exec_script(&mut self, sql: &str) -> BqResult<QueryResult> {
        self.exec_script_with_params(sql, &BTreeMap::new(), &[])
    }

    /// Executes a multi-statement `GoogleSQL` script with named (`@p`) and positional (`?`) parameters.
    pub fn exec_script_with_params(
        &mut self,
        sql: &str,
        params: &BTreeMap<String, BqValue>,
        positional: &[BqValue],
    ) -> BqResult<QueryResult> {
        let stmts = parse_script(sql)?;
        let res = execute_script_stmts_with_governance(
            &stmts,
            &mut self.catalog,
            &mut self.storage,
            params,
            positional,
            &self.governance,
        );
        self.clear_temp_objects();
        res
    }
}
