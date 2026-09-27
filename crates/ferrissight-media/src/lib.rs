//! Backend boundary for future go2rtc, FFmpeg, GStreamer or retina integration.
//! Codec implementation and backend process details belong outside the core.
use async_trait::async_trait;
use ferrissight_core::{CameraId, FerrisSightError, StreamEndpoint};

#[derive(Debug)]
pub struct MediaSource {
    pub camera_id: CameraId,
    pub endpoint: StreamEndpoint,
}
#[async_trait]
pub trait MediaStream: Send + Sync {
    async fn stop(&mut self) -> Result<(), FerrisSightError>;
}
#[async_trait]
pub trait MediaBackend: Send + Sync {
    async fn open(&self, source: MediaSource) -> Result<Box<dyn MediaStream>, FerrisSightError>;
}
