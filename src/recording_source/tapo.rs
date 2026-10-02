//! Experimental protocol boundary only. No transport, authentication or decryption is implemented.
use super::*;
use chrono::NaiveDate;

#[derive(Debug)]
pub struct TapoRecordingCredentials {
    pub control: VendorRecordingCredentials,
    /// May differ from the ONVIF/RTSP Camera Account password.
    pub media_password: SecretString,
}
#[derive(Debug)]
pub enum AuthenticationState {
    Unauthenticated,
    Authenticated { token: SecretString },
    Expired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Download,
    Playback,
}
impl TransferMode {
    /// Deliberately narrower than upstream's fallback for any nonzero response code.
    pub fn fallback(self, error: SourceError, playback: Support) -> Option<Self> {
        if self == Self::Download
            && error == SourceError::UnsupportedMethod
            && playback != Support::Unsupported
        {
            Some(Self::Playback)
        } else {
            None
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ProtocolCapabilities {
    pub dates: Support,
    pub utc_ranges: Support,
    pub day_ranges: Support,
    pub download: Support,
    pub playback: Support,
    pub encrypted_media: Support,
}
/// Device calendar date; conversion requires a verified device timezone/DST policy.
#[derive(Clone, Copy, Debug)]
pub struct RecordingDate(pub NaiveDate);
#[derive(Debug)]
pub enum RecordingQuery {
    Dates {
        start: RecordingDate,
        end: RecordingDate,
    },
    Utc(RangeQuery),
    Day {
        date: RecordingDate,
        start_index: u32,
        page_size: u16,
    },
}
impl RecordingQuery {
    pub fn validate(&self) -> Result<(), SourceError> {
        match self {
            Self::Dates { start, end } if start.0 > end.0 => Err(SourceError::InvalidRequest),
            Self::Day {
                start_index,
                page_size,
                ..
            } if !(1..=256).contains(page_size)
                || start_index.checked_add(u32::from(*page_size)).is_none() =>
            {
                Err(SourceError::InvalidRequest)
            }
            _ => Ok(()),
        }
    }
}
#[derive(Debug)]
pub enum RecordingQueryResult {
    Dates(Vec<RecordingDate>),
    Ranges(RecordingPage),
}
/// Separate local media connection: implementation will own challenge/key exchange/framing.
/// Credentials and session details never cross into generic recording metadata.
#[async_trait]
pub trait LocalMediaSessionFactory: Send + Sync {
    async fn open(
        &self,
        credentials: &TapoRecordingCredentials,
        mode: TransferMode,
        cancel: &Cancellation,
    ) -> Result<Box<dyn RecordingMediaSession>, SourceError>;
}
pub struct TapoRecordingSource {
    camera_id: CameraId,
    _credentials: TapoRecordingCredentials,
    auth: AuthenticationState,
}
impl TapoRecordingSource {
    pub fn new(camera_id: CameraId, credentials: TapoRecordingCredentials) -> Self {
        Self {
            camera_id,
            _credentials: credentials,
            auth: AuthenticationState::Unauthenticated,
        }
    }
    pub fn authentication(&self) -> &AuthenticationState {
        &self.auth
    }
    pub fn protocol_capabilities(&self) -> ProtocolCapabilities {
        ProtocolCapabilities::default()
    }
    /// Validate the typed request but perform no protocol operation yet.
    pub async fn query(
        &self,
        query: &RecordingQuery,
        cancel: &Cancellation,
    ) -> Result<RecordingQueryResult, SourceError> {
        cancel.check()?;
        query.validate()?;
        Err(SourceError::NotImplemented)
    }
}
impl fmt::Debug for TapoRecordingSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TapoRecordingSource(<redacted>)")
    }
}
#[async_trait]
impl RecordingSource for TapoRecordingSource {
    async fn capabilities(
        &self,
        cancel: &Cancellation,
    ) -> Result<RecordingSourceCapabilities, SourceError> {
        cancel.check()?;
        Ok(RecordingSourceCapabilities::default())
    }
    async fn list_ranges(
        &self,
        _query: &RangeQuery,
        cancel: &Cancellation,
    ) -> Result<RecordingPage, SourceError> {
        cancel.check()?;
        Err(SourceError::NotImplemented)
    }
    async fn open_recording(
        &self,
        range: &RecordingRange,
        cancel: &Cancellation,
    ) -> Result<RecordingDownload, SourceError> {
        cancel.check()?;
        if range.camera_id != self.camera_id {
            return Err(SourceError::InvalidRequest);
        }
        Err(SourceError::NotImplemented)
    }
}
