use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ast::ObjectName;
use crate::error::{BqError, BqResult};

use super::models::{DatasetMeta, ProjectMeta, RoutineMeta, RowAccessPolicyMeta, TableMeta};

/// Resolved 3-part `BigQuery` resource reference `(project, dataset, object)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableRef {
    pub project: String,
    pub dataset: String,
    pub table: String,
}

impl TableRef {
    #[must_use]
    pub fn new(
        project: impl Into<String>,
        dataset: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            dataset: dataset.into(),
            table: table.into(),
        }
    }

    #[must_use]
    pub fn storage_key(&self) -> (String, String, String) {
        (
            self.project.clone(),
            self.dataset.clone(),
            self.table.clone(),
        )
    }
}

/// Copy-on-write catalog registry for `Project -> Dataset -> Table/View/Routine`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    default_project: String,
    default_dataset: Option<String>,
    projects: BTreeMap<String, Arc<ProjectMeta>>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new("local")
    }
}

impl Catalog {
    #[must_use]
    pub fn new(default_project: impl Into<String>) -> Self {
        let proj_id = default_project.into();
        let mut projects = BTreeMap::new();
        projects.insert(proj_id.clone(), Arc::new(ProjectMeta::new(proj_id.clone())));
        Self {
            default_project: proj_id,
            default_dataset: None,
            projects,
        }
    }

    #[must_use]
    pub fn default_project(&self) -> &str {
        &self.default_project
    }

    #[must_use]
    pub fn default_dataset(&self) -> Option<&str> {
        self.default_dataset.as_deref()
    }

    pub fn set_default_dataset(&mut self, ds: Option<String>) {
        self.default_dataset = ds;
    }

    pub fn ensure_project(&mut self, project: &str) -> &mut ProjectMeta {
        let arc = self
            .projects
            .entry(project.to_owned())
            .or_insert_with(|| Arc::new(ProjectMeta::new(project)));
        Arc::make_mut(arc)
    }

    #[must_use]
    pub fn get_project(&self, project: &str) -> Option<&ProjectMeta> {
        self.projects.get(project).map(AsRef::as_ref)
    }

    #[must_use]
    pub fn list_projects(&self) -> Vec<&str> {
        self.projects.keys().map(String::as_str).collect()
    }

    pub fn create_dataset(
        &mut self,
        project: &str,
        ds: DatasetMeta,
        if_not_exists: bool,
    ) -> BqResult<()> {
        let proj = self.ensure_project(project);
        if proj.datasets.contains_key(&ds.name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(BqError::already_exists(format!(
                "Already Exists: Dataset {project}:{}",
                ds.name
            )));
        }
        proj.datasets.insert(ds.name.clone(), Arc::new(ds));
        Ok(())
    }

    pub fn drop_dataset(
        &mut self,
        project: &str,
        dataset: &str,
        if_exists: bool,
        cascade: bool,
    ) -> BqResult<()> {
        let Some(proj_arc) = self.projects.get_mut(project) else {
            if if_exists {
                return Ok(());
            }
            return Err(BqError::not_found(format!("Not found: Project {project}")));
        };
        let proj = Arc::make_mut(proj_arc);
        let Some(ds_arc) = proj.datasets.get(dataset) else {
            if if_exists {
                return Ok(());
            }
            return Err(BqError::not_found(format!(
                "Not found: Dataset {project}:{dataset}"
            )));
        };
        if !cascade && !ds_arc.tables.is_empty() {
            return Err(BqError::invalid_argument(format!(
                "Dataset {project}:{dataset} is still in use (pass CASCADE)"
            )));
        }
        proj.datasets.remove(dataset);
        Ok(())
    }

