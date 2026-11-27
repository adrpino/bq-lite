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

/// An immutable batch of rows ingested together, shared across `CoW` snapshots via `Arc`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowChunk {
    pub rows: Vec<Row>,
    pub ingested_at: DateTime<Utc>,
}

/// Chunked table snapshot sharing immutable [`RowChunk`] segments across versions and clones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub chunks: Vec<Arc<RowChunk>>,
    pub total_rows: usize,
}

impl TableSnapshot {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.total_rows
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total_rows == 0
    }

    pub fn push_chunk(&mut self, rows: Vec<Row>, ingested_at: DateTime<Utc>) {
        if rows.is_empty() {
            return;
        }
        self.total_rows = self.total_rows.saturating_add(rows.len());
        self.chunks.push(Arc::new(RowChunk { rows, ingested_at }));
    }

    #[must_use]
    pub fn iter_with_times(&self) -> TableRowIter<'_> {
        TableRowIter {
            chunks: &self.chunks,
            chunk_idx: 0,
            row_idx: 0,
        }
    }

    #[must_use]
    pub fn to_rows(&self) -> Vec<Row> {
        let mut out = Vec::with_capacity(self.total_rows);
        for chunk in &self.chunks {
            out.extend(chunk.rows.iter().cloned());
        }
        out
    }
}

/// Zero-allocation iterator yielding `(&Row, DateTime<Utc>)` across chunked table storage.
#[derive(Debug, Clone)]
pub struct TableRowIter<'a> {
    chunks: &'a [Arc<RowChunk>],
    chunk_idx: usize,
    row_idx: usize,
}

impl TableRowIter<'_> {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            chunks: &[],
            chunk_idx: 0,
            row_idx: 0,
        }
    }
}

impl<'a> Iterator for TableRowIter<'a> {
    type Item = (&'a Row, DateTime<Utc>);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(chunk) = self.chunks.get(self.chunk_idx) {
            if let Some(row) = chunk.rows.get(self.row_idx) {
                self.row_idx += 1;
                return Some((row, chunk.ingested_at));
            }
            self.chunk_idx += 1;
            self.row_idx = 0;
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableVersion {
    pub timestamp: DateTime<Utc>,
    pub snapshot: TableSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableStorage {
    pub current: TableSnapshot,
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
        let snap = TableSnapshot::default();
        Self {
            current: snap.clone(),
            history: vec![TableVersion {
                timestamp: Utc::now(),
                snapshot: snap,
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
        self.current.push_chunk(new_rows, at);
        self.record_version();
    }

    pub fn replace(&mut self, rows: Vec<Row>) {
        let mut snap = TableSnapshot::default();
        snap.push_chunk(rows, Utc::now());
        self.current = snap;
        self.record_version();
    }

    pub fn replace_snapshot(&mut self, snapshot: TableSnapshot) {
        self.current = snapshot;
        self.record_version();
    }

    #[must_use]
    pub fn snapshot_at(&self, ts: DateTime<Utc>) -> Option<TableSnapshot> {
        self.history
            .iter()
            .rev()
            .find(|v| v.timestamp <= ts)
            .map(|v| v.snapshot.clone())
    }

    fn record_version(&mut self) {
        self.history.push(TableVersion {
            timestamp: Utc::now(),
            snapshot: self.current.clone(),
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
            .map_or_else(Vec::new, |s| s.current.to_rows())
    }

    #[must_use]
    pub fn iter_rows(&self, tref: &TableRef) -> (usize, TableRowIter<'_>) {
        self.tables
            .get(&tref.storage_key())
            .map_or((0, TableRowIter::empty()), |s| {
                (s.current.len(), s.current.iter_with_times())
            })
    }

    #[must_use]
    pub fn read_rows_with_times(
        &self,
        tref: &TableRef,
        _fallback: DateTime<Utc>,
    ) -> Vec<(Row, DateTime<Utc>)> {
        let Some(s) = self.tables.get(&tref.storage_key()) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(s.current.len());
        for (row, ts) in s.current.iter_with_times() {
            out.push((row.clone(), ts));
        }
        out
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
            .map(|snap| snap.to_rows())
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
        let snap = match as_of {
            Some(ts) => {
                let storage = self.tables.get(&src.storage_key()).ok_or_else(|| {
                    BqError::not_found(format!(
                        "Not found: Table {}:{}.{}",
                        src.project, src.dataset, src.table
                    ))
                })?;
                storage.snapshot_at(ts).ok_or_else(|| {
                    BqError::invalid_argument(format!(
                        "No snapshot available for {}:{}.{} at {ts}",
                        src.project, src.dataset, src.table
                    ))
                })?
            }
            None => self
                .tables
                .get(&src.storage_key())
                .map_or_else(TableSnapshot::default, |s| s.current.clone()),
        };
        let storage_arc = self
            .tables
            .entry(dst.storage_key())
            .or_insert_with(|| Arc::new(TableStorage::new()));
        Arc::make_mut(storage_arc).replace_snapshot(snap);
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

    #[test]
    fn chunked_appends_and_clones_share_existing_chunks_without_recloning() {
        let mut store = StorageEngine::new();
        let src = TableRef::new("local", "ds", "src");
        let dst = TableRef::new("local", "ds", "dst");
        store.create_table(&src);

        for i in 0..5_i64 {
            store
                .insert_rows(&src, vec![vec![BqValue::Int64(i)]])
                .unwrap();
        }
        let ts = store.tables.get(&src.storage_key()).unwrap();
        assert_eq!(ts.current.len(), 5);
        assert_eq!(ts.current.chunks.len(), 5);
        // Every historical version shares the exact same Arc<RowChunk> pointers for earlier chunks.
        assert!(Arc::ptr_eq(
            &ts.current.chunks[0],
            &ts.history[1].snapshot.chunks[0]
        ));
        assert!(Arc::ptr_eq(
            &ts.current.chunks[2],
            &ts.history[3].snapshot.chunks[2]
        ));

        // Cloning shares all chunk pointers zero-copy, and mutating dst leaves src isolated.
        store.clone_table_data(&src, &dst, None).unwrap();
        {
            let src_ts = store.tables.get(&src.storage_key()).unwrap();
            let dst_ts = store.tables.get(&dst.storage_key()).unwrap();
            assert_eq!(src_ts.current.chunks.len(), dst_ts.current.chunks.len());
            for (a, b) in src_ts.current.chunks.iter().zip(&dst_ts.current.chunks) {
                assert!(Arc::ptr_eq(a, b));
            }
        }
        store
            .insert_rows(&dst, vec![vec![BqValue::Int64(99)]])
            .unwrap();
        assert_eq!(store.read_rows(&src).len(), 5);
        assert_eq!(store.read_rows(&dst).len(), 6);
    }
}
