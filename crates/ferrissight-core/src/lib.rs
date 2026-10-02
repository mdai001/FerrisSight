use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// No Display or Serialize implementation. Exposure requires an explicit call.
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Use only at the authentication boundary; never pass this value to logs.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug)]
pub struct CameraCredentials {
    pub username: SecretString,
    pub password: SecretString,
}

/// Runtime-only endpoint. Never serialize this into recording metadata.
/// Construct components separately; host/path must not contain URL userinfo.
pub struct StreamEndpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub credentials: CameraCredentials,
}

impl fmt::Debug for StreamEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamEndpoint(<redacted>)")
    }
}

/// Random application identity, never derived from network or hardware identity.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CameraId(Uuid);

impl fmt::Display for CameraId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl CameraId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CameraCapabilities {
    pub main_stream: bool,
    pub sub_stream: bool,
    pub audio_input: bool,
    pub audio_output: bool,
    pub ptz: bool,
    pub motion_events: bool,
    pub person_events: bool,
    pub vehicle_events: bool,
}

/// Names are sensitive local presentation data, excluded from API summaries.
#[derive(Clone, Serialize, Deserialize)]
pub struct CameraInfo {
    pub id: CameraId,
    pub name: String,
}
impl fmt::Debug for CameraInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CameraInfo")
            .field("id", &self.id)
            .field("name", &"<redacted>")
            .finish()
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Mjpeg,
    Unknown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    G711,
    Opus,
    Unknown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CameraStatus {
    Unknown,
    Online,
    Offline,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamProfile {
    pub index: u32,
    pub video_codec: VideoCodec,
    pub audio_codec: Option<AudioCodec>,
    pub width: u32,
    pub height: u32,
}
/// Fixed errors intentionally omit raw transport errors, URLs, and paths.
#[derive(Debug, thiserror::Error)]
pub enum FerrisSightError {
    #[error("operation not implemented")]
    NotImplemented,
    #[error("camera unavailable")]
    Unavailable,
    #[error("invalid configuration")]
    InvalidConfiguration,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_and_endpoint_debug_are_redacted() {
        let credentials = CameraCredentials {
            username: SecretString::new("example-user".into()),
            password: SecretString::new("synthetic-test-secret".into()),
        };
        assert_eq!(format!("{:?}", credentials.password), "<redacted>");
        assert_eq!(
            format!("{credentials:?}"),
            "CameraCredentials { username: <redacted>, password: <redacted> }"
        );
        let endpoint = StreamEndpoint {
            scheme: "rtsp".into(),
            host: "192.0.2.10".into(),
            port: 554,
            path: "/synthetic".into(),
            credentials,
        };
        assert_eq!(format!("{endpoint:?}"), "StreamEndpoint(<redacted>)");
    }
    #[test]
    fn identities_and_capabilities_round_trip() {
        let id = CameraId::generate();
        assert_ne!(id, CameraId::generate());
        assert_eq!(id.0.get_version_num(), 4);
        assert_eq!(
            serde_json::from_str::<CameraId>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
        let capabilities = CameraCapabilities {
            ptz: true,
            ..Default::default()
        };
        assert_eq!(
            serde_json::from_str::<CameraCapabilities>(
                &serde_json::to_string(&capabilities).unwrap()
            )
            .unwrap(),
            capabilities
        );
        let info = CameraInfo {
            id,
            name: "camera-test-001".into(),
        };
        assert!(!format!("{info:?}").contains("camera-test-001"));
    }
}
