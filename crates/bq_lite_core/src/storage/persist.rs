use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{BqError, BqResult};
use crate::storage::{Row, RowChunk, StorageEngine, TableSnapshot, TableStorage, TableVersion};

const SNAPSHOT_TABLE: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("bq_lite_snapshot");
const SNAPSHOT_KEY: &str = "state_v1";

const CATALOG_V2_TABLE: TableDefinition<'_, &str, &[u8]> =
    TableDefinition::new("bq_lite_catalog_v2");
const CATALOG_V2_KEY: &str = "catalog";
const TABLES_V2_TABLE: TableDefinition<'_, &str, &[u8]> = TableDefinition::new("bq_lite_tables_v2");

pub(crate) type TableEntry = ((String, String, String), TableStorage);
type DirtyEntry = ((String, String, String), Arc<TableStorage>, Vec<u8>);

#[derive(Debug, Default)]
pub struct PersistCache {
    saved: BTreeMap<(String, String, String), Arc<TableStorage>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EngineSnapshot {
    catalog: Catalog,
    tables: Vec<TableEntry>,
}

/// Chunk-deduplicated wire format so shared `Arc<RowChunk>`s are serialized only once across history.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedTableV2 {
    chunks: Vec<RowChunk>,
    current_chunks: Vec<usize>,
    history: Vec<(DateTime<Utc>, Vec<usize>)>,
}

