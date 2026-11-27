use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use bq_lite_core::catalog::{JobInfoProvider, JobMetadataEntry, TableRef};
use bq_lite_core::storage::persist::PersistCache;
use bq_lite_core::{BqError, BqResult, BqValue, Engine, QueryResult};
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};

pub const MAX_RETAINED_JOBS: usize = 1_000;
pub const DEFAULT_MAX_RETAINED_RESULT_BYTES: usize = 64 * 1024 * 1024;
const MAX_INSERT_DEDUP: usize = 10_000;
const MAX_EPHEMERAL_ENTRIES: usize = 1_000;

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub job_id: String,
    pub project_id: String,
    pub location: String,
    pub state: JobStatus,
    pub created_at: DateTime<Utc>,
    pub result: Option<Arc<QueryResult>>,
    pub error: Option<BqError>,
    pub total_bytes_processed: u64,
    pub query: Option<String>,
    pub configuration: Option<serde_json::Value>,
    pub load_output_rows: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
}

impl JobStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Done => "DONE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReadSessionRecord {
    pub name: String,
    pub result: Arc<QueryResult>,
    pub data_format: i32,
    pub stream_ranges: BTreeMap<String, (usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteStreamType {
    Committed,
    Pending,
    Buffered,
}

#[derive(Debug, Clone)]
pub struct WriteStreamRecord {
    pub name: String,
    pub table: TableRef,
    pub stream_type: WriteStreamType,
    pub created_at: DateTime<Utc>,
    pub finalized: bool,
    pub flushed_count: usize,
    pub staged_rows: Vec<Vec<BqValue>>,
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub default_project: String,
    pub data_dir: Option<PathBuf>,
    pub storage_emulator_host: Option<String>,
    pub engine: Arc<RwLock<Engine>>,
    pub jobs: Arc<RwLock<BTreeMap<String, JobRecord>>>,
    pub job_order: Arc<RwLock<VecDeque<String>>>,
    job_result_bytes: Arc<Mutex<BTreeMap<String, usize>>>,
    pub insert_dedup: Arc<RwLock<BTreeSet<(String, String)>>>,
    pub dedup_order: Arc<RwLock<VecDeque<(String, String)>>>,
    pub resumable_uploads: Arc<RwLock<BTreeMap<String, serde_json::Value>>>,
    pub upload_order: Arc<RwLock<VecDeque<String>>>,
    pub read_sessions: Arc<RwLock<BTreeMap<String, ReadSessionRecord>>>,
    pub read_session_order: Arc<RwLock<VecDeque<String>>>,
    read_session_bytes: Arc<Mutex<BTreeMap<String, usize>>>,
    pub write_streams: Arc<RwLock<BTreeMap<String, WriteStreamRecord>>>,
    pub write_stream_order: Arc<RwLock<VecDeque<String>>>,
    pub persist_cache: Arc<Mutex<PersistCache>>,
    pub max_retained_result_bytes: usize,
}

#[derive(Debug)]
struct ServerJobProvider {
    jobs: Arc<RwLock<BTreeMap<String, JobRecord>>>,
}

impl JobInfoProvider for ServerJobProvider {
    fn list_jobs(&self, project_id: &str) -> Vec<JobMetadataEntry> {
        self.jobs
            .read()
            .values()
            .filter(|j| j.project_id == project_id)
            .map(|j| JobMetadataEntry {
                project_id: j.project_id.clone(),
                job_id: j.job_id.clone(),
                job_type: if j.load_output_rows.is_some() {
                    "LOAD".to_owned()
                } else if j.query.is_some() {
                    "QUERY".to_owned()
                } else {
                    "COPY".to_owned()
                },
                state: j.state.as_str().to_owned(),
                query: j.query.clone(),
                creation_time: j.created_at,
                total_bytes_processed: i64::try_from(j.total_bytes_processed).unwrap_or(i64::MAX),
                error_message: j.error.as_ref().map(|e| e.message.clone()),
            })
            .collect()
    }
}

impl ServerState {
    #[must_use]
    pub fn new(default_project: impl Into<String>) -> Self {
        let proj = default_project.into();
        Self::from_engine(proj, None, Engine::with_project)
    }

    pub fn with_data_dir(default_project: impl Into<String>, dir: PathBuf) -> BqResult<Self> {
        let proj = default_project.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| BqError::internal(format!("Failed to create data dir: {e}")))?;
        let db_file = dir.join("bq_lite.redb");
        let eng = Engine::open_persistent(&db_file, proj.clone())?;
        Ok(Self::from_engine(proj, Some(dir), |_| eng))
    }

    fn from_engine(
        proj: String,
        data_dir: Option<PathBuf>,
        make_eng: impl FnOnce(String) -> Engine,
    ) -> Self {
        let jobs = Arc::new(RwLock::new(BTreeMap::new()));
        let mut eng = make_eng(proj.clone());
        eng.set_job_provider(Arc::new(ServerJobProvider {
            jobs: Arc::clone(&jobs),
        }));
        Self {
            default_project: proj,
            data_dir,
            storage_emulator_host: std::env::var("STORAGE_EMULATOR_HOST").ok(),
            engine: Arc::new(RwLock::new(eng)),
            jobs,
            job_order: Arc::new(RwLock::new(VecDeque::new())),
            job_result_bytes: Arc::new(Mutex::new(BTreeMap::new())),
            insert_dedup: Arc::new(RwLock::new(BTreeSet::new())),
            dedup_order: Arc::new(RwLock::new(VecDeque::new())),
            resumable_uploads: Arc::new(RwLock::new(BTreeMap::new())),
            upload_order: Arc::new(RwLock::new(VecDeque::new())),
            read_sessions: Arc::new(RwLock::new(BTreeMap::new())),
            read_session_order: Arc::new(RwLock::new(VecDeque::new())),
            read_session_bytes: Arc::new(Mutex::new(BTreeMap::new())),
            write_streams: Arc::new(RwLock::new(BTreeMap::new())),
            write_stream_order: Arc::new(RwLock::new(VecDeque::new())),
            persist_cache: Arc::new(Mutex::new(PersistCache::default())),
            max_retained_result_bytes: DEFAULT_MAX_RETAINED_RESULT_BYTES,
        }
    }

    #[must_use]
    pub const fn with_max_retained_result_bytes(mut self, max_bytes: usize) -> Self {
        self.max_retained_result_bytes = max_bytes;
        self
    }

    #[must_use]
    pub fn retained_job_result_bytes(&self) -> usize {
        self.job_result_bytes.lock().values().sum()
    }

    #[must_use]
    pub fn retained_read_session_bytes(&self) -> usize {
        self.read_session_bytes.lock().values().sum()
    }

    pub fn record_job(&self, rec: JobRecord) {
        let id = rec.job_id.clone();
        let bytes = rec.result.as_ref().map_or(0, |r| r.estimated_bytes());
        let mut jobs = self.jobs.write();
        let is_new = jobs.insert(id.clone(), rec).is_none();
        let mut order = self.job_order.write();
        let mut byte_map = self.job_result_bytes.lock();
        if bytes > 0 {
            byte_map.insert(id.clone(), bytes);
        } else {
            byte_map.remove(&id);
        }
        if is_new {
            order.push_back(id.clone());
            while order.len() > MAX_RETAINED_JOBS {
                if let Some(evicted) = order.pop_front() {
                    jobs.remove(&evicted);
                    byte_map.remove(&evicted);
                }
            }
        }
        let mut total_bytes: usize = byte_map.values().sum();
        if total_bytes > self.max_retained_result_bytes {
            for old_id in order.iter() {
                if total_bytes <= self.max_retained_result_bytes {
                    break;
                }
                if old_id == &id {
                    continue;
                }
                if let Some(removed_bytes) = byte_map.remove(old_id) {
                    if let Some(old_job) = jobs.get_mut(old_id) {
                        old_job.result = None;
                    }
                    total_bytes = total_bytes.saturating_sub(removed_bytes);
                }
            }
        }
        drop(byte_map);
        drop(order);
        drop(jobs);
    }

    #[must_use]
    pub fn check_and_record_dedup(&self, key: (String, String)) -> bool {
        let inserted = self.insert_dedup.write().insert(key.clone());
        if inserted {
            let mut order = self.dedup_order.write();
            order.push_back(key);
            while order.len() > MAX_INSERT_DEDUP {
                if let Some(evicted) = order.pop_front() {
                    self.insert_dedup.write().remove(&evicted);
                }
            }
        }
        inserted
    }

    pub fn record_resumable_upload(&self, upload_id: String, meta: serde_json::Value) {
        insert_bounded(
            &self.resumable_uploads,
            &self.upload_order,
            upload_id,
            meta,
            MAX_EPHEMERAL_ENTRIES,
        );
    }

    pub fn record_read_session(&self, name: String, rec: ReadSessionRecord) {
        let bytes = rec.result.estimated_bytes();
        let mut map = self.read_sessions.write();
        let is_new = map.insert(name.clone(), rec).is_none();
        let mut order = self.read_session_order.write();
        let mut byte_map = self.read_session_bytes.lock();
        byte_map.insert(name.clone(), bytes);
        if is_new {
            order.push_back(name);
            while order.len() > MAX_EPHEMERAL_ENTRIES {
                if let Some(evicted) = order.pop_front() {
                    map.remove(&evicted);
                    byte_map.remove(&evicted);
                }
            }
        }
        let mut total_bytes: usize = byte_map.values().sum();
        while total_bytes > self.max_retained_result_bytes && order.len() > 1 {
            if let Some(evicted) = order.pop_front() {
                map.remove(&evicted);
                if let Some(removed_bytes) = byte_map.remove(&evicted) {
                    total_bytes = total_bytes.saturating_sub(removed_bytes);
                }
            }
        }
        drop(byte_map);
        drop(order);
        drop(map);
    }

    pub fn record_write_stream(&self, name: String, rec: WriteStreamRecord) {
        insert_bounded(
            &self.write_streams,
            &self.write_stream_order,
            name,
            rec,
            MAX_EPHEMERAL_ENTRIES,
        );
    }

    pub fn persist_if_enabled(&self) {
        if let Some(ref dir) = self.data_dir {
            let db_file = dir.join("bq_lite.redb");
            let mut cache = self.persist_cache.lock();
            let res = self
                .engine
                .read()
                .save_persistent_incremental(&db_file, &mut cache);
            if let Err(err) = res {
                tracing::error!(error = %err, "Failed to persist redb snapshot");
            }
        }
    }

    pub fn reset(&self) {
        let mut eng = Engine::with_project(self.default_project.clone());
        eng.set_job_provider(Arc::new(ServerJobProvider {
            jobs: Arc::clone(&self.jobs),
        }));
        *self.engine.write() = eng;
        self.jobs.write().clear();
        self.job_order.write().clear();
        self.job_result_bytes.lock().clear();
        self.insert_dedup.write().clear();
        self.dedup_order.write().clear();
        self.resumable_uploads.write().clear();
        self.upload_order.write().clear();
        self.read_sessions.write().clear();
        self.read_session_order.write().clear();
        self.read_session_bytes.lock().clear();
        self.write_streams.write().clear();
        self.write_stream_order.write().clear();
        *self.persist_cache.lock() = PersistCache::default();
        self.persist_if_enabled();
    }
}

