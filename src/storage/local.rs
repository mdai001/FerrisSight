//! Durable local inventory and backend-neutral upload work. Never logs paths or raw errors.
use crate::core::CameraId;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub retention_hours: u32,
    pub max_storage_bytes: u64,
    pub protect_unuploaded: bool,
    /// Reserves future uploader control; Phase 2A makes no network requests.
    pub upload_enabled: bool,
}
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            retention_hours: 72,
            max_storage_bytes: 100 * 1024 * 1024 * 1024,
            protect_unuploaded: true,
            upload_enabled: false,
        }
    }
}
impl StorageConfig {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.retention_hours == 0
            || self.max_storage_bytes == 0
            || self.max_storage_bytes > i64::MAX as u64
        {
            return Err(StorageError::Configuration);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct StorageStatus {
    pub available: bool,
    pub degraded: bool,
    pub used_bytes: u64,
    pub configured_limit_bytes: u64,
    pub storage_pressure: bool,
    pub pending_upload_count: u64,
    pub uploading_count: u64,
    pub retry_count: u64,
    pub failed_count: u64,
    pub oldest_pending_age_seconds: Option<u64>,
    pub deleted_unuploaded_count: u64,
    pub missing_recording_count: u64,
    pub rejected_file_count: u64,
    pub cleanup_failures: u64,
    pub upload_enabled: bool,
}
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("invalid storage configuration")]
    Configuration,
    #[error("local storage unavailable")]
    Io,
    #[error("storage database unavailable")]
    Database,
    #[error("storage coordinator already active")]
    Busy,
    #[error("invalid finalized recording")]
    InvalidRecording,
    #[error("invalid upload state transition")]
    Transition,
}
impl From<rusqlite::Error> for StorageError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}
fn io<T>(r: std::io::Result<T>) -> Result<T, StorageError> {
    r.map_err(|_| StorageError::Io)
}
#[derive(Clone)]
pub struct UploadJob {
    pub segment_id: Uuid,
    pub camera_id: CameraId,
    /// Validated, generated relative UTC path. Never an arbitrary local filename.
    pub relative_path: String,
    pub start_unix_ms: i64,
    pub end_unix_ms: i64,
    pub size_bytes: u64,
    pub attempt: u64,
}
impl std::fmt::Debug for UploadJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadJob")
            .field("segment_id", &self.segment_id)
            .field("camera_id", &self.camera_id)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}
