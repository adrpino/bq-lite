use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use bq_lite_core::catalog::{JobInfoProvider, JobMetadataEntry, TableRef};
use bq_lite_core::storage::persist::PersistCache;
use bq_lite_core::{BqError, BqResult, BqValue, Engine, QueryResult};
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};

pub const MAX_RETAINED_JOBS: usize = 1_000;
const MAX_INSERT_DEDUP: usize = 10_000;
const MAX_EPHEMERAL_ENTRIES: usize = 1_000;

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub job_id: String,
    pub project_id: String,
    pub location: String,
    pub state: JobStatus,
    pub created_at: DateTime<Utc>,
    pub result: Option<QueryResult>,
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
    pub result: QueryResult,
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
    pub insert_dedup: Arc<RwLock<BTreeSet<(String, String)>>>,
    pub dedup_order: Arc<RwLock<VecDeque<(String, String)>>>,
    pub resumable_uploads: Arc<RwLock<BTreeMap<String, serde_json::Value>>>,
    pub upload_order: Arc<RwLock<VecDeque<String>>>,
    pub read_sessions: Arc<RwLock<BTreeMap<String, ReadSessionRecord>>>,
    pub read_session_order: Arc<RwLock<VecDeque<String>>>,
    pub write_streams: Arc<RwLock<BTreeMap<String, WriteStreamRecord>>>,
    pub write_stream_order: Arc<RwLock<VecDeque<String>>>,
    pub persist_cache: Arc<Mutex<PersistCache>>,
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
            insert_dedup: Arc::new(RwLock::new(BTreeSet::new())),
            dedup_order: Arc::new(RwLock::new(VecDeque::new())),
            resumable_uploads: Arc::new(RwLock::new(BTreeMap::new())),
            upload_order: Arc::new(RwLock::new(VecDeque::new())),
            read_sessions: Arc::new(RwLock::new(BTreeMap::new())),
            read_session_order: Arc::new(RwLock::new(VecDeque::new())),
            write_streams: Arc::new(RwLock::new(BTreeMap::new())),
            write_stream_order: Arc::new(RwLock::new(VecDeque::new())),
            persist_cache: Arc::new(Mutex::new(PersistCache::default())),
        }
    }

    pub fn record_job(&self, rec: JobRecord) {
        let id = rec.job_id.clone();
        let is_new = self.jobs.write().insert(id.clone(), rec).is_none();
        if is_new {
            let mut order = self.job_order.write();
            order.push_back(id);
            while order.len() > MAX_RETAINED_JOBS {
                if let Some(evicted) = order.pop_front() {
                    self.jobs.write().remove(&evicted);
                }
            }
        }
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
        insert_bounded(
            &self.read_sessions,
            &self.read_session_order,
            name,
            rec,
            MAX_EPHEMERAL_ENTRIES,
        );
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
        self.insert_dedup.write().clear();
        self.dedup_order.write().clear();
        self.resumable_uploads.write().clear();
        self.upload_order.write().clear();
        self.read_sessions.write().clear();
        self.read_session_order.write().clear();
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
