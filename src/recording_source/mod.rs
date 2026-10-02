//! Experimental recorded-media source/import contracts. No production synchronization.
use crate::{
    core::{AudioCodec, CameraId, SecretString, VideoCodec},
    storage::RecordingId,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::{fmt, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

pub mod tapo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("recording source not implemented")]
    NotImplemented,
    #[error("invalid recording request")]
    InvalidRequest,
    #[error("recording authentication rejected")]
    Authentication,
    #[error("recording session expired")]
    SessionExpired,
    #[error("recording operation unsupported")]
    UnsupportedMethod,
    #[error("recording source unavailable")]
    Unavailable,
    #[error("recording operation timed out")]
    Timeout,
    #[error("invalid recording protocol response")]
    Protocol,
    #[error("recording media decryption failed")]
    Decryption,
    #[error("recording no longer available")]
    Gone,
    #[error("recording operation cancelled")]
    Cancelled,
    #[error("recording resource limit exceeded")]
    ResourceLimit,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Support {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordingSourceCapabilities {
    pub utc_queries: Support,
    pub bounded_download: Support,
    pub cancellation: Support,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingKind {
    Continuous,
    Event,
    Unknown,
}

/// Runtime adapter locator only: never a CameraId, filename, log field or API value.
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueRecordingId(String);
impl OpaqueRecordingId {
    pub fn new(value: String) -> Result<Self, SourceError> {
        if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self(value))
    }
    /// Explicit exposure is permitted only inside an adapter request encoder.
    pub fn expose_for_protocol(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for OpaqueRecordingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}
impl UtcRange {
    /// Half-open UTC interval. Vendor clock correction must precede construction.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, SourceError> {
        if start.timestamp() < 0 || end <= start {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self { start, end })
    }
    pub fn start(self) -> DateTime<Utc> {
        self.start
    }
    pub fn end(self) -> DateTime<Utc> {
        self.end
    }
}
#[derive(Clone, Debug)]
pub struct RecordingRange {
    pub camera_id: CameraId,
    pub utc: UtcRange,
    pub kind: RecordingKind,
    pub source_id: Option<OpaqueRecordingId>,
}
/// Bounded query; opaque continuation is adapter-specific, not a vendor wire structure.
#[derive(Clone, Debug)]
pub struct RangeQuery {
    utc: UtcRange,
    page_size: u16,
    pub continuation: Option<OpaqueRecordingId>,
}
impl RangeQuery {
    pub fn new(utc: UtcRange, page_size: u16) -> Result<Self, SourceError> {
        if !(1..=256).contains(&page_size) || utc.end() - utc.start() > chrono::Duration::days(31) {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self {
            utc,
            page_size,
            continuation: None,
        })
    }
    pub fn utc(&self) -> UtcRange {
        self.utc
    }
    pub fn page_size(&self) -> u16 {
        self.page_size
    }
}
#[derive(Debug)]
pub struct RecordingPage {
    pub ranges: Vec<RecordingRange>,
    pub continuation: Option<OpaqueRecordingId>,
}
/// Distinct from ONVIF/RTSP CameraCredentials; no implicit conversion or serialization.
#[derive(Debug)]
pub struct VendorRecordingCredentials {
    pub username: SecretString,
    pub password: SecretString,
}

/// Sticky, cloneable per-operation cancellation; cancellation before subscription is retained.
#[derive(Clone, Debug)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub fn check(&self) -> Result<(), SourceError> {
        if self.is_cancelled() {
            Err(SourceError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaContainer {
    MpegTs,
    Elementary,
    Mp4,
    Unknown,
}
#[derive(Clone, Copy, Debug)]
pub struct RecordingMedia {
    pub container: MediaContainer,
    pub video: Option<VideoCodec>,
    /// None means not yet observed, not proof that the recording has no audio.
    pub audio: Option<AudioCodec>,
}
/// Media bytes must never be formatted, serialized to logs, or included in errors.
pub struct MediaChunk(Vec<u8>);
impl MediaChunk {
    pub const MAX_BYTES: usize = 1024 * 1024;
    pub fn new(bytes: Vec<u8>) -> Result<Self, SourceError> {
        if bytes.is_empty() {
            return Err(SourceError::Protocol);
        }
        if bytes.len() > Self::MAX_BYTES {
            return Err(SourceError::ResourceLimit);
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for MediaChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaChunk(<redacted>)")
    }
}
#[async_trait]
pub trait RecordingMediaSession: Send {
    /// One bounded decrypted chunk; None only for confirmed completion, never a timeout.
    /// Futures must be cancellation-safe. Drop releases sockets and aborts owned tasks.
    async fn next_chunk(&mut self) -> Result<Option<MediaChunk>, SourceError>;
    async fn close(&mut self) -> Result<(), SourceError>;
}
pub struct RecordingDownload {
    pub media: RecordingMedia,
    session: Box<dyn RecordingMediaSession>,
    cancellation: Cancellation,
    idle_timeout: Duration,
    terminal: Option<Result<(), SourceError>>,
}
impl RecordingDownload {
    pub fn new(
        media: RecordingMedia,
        session: Box<dyn RecordingMediaSession>,
        cancellation: Cancellation,
        idle_timeout: Duration,
    ) -> Result<Self, SourceError> {
        cancellation.check()?;
        if idle_timeout.is_zero() || idle_timeout > Duration::from_secs(300) {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self {
            media,
            session,
            cancellation,
            idle_timeout,
            terminal: None,
        })
    }
    pub async fn next_chunk(&mut self) -> Result<Option<MediaChunk>, SourceError> {
        if let Some(result) = self.terminal {
            return result.map(|()| None);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(SourceError::Cancelled),
            result = tokio::time::timeout(self.idle_timeout, self.session.next_chunk()) => result.unwrap_or(Err(SourceError::Timeout)),
        };
        match &result {
            Ok(None) => self.terminal = Some(Ok(())),
            Err(error) => self.terminal = Some(Err(*error)),
            Ok(Some(_)) => {}
        }
        result
    }
    /// Bounded explicit cleanup. Signals this operation token; use a fresh token per download.
    /// Ordinary drop only drops the owned session, without cancelling the caller token.
    pub async fn cancel(mut self) -> Result<(), SourceError> {
        self.cancellation.cancel();
        tokio::time::timeout(self.idle_timeout, self.session.close())
            .await
            .unwrap_or(Err(SourceError::Timeout))
    }
}
#[async_trait]
pub trait RecordingSource: Send + Sync {
    async fn capabilities(
        &self,
        cancel: &Cancellation,
    ) -> Result<RecordingSourceCapabilities, SourceError>;
    /// Return at most page_size overlapping ranges. Continuations must be scoped to
    /// this camera/query and exhausted in a caller-bounded loop; never fabricate UTC.
    async fn list_ranges(
        &self,
        query: &RangeQuery,
        cancel: &Cancellation,
    ) -> Result<RecordingPage, SourceError>;
    async fn open_recording(
        &self,
        range: &RecordingRange,
        cancel: &Cancellation,
    ) -> Result<RecordingDownload, SourceError>;
}
/// Future private import ledger input. A candidate match is not proof of identical bytes.
#[derive(Debug)]
pub struct ImportIdentity {
    pub source_instance_id: Uuid,
    pub range: RecordingRange,
    /// Optional digest supplied after validated content hashing; redacted like vendor IDs.
    pub fingerprint: Option<OpaqueRecordingId>,
}
#[derive(Debug)]
pub struct ImportedRecording {
    pub source: ImportIdentity,
    pub actual_utc: UtcRange,
    pub recording_ids: Vec<RecordingId>,
}
#[derive(Clone, Copy, Debug)]
pub enum ImportAudioPolicy {
    PreserveWithoutTranscoding,
    OmitExplicitly,
}
/// Future importer owns demux/remux, keyframe splitting, .partial publication and ledger commit.
/// No implementation, file writes, production scheduling or implicit transcoding in this phase.
#[async_trait]
pub trait RecordingImport: Send + Sync {
    async fn import(
        &self,
        identity: ImportIdentity,
        download: RecordingDownload,
        audio: ImportAudioPolicy,
    ) -> Result<ImportedRecording, SourceError>;
}
