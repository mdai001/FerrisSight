use async_trait::async_trait;
use chrono::{DateTime, NaiveDate};
use ferrissight::{
    core::{CameraId, SecretString},
    recording_source::{tapo::*, *},
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
fn utc() -> UtcRange {
    UtcRange::new(
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(60, 0).unwrap(),
    )
    .unwrap()
}
fn credentials() -> TapoRecordingCredentials {
    TapoRecordingCredentials {
        control: VendorRecordingCredentials {
            username: SecretString::new("example-user".into()),
            password: SecretString::new("synthetic-control".into()),
        },
        media_password: SecretString::new("synthetic-media".into()),
    }
}
#[test]
fn utc_and_query_bounds_are_validated() {
    let epoch = utc().start();
    let long_range = UtcRange::new(epoch, epoch + chrono::Duration::days(32)).unwrap();
    assert!(RangeQuery::new(long_range, 10).is_err());
    assert_eq!(
        UtcRange::new(epoch, epoch),
        Err(SourceError::InvalidRequest)
    );
    assert_eq!(
        UtcRange::new(utc().end(), epoch),
        Err(SourceError::InvalidRequest)
    );
    assert_eq!(
        UtcRange::new(DateTime::from_timestamp(-1, 0).unwrap(), epoch),
        Err(SourceError::InvalidRequest)
    );
    for size in [0, 257, u16::MAX] {
        assert!(RangeQuery::new(utc(), size).is_err());
    }
    assert_eq!(RangeQuery::new(utc(), 256).unwrap().page_size(), 256);
    let date = RecordingDate(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
    assert!(RecordingQuery::Day {
        date,
        start_index: u32::MAX,
        page_size: 1
    }
    .validate()
    .is_err());
    assert!(RecordingQuery::Day {
        date,
        start_index: 0,
        page_size: 0
    }
    .validate()
    .is_err());
    let next = RecordingDate(NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
    assert!(RecordingQuery::Dates {
        start: next,
        end: date
    }
    .validate()
    .is_err());
}
#[test]
fn sensitive_ids_credentials_and_media_are_redacted() {
    let id = OpaqueRecordingId::new("synthetic-vendor-id".into()).unwrap();
    assert_eq!(format!("{id:?}"), "<redacted>");
    assert_eq!(id.expose_for_protocol(), "synthetic-vendor-id");
    for value in [
        String::new(),
        "x".repeat(1025),
        "synthetic\nidentifier".into(),
    ] {
        assert!(OpaqueRecordingId::new(value).is_err());
    }
    let rendered = format!("{:?}", credentials());
    for value in ["example-user", "synthetic-control", "synthetic-media"] {
        assert!(!rendered.contains(value));
    }
    let state = AuthenticationState::Authenticated {
        token: SecretString::new("synthetic-token".into()),
    };
    assert!(!format!("{state:?}").contains("synthetic-token"));
    assert_eq!(
        format!(
            "{:?}",
            MediaChunk::new(b"synthetic-private-media".to_vec()).unwrap()
        ),
        "MediaChunk(<redacted>)"
    );
    assert!(MediaChunk::new(vec![]).is_err());
    assert!(MediaChunk::new(vec![0; MediaChunk::MAX_BYTES + 1]).is_err());
}
#[test]
fn fallback_is_only_for_explicit_unsupported_method_and_only_once() {
    assert_eq!(
        TransferMode::Download.fallback(SourceError::UnsupportedMethod, Support::Unknown),
        Some(TransferMode::Playback)
    );
    assert_eq!(
        TransferMode::Download.fallback(SourceError::UnsupportedMethod, Support::Unsupported),
        None
    );
    assert_eq!(
        TransferMode::Playback.fallback(SourceError::UnsupportedMethod, Support::Supported),
        None
    );
    for error in [
        SourceError::Authentication,
        SourceError::SessionExpired,
        SourceError::Timeout,
        SourceError::Protocol,
        SourceError::Decryption,
        SourceError::Cancelled,
        SourceError::Gone,
        SourceError::Unavailable,
        SourceError::ResourceLimit,
        SourceError::InvalidRequest,
        SourceError::NotImplemented,
    ] {
        assert_eq!(
            TransferMode::Download.fallback(error, Support::Supported),
            None
        );
    }
}
#[tokio::test]
async fn cancellation_is_sticky_for_existing_and_future_waiters() {
    let token = Cancellation::default();
    let copy = token.clone();
    let task = tokio::spawn(async move {
        copy.cancelled().await;
    });
    token.cancel();
    token.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .unwrap();
    assert_eq!(token.check(), Err(SourceError::Cancelled));
}
#[tokio::test]
async fn skeleton_is_inert_and_does_not_claim_device_support() {
    let camera = CameraId::generate();
    let source = TapoRecordingSource::new(camera, credentials());
    let cancel = Cancellation::default();
    assert!(matches!(
        source.authentication(),
        AuthenticationState::Unauthenticated
    ));
    assert_eq!(
        source.capabilities(&cancel).await.unwrap().utc_queries,
        Support::Unknown
    );
    assert!(matches!(
        source
            .list_ranges(&RangeQuery::new(utc(), 10).unwrap(), &cancel)
            .await,
        Err(SourceError::NotImplemented)
    ));
    let mut range = RecordingRange {
        camera_id: camera,
        utc: utc(),
        kind: RecordingKind::Unknown,
        source_id: None,
    };
    assert!(matches!(
        source.open_recording(&range, &cancel).await,
        Err(SourceError::NotImplemented)
    ));
    range.camera_id = CameraId::generate();
    assert!(matches!(
        source.open_recording(&range, &cancel).await,
        Err(SourceError::InvalidRequest)
    ));
    cancel.cancel();
    assert!(matches!(
        source.open_recording(&range, &cancel).await,
        Err(SourceError::Cancelled)
    ));
}
struct Session {
    started: Option<tokio::sync::oneshot::Sender<()>>,
    chunks: VecDeque<Vec<u8>>,
    stall: bool,
    closed: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl RecordingMediaSession for Session {
    async fn next_chunk(&mut self) -> Result<Option<MediaChunk>, SourceError> {
        if let Some(started) = self.started.take() {
            let _ = started.send(());
        }
        if self.stall {
            std::future::pending::<()>().await;
        }
        self.chunks.pop_front().map(MediaChunk::new).transpose()
    }
    async fn close(&mut self) -> Result<(), SourceError> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}
fn download(
    stall: bool,
    token: Cancellation,
) -> (RecordingDownload, Arc<AtomicBool>, Arc<AtomicBool>) {
    let closed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let session = Session {
        started: None,
        chunks: VecDeque::from([vec![1, 2], vec![3]]),
        stall,
        closed: closed.clone(),
        dropped: dropped.clone(),
    };
    (
        RecordingDownload::new(
            RecordingMedia {
                container: MediaContainer::MpegTs,
                video: None,
                audio: None,
            },
            Box::new(session),
            token,
            Duration::from_millis(20),
        )
        .unwrap(),
        closed,
        dropped,
    )
}
#[tokio::test]
async fn media_is_incremental_and_confirmed_eof_is_stable() {
    let token = Cancellation::default();
    let (mut stream, _, dropped) = download(false, token.clone());
    assert_eq!(
        stream.next_chunk().await.unwrap().unwrap().as_bytes(),
        &[1, 2]
    );
    assert_eq!(stream.next_chunk().await.unwrap().unwrap().as_bytes(), &[3]);
    assert!(stream.next_chunk().await.unwrap().is_none());
    assert!(stream.next_chunk().await.unwrap().is_none());
    drop(stream);
    assert!(!token.is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn timeout_never_turns_into_successful_eof() {
    let (mut stream, _, _) = download(true, Cancellation::default());
    for _ in 0..2 {
        assert!(matches!(
            stream.next_chunk().await,
            Err(SourceError::Timeout)
        ));
    }
}
#[tokio::test]
async fn cancellation_interrupts_pending_read_and_closes_session() {
    let token = Cancellation::default();
    let (started, observed) = tokio::sync::oneshot::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let mut stream = RecordingDownload::new(
        RecordingMedia {
            container: MediaContainer::MpegTs,
            video: None,
            audio: None,
        },
        Box::new(Session {
            started: Some(started),
            chunks: VecDeque::new(),
            stall: true,
            closed: closed.clone(),
            dropped: dropped.clone(),
        }),
        token.clone(),
        Duration::from_secs(1),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        assert!(matches!(
            stream.next_chunk().await,
            Err(SourceError::Cancelled)
        ));
        stream.cancel().await.unwrap();
    });
    observed.await.unwrap();
    token.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(closed.load(Ordering::SeqCst));
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_after_confirmed_eof_does_not_change_completion() {
    let token = Cancellation::default();
    let (mut stream, _, _) = download(false, token.clone());
    while stream.next_chunk().await.unwrap().is_some() {}
    token.cancel();
    assert!(stream.next_chunk().await.unwrap().is_none());
}
