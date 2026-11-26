#[cfg(feature = "persistence")]
pub mod persist;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::catalog::TableRef;
use crate::error::{BqError, BqResult};
use crate::types::BqValue;

pub type Row = Vec<BqValue>;

const MAX_TABLE_HISTORY: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableVersion {
    pub timestamp: DateTime<Utc>,
    pub rows: Arc<Vec<Row>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableStorage {
    pub current: Arc<Vec<Row>>,
    #[serde(default)]
    pub ingestion_times: Arc<Vec<DateTime<Utc>>>,
    pub history: Vec<TableVersion>,
}

impl Default for TableStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl TableStorage {
    #[must_use]
    pub fn new() -> Self {
        let rows = Arc::new(Vec::new());
        Self {
            current: Arc::clone(&rows),
            ingestion_times: Arc::new(Vec::new()),
            history: vec![TableVersion {
                timestamp: Utc::now(),
                rows,
            }],
        }
    }

    pub fn append(&mut self, new_rows: Vec<Row>) {
        self.append_at(new_rows, Utc::now());
    }

    pub fn append_at(&mut self, new_rows: Vec<Row>, at: DateTime<Utc>) {
        if new_rows.is_empty() {
            return;
        }
        let count = new_rows.len();
        Arc::make_mut(&mut self.current).extend(new_rows);
        Arc::make_mut(&mut self.ingestion_times).extend(std::iter::repeat_n(at, count));
        self.record_version();
    }

    pub fn replace(&mut self, rows: Vec<Row>) {
        let now = Utc::now();
        let count = rows.len();
        self.current = Arc::new(rows);
        self.ingestion_times = Arc::new(vec![now; count]);
        self.record_version();
    }

    #[must_use]
    pub fn snapshot_at(&self, ts: DateTime<Utc>) -> Option<Arc<Vec<Row>>> {
        self.history
            .iter()
            .rev()
            .find(|v| v.timestamp <= ts)
            .map(|v| Arc::clone(&v.rows))
    }

    fn record_version(&mut self) {
        self.history.push(TableVersion {
            timestamp: Utc::now(),
            rows: Arc::clone(&self.current),
        });
        while self.history.len() > MAX_TABLE_HISTORY {
            // Preserve the initial version at index 0 and evict the oldest intermediate version.
            self.history.remove(1);
        }
    }
}

/// Copy-on-write storage engine keyed by `(project, dataset, table)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StorageEngine {
    tables: BTreeMap<(String, String, String), Arc<TableStorage>>,
}

impl StorageEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_table(&mut self, tref: &TableRef) {
        self.tables
            .insert(tref.storage_key(), Arc::new(TableStorage::new()));
    }

    pub fn insert_rows(&mut self, tref: &TableRef, rows: Vec<Row>) -> BqResult<()> {
        self.insert_rows_at(tref, rows, Utc::now())
    }

    pub fn insert_rows_at(
        &mut self,
        tref: &TableRef,
        rows: Vec<Row>,
        at: DateTime<Utc>,
    ) -> BqResult<()> {
        let storage_arc = self
            .tables
            .entry(tref.storage_key())
            .or_insert_with(|| Arc::new(TableStorage::new()));
        Arc::make_mut(storage_arc).append_at(rows, at);
        Ok(())
    }

    pub fn replace_rows(&mut self, tref: &TableRef, rows: Vec<Row>) {
        let storage_arc = self
            .tables
            .entry(tref.storage_key())
            .or_insert_with(|| Arc::new(TableStorage::new()));
        Arc::make_mut(storage_arc).replace(rows);
    }

    #[must_use]
    pub fn read_rows(&self, tref: &TableRef) -> Vec<Row> {
        self.tables
            .get(&tref.storage_key())
            .map_or_else(Vec::new, |s| (*s.current).clone())
    }

    #[must_use]
    pub fn read_rows_with_times(
        &self,
        tref: &TableRef,
        fallback: DateTime<Utc>,
    ) -> Vec<(Row, DateTime<Utc>)> {
        let Some(s) = self.tables.get(&tref.storage_key()) else {
            return Vec::new();
        };
        s.current
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let ts = s.ingestion_times.get(i).copied().unwrap_or(fallback);
                (row.clone(), ts)
            })
            .collect()
    }

    pub fn read_rows_as_of(&self, tref: &TableRef, ts: DateTime<Utc>) -> BqResult<Vec<Row>> {
        let storage = self.tables.get(&tref.storage_key()).ok_or_else(|| {
            BqError::not_found(format!(
                "Not found: Table {}:{}.{}",
                tref.project, tref.dataset, tref.table
            ))
        })?;
        storage
            .snapshot_at(ts)
            .map(|arc| (*arc).clone())
            .ok_or_else(|| {
                BqError::invalid_argument(format!(
                    "No snapshot available for {}:{}.{} at {ts}",
                    tref.project, tref.dataset, tref.table
                ))
            })
    }

    pub fn clone_table_data(
        &mut self,
        src: &TableRef,
        dst: &TableRef,
        as_of: Option<DateTime<Utc>>,
    ) -> BqResult<()> {
        let rows = match as_of {
            Some(ts) => self.read_rows_as_of(src, ts)?,
            None => self.read_rows(src),
        };
        self.replace_rows(dst, rows);
        Ok(())
    }

    pub fn drop_table(&mut self, tref: &TableRef) {
        self.tables.remove(&tref.storage_key());
    }

    pub fn drop_dataset(&mut self, project: &str, dataset: &str) {
        self.tables
            .retain(|(p, d, _), _| !(p == project && d == dataset));
    }

    #[cfg(feature = "persistence")]
    pub(crate) const fn tables_map(
        &self,
    ) -> &BTreeMap<(String, String, String), Arc<TableStorage>> {
        &self.tables
    }

    #[cfg(feature = "persistence")]
    pub(crate) fn from_entries(entries: Vec<persist::TableEntry>) -> Self {
        let tables = entries.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
        Self { tables }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cow_fork_and_time_travel_work_independently() {
        let mut store = StorageEngine::new();
        let tref = TableRef::new("local", "ds", "t");
        store.create_table(&tref);
        store
            .insert_rows(&tref, vec![vec![BqValue::Int64(1)]])
            .unwrap();
        let t1 = Utc::now();

        let mut fork = store.clone();
        fork.insert_rows(&tref, vec![vec![BqValue::Int64(2)]])
            .unwrap();

        assert_eq!(store.read_rows(&tref).len(), 1);
        assert_eq!(fork.read_rows(&tref).len(), 2);
        assert_eq!(fork.read_rows_as_of(&tref, t1).unwrap().len(), 1);
    }
}
