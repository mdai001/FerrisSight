//! Vendor-neutral camera boundaries; protocol adapters implement these traits.
use crate::core::{
    CameraCapabilities, CameraInfo, CameraStatus, FerrisSightError, StreamEndpoint, StreamProfile,
};
use async_trait::async_trait;

#[async_trait]
pub trait CameraProvider: Send + Sync {
    /// Discovery results are local runtime state, never telemetry.
    async fn discover(&self) -> Result<Vec<CameraInfo>, FerrisSightError>;
    async fn connect(
        &self,
        camera: CameraInfo,
        endpoint: StreamEndpoint,
    ) -> Result<Box<dyn CameraDevice>, FerrisSightError>;
}
#[async_trait]
pub trait CameraDevice: Send + Sync {
    async fn device_info(&self) -> Result<CameraInfo, FerrisSightError>;
    async fn capabilities(&self) -> Result<CameraCapabilities, FerrisSightError>;
    async fn stream_profiles(&self) -> Result<Vec<StreamProfile>, FerrisSightError>;
    /// Credentials remain separate; do not return a credential-bearing URI string.
    async fn stream_uri(&self, profile_index: u32) -> Result<StreamEndpoint, FerrisSightError>;
    async fn health(&self) -> Result<CameraStatus, FerrisSightError>;
}