    pub fn get_dataset(&self, project: &str, dataset: &str) -> BqResult<&DatasetMeta> {
        let proj = self
            .projects
            .get(project)
            .ok_or_else(|| BqError::not_found(format!("Not found: Project {project}")))?;
        proj.datasets
            .get(dataset)
            .map(AsRef::as_ref)
            .ok_or_else(|| BqError::not_found(format!("Not found: Dataset {project}:{dataset}")))
    }

    pub fn get_dataset_mut(&mut self, project: &str, dataset: &str) -> BqResult<&mut DatasetMeta> {
        let proj = self
            .projects
            .get_mut(project)
            .ok_or_else(|| BqError::not_found(format!("Not found: Project {project}")))?;
        let ds_arc = Arc::make_mut(proj)
            .datasets
            .get_mut(dataset)
            .ok_or_else(|| BqError::not_found(format!("Not found: Dataset {project}:{dataset}")))?;
        Ok(Arc::make_mut(ds_arc))
    }

    pub fn resolve_dataset_name(&self, name: &ObjectName) -> BqResult<(String, String)> {
        let parts = name.normalized_parts();
        match parts.as_slice() {
            [ds] => Ok((self.default_project.clone(), ds.clone())),
            [proj, ds] => Ok((proj.clone(), ds.clone())),
            _ => Err(BqError::invalid_argument(format!(
                "Invalid dataset name: {}",
                parts.join(".")
            ))),
        }
    }

    pub fn resolve_table_name(&self, name: &ObjectName) -> BqResult<TableRef> {
        let parts = name.normalized_parts();
        let raw = match parts.as_slice() {
            [tbl] => {
                let ds = self.default_dataset.as_ref().ok_or_else(|| {
                    BqError::invalid_argument(format!(
                        "Table name '{tbl}' must be qualified with a dataset when no default dataset is set"
                    ))
                })?;
                TableRef::new(&self.default_project, ds, tbl)
            }
            [ds, tbl] => TableRef::new(&self.default_project, ds, tbl),
            [proj, ds, tbl] => TableRef::new(proj, ds, tbl),
            _ => {
                return Err(BqError::invalid_argument(format!(
                    "Invalid table identifier: {}",
                    parts.join(".")
                )))
            }
        };
        Ok(self.canonical_table_ref(&raw))
    }

    #[must_use]
    pub fn canonical_table_ref(&self, tref: &TableRef) -> TableRef {
        if let Ok(ds) = self.get_dataset(&tref.project, &tref.dataset) {
            if let Some(key) = ds.find_table_key(&tref.table) {
                return TableRef::new(&tref.project, &tref.dataset, key);
            }
        }
        tref.clone()
    }

    pub fn put_table(
        &mut self,
        tref: &TableRef,
        meta: TableMeta,
        or_replace: bool,
        if_not_exists: bool,
    ) -> BqResult<bool> {
        let ds = self.get_dataset_mut(&tref.project, &tref.dataset)?;
        if let Some(existing_key) = ds.find_table_key(&tref.table) {
            if if_not_exists && !or_replace {
                return Ok(false);
            }
            if !or_replace {
                return Err(BqError::already_exists(format!(
                    "Already Exists: Table {}:{}.{}",
                    tref.project, tref.dataset, tref.table
                )));
            }
            ds.tables.insert(existing_key, Arc::new(meta));
            return Ok(true);
        }
        ds.tables.insert(tref.table.clone(), Arc::new(meta));
        Ok(true)
    }

    pub fn get_table(&self, tref: &TableRef) -> BqResult<&TableMeta> {
        let ds = self.get_dataset(&tref.project, &tref.dataset)?;
        ds.get_table(&tref.table).map(AsRef::as_ref).ok_or_else(|| {
            BqError::not_found(format!(
                "Not found: Table {}:{}.{}",
                tref.project, tref.dataset, tref.table
            ))
        })
    }

