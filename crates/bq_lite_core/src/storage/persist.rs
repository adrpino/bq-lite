use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{BqError, BqResult};
use crate::storage::{StorageEngine, TableStorage};

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
            let bytes = serde_json::to_vec(arc.as_ref())
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
                        let tstorage: TableStorage = serde_json::from_slice(v_guard.value())
                            .map_err(|e| {
                                BqError::internal(format!("Failed to deserialize table: {e}"))
                            })?;
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
