//! Experimental import checkpoint journal, not a scheduler or media publisher.
//! The caller must serialize workers and validate published media before completion.
use super::{SourceError, UtcRange};
use crate::{core::CameraId, storage::RecordingId};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fmt;
use uuid::Uuid;
#[derive(Clone, PartialEq, Eq)]
pub struct ImportKey([u8; 32]);
impl fmt::Debug for ImportKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
impl ImportKey {
    /// Caller computes SHA-256 of validated content. No credentials/device identity as inputs.
    pub fn from_content(source: Uuid, camera: CameraId, utc: UtcRange, sha256: [u8; 32]) -> Self {
        let mut h = Sha256::new();
        h.update(b"ferrissight-import-v1");
        h.update(source.as_bytes());
        h.update(camera.to_string().as_bytes());
        for t in [utc.start(), utc.end()] {
            h.update(t.timestamp().to_be_bytes());
            h.update(t.timestamp_subsec_nanos().to_be_bytes());
        }
        h.update(sha256);
        Self(h.finalize().into())
    }
}
#[derive(Debug)]
pub struct ImportCheckpoint {
    pub operation_id: Uuid,
    pub recordings: Option<Vec<RecordingId>>,
}
pub struct ImportJournal {
    db: Connection,
}
impl ImportJournal {
    /// Dedicated private database; callers own permissions and single-worker coordination.
    pub fn new(db: Connection) -> Result<Self, SourceError> {
        db.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(|_| SourceError::Unavailable)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS import_checkpoints (
            source_key BLOB PRIMARY KEY CHECK(length(source_key)=32),
            operation_id TEXT NOT NULL UNIQUE, recordings TEXT);",
        )
        .map_err(|_| SourceError::Unavailable)?;
        Ok(Self { db })
    }
    /// Repeated discovery/restart returns the same operation. Pending never means imported.
    /// Publication manifests must carry this operation ID for crash reconciliation.
    pub fn reserve(&self, key: &ImportKey) -> Result<ImportCheckpoint, SourceError> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO import_checkpoints(source_key,operation_id) VALUES(?1,?2)",
                params![key.0.as_slice(), Uuid::new_v4().to_string()],
            )
            .map_err(|_| SourceError::Unavailable)?;
        let (id, recordings): (String, Option<String>) = self
            .db
            .query_row(
                "SELECT operation_id,recordings FROM import_checkpoints WHERE source_key=?1",
                [key.0.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| SourceError::Unavailable)?;
        Ok(ImportCheckpoint {
            operation_id: Uuid::parse_str(&id).map_err(|_| SourceError::Protocol)?,
            recordings: recordings
                .map(|s| serde_json::from_str(&s).map_err(|_| SourceError::Protocol))
                .transpose()?,
        })
    }
    /// Only after atomic media publication and verification. Repeating identical completion
    /// succeeds; a conflicting publication is rejected, not silently overwritten.
    pub fn complete(
        &mut self,
        key: &ImportKey,
        operation: Uuid,
        ids: &[RecordingId],
    ) -> Result<(), SourceError> {
        if ids.is_empty() || ids.len() > 256 {
            return Err(SourceError::InvalidRequest);
        }
        let encoded = serde_json::to_string(ids).map_err(|_| SourceError::Protocol)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| SourceError::Unavailable)?;
        let n=tx.execute("UPDATE import_checkpoints SET recordings=?3 WHERE source_key=?1 AND operation_id=?2 AND (recordings IS NULL OR recordings=?3)",params![key.0.as_slice(),operation.to_string(),encoded]).map_err(|_|SourceError::Unavailable)?;
        if n != 1 {
            return Err(SourceError::InvalidRequest);
        }
        tx.commit().map_err(|_| SourceError::Unavailable)
    }
}