    pub fn drop_table(&mut self, tref: &TableRef, if_exists: bool) -> BqResult<()> {
        let Ok(ds) = self.get_dataset_mut(&tref.project, &tref.dataset) else {
            if if_exists {
                return Ok(());
            }
            return Err(BqError::not_found(format!(
                "Not found: Dataset {}:{}",
                tref.project, tref.dataset
            )));
        };
        let Some(key) = ds.find_table_key(&tref.table) else {
            if if_exists {
                return Ok(());
            }
            return Err(BqError::not_found(format!(
                "Not found: Table {}:{}.{}",
                tref.project, tref.dataset, tref.table
            )));
        };
        ds.tables.remove(&key);
        ds.policies.remove(&key);
        Ok(())
    }

    pub fn put_routine(
        &mut self,
        tref: &TableRef,
        routine: RoutineMeta,
        or_replace: bool,
        if_not_exists: bool,
    ) -> BqResult<()> {
        let ds = self.get_dataset_mut(&tref.project, &tref.dataset)?;
        if ds.routines.contains_key(&tref.table) {
            if if_not_exists && !or_replace {
                return Ok(());
            }
            if !or_replace {
                return Err(BqError::already_exists(format!(
                    "Already Exists: Routine {}:{}.{}",
                    tref.project, tref.dataset, tref.table
                )));
            }
        }
        ds.routines.insert(tref.table.clone(), Arc::new(routine));
        Ok(())
    }

    pub fn put_policy(
        &mut self,
        tref: &TableRef,
        policy: RowAccessPolicyMeta,
        or_replace: bool,
        if_not_exists: bool,
    ) -> BqResult<()> {
        let _ = self.get_table(tref)?;
        let ds = self.get_dataset_mut(&tref.project, &tref.dataset)?;
        let list = ds.policies.entry(tref.table.clone()).or_default();
        if let Some(pos) = list.iter().position(|p| p.name == policy.name) {
            if if_not_exists && !or_replace {
                return Ok(());
            }
            if !or_replace {
                return Err(BqError::already_exists(format!(
                    "Already Exists: Row access policy {}",
                    policy.name
                )));
            }
            if let Some(slot) = list.get_mut(pos) {
                *slot = policy;
            }
            return Ok(());
        }
        list.push(policy);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Ident;
    use crate::types::BqType;
    use chrono::Utc;

    #[test]
    fn resolves_three_part_and_backtick_table_names() {
        let mut cat = Catalog::new("my-proj");
        cat.set_default_dataset(Some("default_ds".to_owned()));

        let one = cat
            .resolve_table_name(&ObjectName::new(vec![Ident::unquoted("events")]))
            .unwrap();
        assert_eq!(one, TableRef::new("my-proj", "default_ds", "events"));

        let backtick = cat
            .resolve_table_name(&ObjectName::new(vec![Ident::quoted("other-p.ds1.tbl1")]))
            .unwrap();
        assert_eq!(backtick, TableRef::new("other-p", "ds1", "tbl1"));
    }

    #[test]
    fn cow_clone_isolates_catalog_mutations() {
        let mut cat = Catalog::new("local");
        cat.create_dataset("local", DatasetMeta::new("ds", "US"), false)
            .unwrap();
        let mut fork = cat.clone();
        fork.put_table(
            &TableRef::new("local", "ds", "t1"),
            TableMeta {
                name: "t1".to_owned(),
                kind: crate::catalog::TableKind::Table,
                columns: vec![super::super::ColumnSchema::new("id", BqType::Int64)],
                partition_by: None,
                cluster_by: Vec::new(),
                view_query: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                options: BTreeMap::new(),
                description: None,
                friendly_name: None,
                labels: BTreeMap::new(),
                view_sql: None,
                expiration_ms: None,
                external_uris: Vec::new(),
                external_format: None,
            },
            false,
            false,
        )
        .unwrap();

        assert!(cat.get_table(&TableRef::new("local", "ds", "t1")).is_err());
        assert!(fork.get_table(&TableRef::new("local", "ds", "t1")).is_ok());
    }
}