fn insert_bounded<V>(
    map: &RwLock<BTreeMap<String, V>>,
    order: &RwLock<VecDeque<String>>,
    key: String,
    val: V,
    cap: usize,
) {
    let is_new = map.write().insert(key.clone(), val).is_none();
    if is_new {
        let mut ord = order.write();
        ord.push_back(key);
        while ord.len() > cap {
            if let Some(evicted) = ord.pop_front() {
                map.write().remove(&evicted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bq_lite_core::executor::QueryColumn;
    use bq_lite_core::BqType;

    fn make_sized_result(payload_len: usize) -> Arc<QueryResult> {
        Arc::new(QueryResult::new(
            vec![QueryColumn::new("payload", BqType::String)],
            vec![vec![BqValue::String("x".repeat(payload_len))]],
        ))
    }

    fn make_job(job_id: &str, payload_len: usize) -> JobRecord {
        JobRecord {
            job_id: job_id.to_owned(),
            project_id: "local".to_owned(),
            location: "US".to_owned(),
            state: JobStatus::Done,
            created_at: Utc::now(),
            result: Some(make_sized_result(payload_len)),
            error: None,
            total_bytes_processed: 0,
            query: Some(format!("SELECT '{job_id}'")),
            configuration: None,
            load_output_rows: None,
        }
    }

    #[test]
    fn job_result_cache_evicts_oldest_payloads_over_byte_budget() {
        let state = ServerState::new("local").with_max_retained_result_bytes(2_500);
        state.record_job(make_job("j1", 1_000));
        state.record_job(make_job("j2", 1_000));
        state.record_job(make_job("j3", 1_000));

        assert!(state.retained_job_result_bytes() <= 2_500);
        let jobs = state.jobs.read();
        // Metadata for j1 stays retained for INFORMATION_SCHEMA.JOBS, only its cached result is dropped.
        assert!(jobs.contains_key("j1"));
        assert!(jobs["j1"].result.is_none());
        assert_eq!(jobs["j1"].query.as_deref(), Some("SELECT 'j1'"));
        // Newest job result stays cached.
        assert!(jobs["j3"].result.is_some());
        drop(jobs);
    }

    #[test]
    fn read_session_cache_evicts_oldest_sessions_over_byte_budget() {
        let state = ServerState::new("local").with_max_retained_result_bytes(2_500);
        for i in 1..=3 {
            let name = format!("s{i}");
            state.record_read_session(
                name.clone(),
                ReadSessionRecord {
                    name,
                    result: make_sized_result(1_000),
                    data_format: 1,
                    stream_ranges: BTreeMap::new(),
                },
            );
        }

        assert!(state.retained_read_session_bytes() <= 2_500);
        let sessions = state.read_sessions.read();
        assert!(!sessions.contains_key("s1"));
        assert!(sessions.contains_key("s3"));
        drop(sessions);
    }
}
