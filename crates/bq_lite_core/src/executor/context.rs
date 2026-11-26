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
    pub qualifier: Option<String>,
    pub name: String,
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
        qr.rows
            .iter()
            .map(|row| {
                let cols = qr
                    .columns
                    .iter()
                    .zip(row.iter())
                    .map(|(col, val)| RowBinding {
                        qualifier: alias.map(str::to_owned),
                        name: col.name.clone(),
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
                    name: Some(c.name.clone()),
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
    pub ctes: BTreeMap<String, QueryResult>,
    pub params: BTreeMap<String, BqValue>,
    pub positional_params: Vec<BqValue>,
    pub vars: BTreeMap<String, BqValue>,
    pub sys_vars: BTreeMap<String, BqValue>,
    pub outer_row: Option<&'a EvalRow>,
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
            outer_row: None,
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
        ExecContext {
            catalog: self.catalog,
            storage: self.storage,
            ctes: self.ctes.clone(),
            params: self.params.clone(),
            positional_params: self.positional_params.clone(),
            vars: self.vars.clone(),
            sys_vars: self.sys_vars.clone(),
            outer_row: Some(outer),
            now: self.now,
            governance: self.governance.clone(),
        }
    }
}