#[derive(Debug, Deserialize)]
struct LegacyTableVersion {
    timestamp: DateTime<Utc>,
    rows: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct LegacyTableStorage {
    current: Vec<Row>,
    #[serde(default)]
    ingestion_times: Vec<DateTime<Utc>>,
    #[serde(default)]
    history: Vec<LegacyTableVersion>,
}

fn encode_table_storage(ts: &TableStorage) -> PersistedTableV2 {
    let mut ptr_to_idx: HashMap<usize, usize> = HashMap::new();
    let mut chunks: Vec<RowChunk> = Vec::new();
    let mut intern = |snap: &TableSnapshot| -> Vec<usize> {
        let mut indices = Vec::with_capacity(snap.chunks.len());
        for ch in &snap.chunks {
            let key = Arc::as_ptr(ch) as usize;
            let idx = *ptr_to_idx.entry(key).or_insert_with(|| {
                let id = chunks.len();
                chunks.push((**ch).clone());
                id
            });
            indices.push(idx);
        }
        indices
    };
    let current_chunks = intern(&ts.current);
    let history = ts
        .history
        .iter()
        .map(|v| (v.timestamp, intern(&v.snapshot)))
        .collect();
    PersistedTableV2 {
        chunks,
        current_chunks,
        history,
    }
}

fn decode_table_storage(raw: &[u8]) -> BqResult<TableStorage> {
    if let Ok(v2) = serde_json::from_slice::<PersistedTableV2>(raw) {
        let shared: Vec<Arc<RowChunk>> = v2.chunks.into_iter().map(Arc::new).collect();
        let rebuild = |idxs: &[usize]| -> TableSnapshot {
            let mut chs = Vec::with_capacity(idxs.len());
            let mut total_rows = 0_usize;
            for &i in idxs {
                if let Some(ch) = shared.get(i) {
                    total_rows = total_rows.saturating_add(ch.rows.len());
                    chs.push(Arc::clone(ch));
                }
            }
            TableSnapshot {
                chunks: chs,
                total_rows,
            }
        };
        let current = rebuild(&v2.current_chunks);
        let history = v2
            .history
            .into_iter()
            .map(|(timestamp, idxs)| TableVersion {
                timestamp,
                snapshot: rebuild(&idxs),
            })
            .collect();
        return Ok(TableStorage { current, history });
    }
    if let Ok(ts) = serde_json::from_slice::<TableStorage>(raw) {
        return Ok(ts);
    }
    let legacy: LegacyTableStorage = serde_json::from_slice(raw)
        .map_err(|e| BqError::internal(format!("Failed to deserialize table: {e}")))?;
    let default_ts = legacy
        .ingestion_times
        .first()
        .copied()
        .unwrap_or_else(Utc::now);
    let mut current = TableSnapshot::default();
    current.push_chunk(legacy.current, default_ts);
    let history = if legacy.history.is_empty() {
        vec![TableVersion {
            timestamp: default_ts,
            snapshot: current.clone(),
        }]
    } else {
        legacy
            .history
            .into_iter()
            .map(|v| {
                let mut snap = TableSnapshot::default();
                snap.push_chunk(v.rows, v.timestamp);
                TableVersion {
                    timestamp: v.timestamp,
                    snapshot: snap,
                }
            })
            .collect()
    };
    Ok(TableStorage { current, history })
}

fn encode_table_key((p, d, t): &(String, String, String)) -> String {
    format!("{p}\0{d}\0{t}")
}

fn decode_table_key(raw: &str) -> Option<(String, String, String)> {
    let mut parts = raw.splitn(3, '\0');
    let p = parts.next()?.to_owned();
    let d = parts.next()?.to_owned();
    let t = parts.next()?.to_owned();
    Some((p, d, t))
}

/// Saves `catalog` and `storage` atomically to a `redb` database file.
pub fn save_snapshot(path: &Path, catalog: &Catalog, storage: &StorageEngine) -> BqResult<()> {
    let mut cache = PersistCache::default();
    save_snapshot_incremental(path, catalog, storage, &mut cache)
}

/// Incrementally saves `catalog` and only modified [`TableStorage`] chunks to `redb`.
pub fn save_snapshot_incremental(
    path: &Path,
    catalog: &Catalog,
    storage: &StorageEngine,
    cache: &mut PersistCache,
) -> BqResult<()> {
    let db = Database::create(path)
        .map_err(|e| BqError::internal(format!("Failed to open redb database: {e}")))?;
    let cat_bytes = serde_json::to_vec(catalog)
        .map_err(|e| BqError::internal(format!("Failed to serialize catalog: {e}")))?;

    let live_tables = storage.tables_map();
    let removed_keys: Vec<(String, String, String)> = cache
        .saved
        .keys()
        .filter(|k| !live_tables.contains_key(*k))
        .cloned()
        .collect();

    let mut dirty: Vec<DirtyEntry> = Vec::new();
    for (k, arc) in live_tables {
        let unchanged = cache
            .saved
            .get(k)
            .is_some_and(|prev| Arc::ptr_eq(prev, arc));
        if !unchanged {
            let encoded = encode_table_storage(arc.as_ref());
            let bytes = serde_json::to_vec(&encoded)
                .map_err(|e| BqError::internal(format!("Failed to serialize table: {e}")))?;
            dirty.push((k.clone(), Arc::clone(arc), bytes));
        }
    }

    let write_txn = db
        .begin_write()
        .map_err(|e| BqError::internal(format!("redb write txn failed: {e}")))?;
    {
        let mut cat_tbl = write_txn
            .open_table(CATALOG_V2_TABLE)
            .map_err(|e| BqError::internal(format!("redb open catalog table failed: {e}")))?;
        cat_tbl
            .insert(CATALOG_V2_KEY, cat_bytes.as_slice())
            .map_err(|e| BqError::internal(format!("redb catalog insert failed: {e}")))?;

        let mut data_tbl = write_txn
            .open_table(TABLES_V2_TABLE)
            .map_err(|e| BqError::internal(format!("redb open data table failed: {e}")))?;
        for rk in &removed_keys {
            let encoded = encode_table_key(rk);
            let _ = data_tbl.remove(encoded.as_str());
            cache.saved.remove(rk);
        }
        for (k, arc, bytes) in dirty {
            let encoded = encode_table_key(&k);
            data_tbl
                .insert(encoded.as_str(), bytes.as_slice())
                .map_err(|e| BqError::internal(format!("redb table insert failed: {e}")))?;
            cache.saved.insert(k, arc);
        }
    }
    write_txn
        .commit()
        .map_err(|e| BqError::internal(format!("redb commit failed: {e}")))?;
    Ok(())
}

/// Loads `catalog` and `storage` from an existing `redb` database file if present.
pub fn load_snapshot(path: &Path) -> BqResult<Option<(Catalog, StorageEngine)>> {
    if !path.exists() {
        return Ok(None);
    }
    let db = Database::create(path)
        .map_err(|e| BqError::internal(format!("Failed to open redb database: {e}")))?;
    let read_txn = db
        .begin_read()
        .map_err(|e| BqError::internal(format!("redb read txn failed: {e}")))?;

    if let Ok(cat_tbl) = read_txn.open_table(CATALOG_V2_TABLE) {
        if let Some(cat_val) = cat_tbl
            .get(CATALOG_V2_KEY)
            .map_err(|e| BqError::internal(format!("redb catalog get failed: {e}")))?
        {
            let catalog: Catalog = serde_json::from_slice(cat_val.value())
                .map_err(|e| BqError::internal(format!("Failed to deserialize catalog: {e}")))?;
            let mut entries = Vec::new();
            if let Ok(data_tbl) = read_txn.open_table(TABLES_V2_TABLE) {
                let iter = data_tbl
                    .iter()
                    .map_err(|e| BqError::internal(format!("redb iter failed: {e}")))?;
                for item in iter {
                    let (k_guard, v_guard) = item
                        .map_err(|e| BqError::internal(format!("redb entry read failed: {e}")))?;
                    if let Some(tkey) = decode_table_key(k_guard.value()) {
                        let tstorage = decode_table_storage(v_guard.value())?;
                        entries.push((tkey, tstorage));
                    }
                }
            }
            return Ok(Some((catalog, StorageEngine::from_entries(entries))));
        }
    }

    let Ok(table) = read_txn.open_table(SNAPSHOT_TABLE) else {
        return Ok(None);
    };
    let Some(val) = table
        .get(SNAPSHOT_KEY)
        .map_err(|e| BqError::internal(format!("redb get failed: {e}")))?
    else {
        return Ok(None);
    };
    let snap: EngineSnapshot = serde_json::from_slice(val.value())
        .map_err(|e| BqError::internal(format!("Failed to deserialize snapshot: {e}")))?;
    Ok(Some((
        snap.catalog,
        StorageEngine::from_entries(snap.tables),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::TableRef;
    use crate::types::BqValue;

    #[test]
    fn persisted_v2_deduplicates_shared_chunks_and_restores_sharing() {
        let mut ts = TableStorage::new();
        for i in 0..10_i64 {
            ts.append(vec![vec![BqValue::Int64(i)]]);
        }
        let encoded = encode_table_storage(&ts);
        // 10 appends across 11 versions should serialize only 10 distinct chunks total.
        assert_eq!(encoded.chunks.len(), 10);
        assert_eq!(encoded.current_chunks.len(), 10);
        assert_eq!(encoded.history.len(), 11);

        let raw = serde_json::to_vec(&encoded).unwrap();
        let decoded = decode_table_storage(&raw).unwrap();
        assert_eq!(decoded.current.len(), 10);
        assert!(Arc::ptr_eq(
            &decoded.current.chunks[0],
            &decoded.history[1].snapshot.chunks[0]
        ));
    }

    #[test]
    fn decode_table_storage_reads_legacy_unsharded_json() {
        let now = Utc::now();
        let legacy_json = serde_json::json!({
            "current": [[{ "Int64": 42 }]],
            "ingestion_times": [now],
            "history": [{ "timestamp": now, "rows": [[{ "Int64": 42 }]] }]
        });
        let raw = serde_json::to_vec(&legacy_json).unwrap();
        let decoded = decode_table_storage(&raw).unwrap();
        assert_eq!(decoded.current.len(), 1);
        assert_eq!(decoded.current.to_rows(), vec![vec![BqValue::Int64(42)]]);
        let _ = TableRef::new("p", "d", "t");
    }
}
