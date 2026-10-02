//! Local recording metadata, retention and future explicit upload queues.
//! Cloud adapters require minimum OAuth scopes and protected token storage.
use crate::core::{CameraId, FerrisSightError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct RecordingId(Uuid);
impl RecordingId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}
/// Metadata intentionally excludes paths, endpoints, names and hardware identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingMetadata {
    pub camera_id: CameraId,
    pub started_at_unix_seconds: u64,
    pub duration_seconds: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingSegment {
    pub id: RecordingId,
    pub metadata: RecordingMetadata,
}
#[async_trait]
pub trait RecordingStore: Send + Sync {
    async fn metadata(&self, id: RecordingId)
        -> Result<Option<RecordingSegment>, FerrisSightError>;
    async fn save_metadata(&self, segment: RecordingSegment) -> Result<(), FerrisSightError>;
    /// Implementations must remove associated media as well as metadata.
    async fn delete(&self, id: RecordingId) -> Result<(), FerrisSightError>;
}

pub mod mp4;

pub mod local;
pub mod service;
