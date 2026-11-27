use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ast::{CreateFunctionStmt, CreateProcedureStmt, Expr, Query};
use crate::types::{BqType, BqValue};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    pub ty: BqType,
    pub nullable: bool,
    pub default_expr: Option<Expr>,
    pub description: Option<String>,
    #[serde(default)]
    pub policy_tag: Option<String>,
}

impl ColumnSchema {
    #[must_use]
    pub fn new(name: impl Into<String>, ty: BqType) -> Self {
        Self {
            name: name.into(),
            ty,
            nullable: true,
            default_expr: None,
            description: None,
            policy_tag: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
    Snapshot,
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableMeta {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<ColumnSchema>,
    pub partition_by: Option<String>,
    pub cluster_by: Vec<String>,
    pub view_query: Option<Box<Query>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub options: BTreeMap<String, BqValue>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub view_sql: Option<String>,
    #[serde(default)]
    pub expiration_ms: Option<i64>,
    #[serde(default)]
    pub external_uris: Vec<String>,
    #[serde(default)]
    pub external_format: Option<String>,
    #[serde(default)]
    pub primary_key: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobMetadataEntry {
    pub project_id: String,
    pub job_id: String,
    pub job_type: String,
    pub state: String,
    pub query: Option<String>,
    pub creation_time: DateTime<Utc>,
    pub total_bytes_processed: i64,
    pub error_message: Option<String>,
}

pub trait JobInfoProvider: Send + Sync + std::fmt::Debug {
    fn list_jobs(&self, project_id: &str) -> Vec<JobMetadataEntry>;
}

impl TableMeta {
    #[must_use]
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnSchema> {
        self.column_index(name).and_then(|i| self.columns.get(i))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RoutineMeta {
    Function(Box<CreateFunctionStmt>),
    Procedure(Box<CreateProcedureStmt>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowAccessPolicyMeta {
    pub name: String,
    pub grantees: Vec<String>,
    pub filter_expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetMeta {
    pub name: String,
    pub location: String,
    pub is_case_insensitive: bool,
    pub tables: BTreeMap<String, Arc<TableMeta>>,
    pub routines: BTreeMap<String, Arc<RoutineMeta>>,
    pub policies: BTreeMap<String, Vec<RowAccessPolicyMeta>>,
    pub options: BTreeMap<String, BqValue>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub default_table_expiration_ms: Option<i64>,
    #[serde(default)]
    pub access: Vec<serde_json::Value>,
}

impl DatasetMeta {
    #[must_use]
    pub fn new(name: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            location: location.into(),
            is_case_insensitive: false,
            tables: BTreeMap::new(),
            routines: BTreeMap::new(),
            policies: BTreeMap::new(),
            options: BTreeMap::new(),
            created_at: Utc::now(),
            description: None,
            friendly_name: None,
            labels: BTreeMap::new(),
            default_table_expiration_ms: None,
            access: Vec::new(),
        }
    }

    #[must_use]
    pub fn find_table_key(&self, name: &str) -> Option<String> {
        if self.tables.contains_key(name) {
            return Some(name.to_owned());
        }
        if self.is_case_insensitive {
            return self
                .tables
                .keys()
                .find(|k| k.eq_ignore_ascii_case(name))
                .cloned();
        }
        None
    }

    #[must_use]
    pub fn get_table(&self, name: &str) -> Option<&Arc<TableMeta>> {
        let key = self.find_table_key(name)?;
        self.tables.get(&key)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: String,
    pub datasets: BTreeMap<String, Arc<DatasetMeta>>,
}

impl ProjectMeta {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            datasets: BTreeMap::new(),
        }
    }
}