/// Opaque provider locator. It never appears in public status or normal formatting.
pub struct RemoteObjectId(String);
impl std::fmt::Debug for RemoteObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}
impl RemoteObjectId {
    pub fn new(value: String) -> Result<Self, StorageError> {
        if value.is_empty()
            || value.len() > 256
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(StorageError::Configuration);
        }
        Ok(Self(value))
    }
}
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("upload temporarily unavailable")]
    Retryable,
    #[error("upload rejected")]
    Permanent,
}
#[async_trait::async_trait]
pub trait UploadBackend: Send + Sync {
    /// Backends must use segment_id as an idempotency key and confirm remote durability
    /// before success. Input is an already-open local file, not a local absolute path.
    async fn upload(&self, job: &UploadJob, file: File) -> Result<RemoteObjectId, UploadError>;
}
#[derive(Deserialize)]
struct Sidecar {
    #[serde(rename = "segmentId")]
    segment_id: Option<Uuid>,
    #[serde(rename = "cameraId")]
    camera_id: CameraId,
    #[serde(rename = "utcTiming")]
    timing: Timing,
}
#[derive(Deserialize)]
struct Timing {
    logical_minute_start_unix_ms: i64,
    first_frame_unix_ms: i64,
    end_frame_unix_ms: i64,
}
/// One coordinator owns this store. SQLite is never called on the real-time recorder path.
/// The root must be a trusted, local filesystem directory, not a network share.
pub struct LocalStore {
    root: PathBuf,
    db: Connection,
    _owner: File,
    config: StorageConfig,
}
impl LocalStore {
    pub fn open(root: &Path, config: StorageConfig, now_ms: i64) -> Result<Self, StorageError> {
        config.validate()?;
        if now_ms < 0 {
            return Err(StorageError::Configuration);
        }
        io(fs::create_dir_all(root))?;
        let root = io(fs::canonicalize(root))?;
        let control = root.join(".storage");
        if control.exists() && io(fs::symlink_metadata(&control))?.file_type().is_symlink() {
            return Err(StorageError::Io);
        }
        io(fs::create_dir_all(&control))?;
        let owner_path = control.join("owner.lock");
        for name in [
            "owner.lock",
            "queue.sqlite3",
            "queue.sqlite3-wal",
            "queue.sqlite3-shm",
            "queue.sqlite3-journal",
        ] {
            if let Ok(meta) = fs::symlink_metadata(control.join(name)) {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(StorageError::Io);
                }
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let owner = io(options.open(owner_path))?;
        owner.try_lock_exclusive().map_err(|_| StorageError::Busy)?;
        // Precreate with private permissions; SQLite journals inherit database permissions.
        io(options.open(control.join("queue.sqlite3")))?;
        let db = Connection::open(control.join("queue.sqlite3"))?;
        db.busy_timeout(Duration::from_millis(250))?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err(StorageError::Database);
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS recordings (
            segment_id TEXT PRIMARY KEY, camera_id TEXT NOT NULL, relative_path TEXT NOT NULL,
            start_ms INTEGER NOT NULL CHECK(start_ms>=0), end_ms INTEGER NOT NULL CHECK(end_ms>start_ms), size_bytes INTEGER NOT NULL CHECK(size_bytes>0),
            state TEXT NOT NULL CHECK(state IN ('pending','uploading','retry_wait','uploaded','failed')),
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count>=0), next_retry_ms INTEGER NOT NULL DEFAULT 0,
            remote_object_id TEXT, local_state TEXT NOT NULL DEFAULT 'present'
                CHECK(local_state IN ('present','deleting','deleted','missing')));
            CREATE UNIQUE INDEX IF NOT EXISTS live_path ON recordings(relative_path)
                WHERE local_state IN ('present','deleting');
            CREATE INDEX IF NOT EXISTS upload_due ON recordings(local_state,state,next_retry_ms,start_ms);
            PRAGMA user_version=1;")?;
        db.execute("UPDATE recordings SET state='retry_wait', next_retry_ms=?1 WHERE state='uploading' AND local_state='present'", [now_ms])?;
        Ok(Self {
            root,
            db,
            _owner: owner,
            config,
        })
    }
    pub fn set_config(&mut self, config: StorageConfig) -> Result<(), StorageError> {
        config.validate()?;
        self.config = config;
        Ok(())
    }
    /// Only already-finalized UTC MP4s with matching bounded JSON sidecars are indexed.
    pub fn enqueue(&mut self, relative: &str) -> Result<Uuid, StorageError> {
        let (camera, logical_minute) = validate_path(relative)?;
        let path = self.safe_path(relative)?;
        let media = io(fs::metadata(&path))?;
        if !media.is_file() || media.len() == 0 || media.len() > i64::MAX as u64 {
            return Err(StorageError::InvalidRecording);
        }
        let reader = mp4::Mp4Reader::read_header(io(File::open(&path))?, media.len())
            .map_err(|_| StorageError::InvalidRecording)?;
        if reader
            .sample_count(1)
            .map_err(|_| StorageError::InvalidRecording)?
            == 0
        {
            return Err(StorageError::InvalidRecording);
        }
        let metadata_rel = relative.trim_end_matches(".mp4").to_owned() + ".json";
        let meta_path = self.safe_path(&metadata_rel)?;
        let mut json = Vec::new();
        io(io(File::open(meta_path))?
            .take(65537)
            .read_to_end(&mut json))?;
        if json.len() > 65536 {
            return Err(StorageError::InvalidRecording);
        }
        let sidecar: Sidecar =
            serde_json::from_slice(&json).map_err(|_| StorageError::InvalidRecording)?;
        let timing = sidecar.timing;
        if sidecar.camera_id != camera
            || timing.logical_minute_start_unix_ms != logical_minute
            || timing.first_frame_unix_ms < 0
            || timing.end_frame_unix_ms <= timing.first_frame_unix_ms
        {
            return Err(StorageError::InvalidRecording);
        }
        let id = sidecar.segment_id.unwrap_or_else(|| {
            Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!(
                    "{relative}:{}:{}:{}",
                    timing.first_frame_unix_ms,
                    timing.end_frame_unix_ms,
                    media.len()
                )
                .as_bytes(),
            )
        });
        self.db.execute("INSERT INTO recordings(segment_id,camera_id,relative_path,start_ms,end_ms,size_bytes,state)
            VALUES(?1,?2,?3,?4,?5,?6,'pending') ON CONFLICT(segment_id) DO NOTHING",
            params![id.to_string(),camera.to_string(),relative,timing.first_frame_unix_ms,timing.end_frame_unix_ms,media.len() as i64])?;
        let matches: bool = self.db.query_row("SELECT camera_id=?2 AND relative_path=?3 AND start_ms=?4 AND end_ms=?5 AND size_bytes=?6 FROM recordings WHERE segment_id=?1",
            params![id.to_string(),camera.to_string(),relative,timing.first_frame_unix_ms,timing.end_frame_unix_ms,media.len() as i64], |r|r.get(0))?;
        if !matches {
            return Err(StorageError::InvalidRecording);
        }
        self.db.execute("UPDATE recordings SET local_state='present',state=CASE WHEN state='uploaded' THEN state ELSE 'pending' END,next_retry_ms=0 WHERE segment_id=?1 AND local_state IN ('missing','deleted')",[id.to_string()])?;
        Ok(id)
    }
    fn safe_path(&self, relative: &str) -> Result<PathBuf, StorageError> {
        // Metadata uses the same strict generated filename layout as its MP4.
        let mp4 = relative.strip_suffix(".json").map(|s| format!("{s}.mp4"));
        validate_path(mp4.as_deref().unwrap_or(relative))?;
        let mut path = self.root.clone();
        for component in relative.split('/') {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(StorageError::InvalidRecording)
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(StorageError::Io),
            }
        }
        Ok(path)
    }
    pub fn claim_next(&mut self, now_ms: i64) -> Result<Option<UploadJob>, StorageError> {
        if !self.config.upload_enabled {
            return Ok(None);
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row=tx.query_row("SELECT segment_id,camera_id,relative_path,start_ms,end_ms,size_bytes,attempt_count FROM recordings
            WHERE local_state='present' AND (state='pending' OR (state='retry_wait' AND next_retry_ms<=?1)) ORDER BY start_ms,segment_id LIMIT 1",
            [now_ms], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?))).optional()?;
        let Some((id, camera, path, start, end, size, attempt)) = row else {
            return Ok(None);
        };
        tx.execute("UPDATE recordings SET state='uploading',attempt_count=attempt_count+1 WHERE segment_id=?1",[&id])?;
        tx.commit()?;
        Ok(Some(UploadJob {
            segment_id: Uuid::parse_str(&id).map_err(|_| StorageError::Database)?,
            camera_id: serde_json::from_value(serde_json::Value::String(camera))
                .map_err(|_| StorageError::Database)?,
            relative_path: path,
            start_unix_ms: start,
            end_unix_ms: end,
            size_bytes: u64::try_from(size).map_err(|_| StorageError::Database)?,
            attempt: u64::try_from(attempt.checked_add(1).ok_or(StorageError::Database)?)
                .map_err(|_| StorageError::Database)?,
        }))
    }
    pub fn open_upload(&self, job: &UploadJob) -> Result<File, StorageError> {
        let live: bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM recordings WHERE segment_id=?1 AND state='uploading' AND attempt_count=?2 AND local_state='present' AND relative_path=?3)",params![job.segment_id.to_string(),i64::try_from(job.attempt).map_err(|_|StorageError::Transition)?,job.relative_path],|r|r.get(0))?;
        if !live {
            return Err(StorageError::Transition);
        }
        let path = self.safe_path(&job.relative_path)?;
        let file = io(File::open(path))?;
        if io(file.metadata())?.len() != job.size_bytes {
            return Err(StorageError::InvalidRecording);
        }
        Ok(file)
    }
    pub fn uploaded(
        &mut self,
        job: &UploadJob,
        remote: RemoteObjectId,
    ) -> Result<(), StorageError> {
        self.finish_upload(job, "uploaded", 0, Some(remote.0))
    }
    pub fn retry(&mut self, job: &UploadJob, next_retry_ms: i64) -> Result<(), StorageError> {
        if next_retry_ms < 0 {
            return Err(StorageError::Configuration);
        }
        self.finish_upload(job, "retry_wait", next_retry_ms, None)
    }
    pub fn fail(&mut self, job: &UploadJob) -> Result<(), StorageError> {
        self.finish_upload(job, "failed", 0, None)
    }
    fn finish_upload(
        &mut self,
        job: &UploadJob,
        state: &str,
        next: i64,
        remote: Option<String>,
    ) -> Result<(), StorageError> {
        let n=self.db.execute("UPDATE recordings SET state=?3,next_retry_ms=?4,remote_object_id=?5 WHERE segment_id=?1 AND attempt_count=?2 AND state='uploading' AND local_state='present'",
            params![job.segment_id.to_string(),i64::try_from(job.attempt).map_err(|_|StorageError::Transition)?,state,next,remote])?;
        if n != 1 {
            return Err(StorageError::Transition);
        }
        Ok(())
    }
    /// Scan repairs missed enqueue events. A DB error never removes finalized media.
    pub fn maintain(&mut self, now_ms: i64) -> Result<StorageStatus, StorageError> {
        if now_ms < 0 {
            return Err(StorageError::Configuration);
        }
        let mut status = StorageStatus {
            available: true,
            configured_limit_bytes: self.config.max_storage_bytes,
            upload_enabled: self.config.upload_enabled,
            ..Default::default()
        };
        // Resume committed deletion intents BEFORE rediscovering files.
        for (id, path) in self
            .rows("SELECT segment_id,relative_path FROM recordings WHERE local_state='deleting'")?
        {
            if self.delete_marked(&id, &path).is_err() {
                status.cleanup_failures += 1;
            }
        }
        let mut finalized = vec![];
        walk(&self.root, &self.root, 0, &mut finalized, &mut status)?;
        let mut rejected = std::collections::HashSet::new();
        for path in finalized {
            if self.enqueue(&path).is_err() {
                status.rejected_file_count += 1;
                rejected.insert(path);
            }
        }
        for (id, path) in self
            .rows("SELECT segment_id,relative_path FROM recordings WHERE local_state='present'")?
        {
            let safe = self.safe_path(&path)?;
            if !safe.exists() {
                self.db.execute("UPDATE recordings SET local_state='missing',state=CASE WHEN state='uploaded' THEN state ELSE 'failed' END WHERE segment_id=?1",[id])?;
            }
        }
        let cutoff = now_ms.saturating_sub(i64::from(self.config.retention_hours) * 3_600_000);
        let mut stmt=self.db.prepare("SELECT segment_id,relative_path,state,end_ms FROM recordings WHERE local_state='present' AND state!='uploading' ORDER BY CASE WHEN state='uploaded' THEN 0 ELSE 1 END,end_ms,segment_id")?;
        let candidates = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, path, state, end) in candidates {
            if rejected.contains(&path) {
                continue;
            }
            let uploaded = state == "uploaded";
            let over = status.used_bytes >= self.config.max_storage_bytes;
            if !(uploaded && end <= cutoff || over && (uploaded || !self.config.protect_unuploaded))
            {
                continue;
            }
            let bytes = self.pair_size(&path)?;
            // Durable intent prevents upload claims; a crash retries the deletion on startup.
            self.db.execute("UPDATE recordings SET local_state='deleting' WHERE segment_id=?1 AND local_state='present' AND state!='uploading'",[&id])?;
            if self.delete_marked(&id, &path).is_err() {
                status.cleanup_failures += 1;
            }
            // A metadata/pruning/DB failure may happen AFTER media bytes were reclaimed.
            // Count actual remaining files so we do not evict extra recordings by mistake.
            let remaining = self.pair_size(&path)?;
            status.used_bytes = status
                .used_bytes
                .saturating_sub(bytes.saturating_sub(remaining));
        }
        status.storage_pressure = status.used_bytes >= self.config.max_storage_bytes;
        status.degraded = status.cleanup_failures > 0 || status.rejected_file_count > 0;
        for state in ["pending", "uploading", "retry_wait", "failed"] {
            let n: u64 = self.db.query_row(
                "SELECT count(*) FROM recordings WHERE local_state='present' AND state=?1",
                [state],
                |r| r.get::<_, i64>(0),
            )? as u64;
            match state {
                "pending" => status.pending_upload_count = n,
                "uploading" => status.uploading_count = n,
                "retry_wait" => status.retry_count = n,
                _ => status.failed_count = n,
            }
        }
        let oldest:Option<i64>=self.db.query_row("SELECT min(start_ms) FROM recordings WHERE local_state='present' AND state IN ('pending','uploading','retry_wait','failed')",[],|r|r.get(0))?;
        status.oldest_pending_age_seconds =
            oldest.map(|t| now_ms.saturating_sub(t).max(0) as u64 / 1000);
        status.deleted_unuploaded_count = self.db.query_row(
            "SELECT count(*) FROM recordings WHERE local_state='deleted' AND state!='uploaded'",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        status.missing_recording_count = self.db.query_row(
            "SELECT count(*) FROM recordings WHERE local_state='missing'",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        status.degraded |= status.missing_recording_count > 0;
        Ok(status)
    }
    fn rows(&self, sql: &str) -> Result<Vec<(String, String)>, StorageError> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    fn pair_size(&self, path: &str) -> Result<u64, StorageError> {
        let p = self.safe_path(path)?;
        Ok(fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
            + fs::metadata(p.with_extension("json"))
                .map(|m| m.len())
                .unwrap_or(0))
    }
    fn delete_marked(&mut self, id: &str, relative: &str) -> Result<(), StorageError> {
        let mp4 = self.safe_path(relative)?;
        let metadata_relative = relative.trim_end_matches(".mp4").to_owned() + ".json";
        let metadata = self.safe_path(&metadata_relative)?;
        // A recorder can reuse a now-empty minute filename after a coordinator crash.
        // Recover the OLD deletion intent without deleting a newly published segment.
        let stored_size: i64 = self.db.query_row(
            "SELECT size_bytes FROM recordings WHERE segment_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if metadata.exists() {
            let mut bytes = Vec::new();
            io(io(File::open(&metadata))?
                .take(65537)
                .read_to_end(&mut bytes))?;
            if bytes.len() > 65536 {
                return Err(StorageError::InvalidRecording);
            }
            let sidecar: Sidecar =
                serde_json::from_slice(&bytes).map_err(|_| StorageError::InvalidRecording)?;
            let current_size = fs::metadata(&mp4)
                .map(|m| m.len())
                .unwrap_or(stored_size as u64);
            let current_id = sidecar.segment_id.unwrap_or_else(|| {
                Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    format!(
                        "{relative}:{}:{}:{current_size}",
                        sidecar.timing.first_frame_unix_ms, sidecar.timing.end_frame_unix_ms
                    )
                    .as_bytes(),
                )
            });
            if current_id.to_string() != id {
                self.db.execute("UPDATE recordings SET local_state='deleted' WHERE segment_id=?1 AND local_state='deleting'",[id])?;
                return Ok(());
            }
            if current_size != stored_size as u64 {
                return Err(StorageError::InvalidRecording);
            }
        } else if mp4.exists() {
            return Err(StorageError::InvalidRecording);
        }
        // Remove publication marker first. Never touch a partial, even during recovery.
        for p in [&mp4, &metadata] {
            match fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(StorageError::Io),
            }
        }
        if let Some(parent) = mp4.parent() {
            if parent.exists() {
                sync_directory(parent)?;
            }
        }
        let mut parent = mp4.parent();
        while let Some(p) = parent {
            if p.parent() == Some(self.root.as_path()) || p == self.root {
                break;
            }
            match fs::remove_dir(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(StorageError::Io),
            }
            parent = p.parent();
        }
        self.db.execute("UPDATE recordings SET local_state='deleted' WHERE segment_id=?1 AND local_state='deleting'",[id])?;
        Ok(())
    }
}
fn validate_path(relative: &str) -> Result<(CameraId, i64), StorageError> {
    let p: Vec<_> = relative.split('/').collect();
    if p.len() != 6 {
        return Err(StorageError::InvalidRecording);
    }
    let id = p[0]
        .strip_prefix("camera-")
        .ok_or(StorageError::InvalidRecording)?;
    let uuid = Uuid::parse_str(id).map_err(|_| StorageError::InvalidRecording)?;
    if uuid.to_string() != id {
        return Err(StorageError::InvalidRecording);
    }
    let camera = serde_json::from_value(serde_json::Value::String(id.into()))
        .map_err(|_| StorageError::InvalidRecording)?;
    for (s, len) in [(p[1], 4), (p[2], 2), (p[3], 2), (p[4], 2)] {
        if s.len() != len || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StorageError::InvalidRecording);
        }
    }
    let stem = p[5]
        .strip_suffix(".mp4")
        .ok_or(StorageError::InvalidRecording)?;
    let (minute, seq) = stem.split_once('_').ok_or(StorageError::InvalidRecording)?;
    if minute.len() != 2
        || seq.len() < 3
        || seq.len() > 20
        || !minute
            .bytes()
            .chain(seq.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(StorageError::InvalidRecording);
    }
    let time = chrono::NaiveDateTime::parse_from_str(
        &format!("{}-{}-{} {}:{}:00", p[1], p[2], p[3], p[4], minute),
        "%Y-%m-%d %H:%M:%S",
    )
    .map_err(|_| StorageError::InvalidRecording)?;
    Ok((camera, time.and_utc().timestamp_millis()))
}
fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    finalized: &mut Vec<String>,
    status: &mut StorageStatus,
) -> Result<(), StorageError> {
    if depth > 6 {
        status.rejected_file_count += 1;
        return Ok(());
    }
    for entry in io(fs::read_dir(dir))? {
        let entry = io(entry)?;
        if depth == 0 && entry.file_name() == ".storage" {
            continue;
        }
        let meta = io(fs::symlink_metadata(entry.path()))?;
        if meta.file_type().is_symlink() {
            status.rejected_file_count += 1;
            continue;
        }
        if meta.is_dir() {
            walk(root, &entry.path(), depth + 1, finalized, status)?;
        } else if meta.is_file() {
            status.used_bytes = status.used_bytes.saturating_add(meta.len());
            if entry.path().extension().is_some_and(|e| e == "mp4") {
                if let Some(relative) = entry
                    .path()
                    .strip_prefix(root)
                    .ok()
                    .and_then(|p| p.to_str())
                {
                    finalized.push(relative.replace('\\', "/"));
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn sync_directory(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        io(io(File::open(path))?.sync_all())?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
