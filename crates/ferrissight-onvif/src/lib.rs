//! Future WS-Discovery, device information, capabilities, profiles, stream URI,
//! PTZ and event probes. No SOAP or network discovery is implemented in Phase 0.
use async_trait::async_trait;
use ferrissight_camera::{CameraDevice, CameraProvider};
use ferrissight_core::{CameraInfo, FerrisSightError, StreamEndpoint};

#[derive(Default)]
pub struct OnvifProvider;
#[async_trait]
impl CameraProvider for OnvifProvider {
    async fn discover(&self) -> Result<Vec<CameraInfo>, FerrisSightError> {
        Err(FerrisSightError::NotImplemented)
    }
    async fn connect(
        &self,
        _camera: CameraInfo,
        _endpoint: StreamEndpoint,
    ) -> Result<Box<dyn CameraDevice>, FerrisSightError> {
        Err(FerrisSightError::NotImplemented)
    }
}
