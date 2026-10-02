//! Bounded, video-only RTSP recording. Dependency diagnostics must be suppressed by callers.
use crate::core::{CameraId, StreamEndpoint};
use crate::storage::mp4::{
    H264Config, Mp4Segments, RecordingError, SegmentReport, UtcMinuteTarget, VideoSample,
};
use futures_util::StreamExt;
use retina::{
    client::{PlayOptions, Session, SessionGroup, SessionOptions, SetupOptions, Transport},
    codec::{CodecItem, ParametersRef, VideoParametersCodec},
};
use serde::Serialize;
use std::{
    future::Future,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, Semaphore},
    time::{timeout, timeout_at, Instant},
};

#[derive(Clone, Copy)]
pub struct RecordingOptions {
    pub duration: Duration,
    pub segment_seconds: u32,
}
impl Default for RecordingOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(120),
            segment_seconds: 30,
        }
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub enum RecordingEnd {
    Completed,
    Shutdown,
    Disconnected,
    Stalled,
    PacketLoss,
    ParametersChanged,
    InvalidTimestamp,
    ProtocolError,
    StorageError,
}
#[derive(Debug, Serialize)]
pub struct RecordingReport {
    pub camera_id: CameraId,
    pub codec: &'static str,
    pub resolution: (u16, u16),
    pub end: RecordingEnd,
    pub received_frames: u64,
    pub discarded_before_keyframe: u64,
    pub segments: Vec<SegmentReport>,
    pub clean_disconnect: bool,
    pub keepalive: crate::media::rtsp::KeepaliveReport,
    pub timing: SessionTiming,
}
/// Gateway observations only; UTC mapping is not a claim of sensor capture time.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SessionTiming {
    pub setup_succeeded: bool,
    pub setup_millis: u64,
    pub teardown_millis: u64,
    pub clean_teardown: bool,
    pub first_received_unix_ms: Option<i64>,
    pub last_received_unix_ms: Option<i64>,
}
pub(crate) fn unix_millis() -> Result<i64, RecordError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or(RecordError::Configuration)
}
pub(crate) struct MinuteContext {
    pub window_start_unix_ms: i64,
    pub deadline: Instant,
    pub transport: Transport,
    pub group: Arc<SessionGroup>,
    pub storage_slots: Arc<Semaphore>,
    pub rotate_utc_minutes: bool,
}
pub(crate) struct RecordingAttempt {
    pub result: Result<RecordingReport, RecordError>,
    pub timing: SessionTiming,
}
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("invalid recording configuration")]
    Configuration,
    #[error("RTSP recording connection failed")]
    Connection,
    #[error("RTSP recording connection timed out")]
    Timeout,
    #[error("recording requires H.264 video parameters")]
    Unsupported,
    #[error("recording worker failed")]
    Worker,
    #[error("recording ended before usable video: {0:?}")]
    NoFrames(RecordingEnd),
    #[error(transparent)]
    Storage(#[from] RecordingError),
}
fn parameters(stream: &retina::client::Stream) -> Result<H264Config, RecordError> {
    let Some(ParametersRef::Video(p)) = stream.parameters() else {
        return Err(RecordError::Unsupported);
    };
    let VideoParametersCodec::H264 { sps, pps } = p.codec_params() else {
        return Err(RecordError::Unsupported);
    };
    let (width, height) = p.pixel_dimensions();
    Ok(H264Config {
        width: width.try_into().map_err(|_| RecordError::Unsupported)?,
        height: height.try_into().map_err(|_| RecordError::Unsupported)?,
        sps: sps.to_vec(),
        pps: pps.to_vec(),
        timescale: 90_000,
    })
}
/// Graceful cancellation is an explicit future: dropping this future is not a finalized shutdown.
/// RTSP errors end this bounded run; no automatic reconnect or recording across timestamp resets.
pub async fn record<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    shutdown: F,
) -> Result<RecordingReport, RecordError> {
    record_attempt(endpoint, camera_id, directory, options, None, shutdown)
        .await
        .result
}

pub(crate) async fn record_attempt<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    context: Option<MinuteContext>,
    shutdown: F,
) -> RecordingAttempt {
    let mut timing = SessionTiming::default();
    let result = record_attempt_inner(
        endpoint,
        camera_id,
        directory,
        options,
        context,
        shutdown,
        &mut timing,
    )
    .await;
    RecordingAttempt { result, timing }
}

#[allow(clippy::too_many_arguments)]
async fn record_attempt_inner<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    context: Option<MinuteContext>,
    shutdown: F,
    timing: &mut SessionTiming,
) -> Result<RecordingReport, RecordError> {
    if options.duration.is_zero()
        || options.duration > Duration::from_secs(600)
        || options.segment_seconds == 0
        || options.segment_seconds > 60
    {
        return Err(RecordError::Configuration);
    }
    let url =
        crate::media::probe::endpoint_url(endpoint).map_err(|_| RecordError::Configuration)?;
    let group = context
        .as_ref()
        .map(|c| c.group.clone())
        .unwrap_or_else(|| Arc::new(SessionGroup::default()));
    let session_options = crate::media::rtsp::session_options(endpoint, group.clone());
    let result = record_inner(
        url,
        session_options,
        camera_id,
        directory,
        options,
        context,
        shutdown,
        timing,
    )
    .await;
    let teardown_start = Instant::now();
    let clean = matches!(
        timeout(Duration::from_secs(5), group.await_teardown()).await,
        Ok(Ok(()))
    );
    timing.teardown_millis = teardown_start.elapsed().as_millis() as u64;
    timing.clean_teardown = clean;
    result.map(|mut report| {
        report.clean_disconnect = clean;
        report.timing = timing.clone();
        report
    })
}
#[allow(clippy::too_many_arguments)]
async fn record_inner<F: Future<Output = ()>>(
    url: url::Url,
    session_options: SessionOptions,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    context: Option<MinuteContext>,
    shutdown: F,
    timing: &mut SessionTiming,
) -> Result<RecordingReport, RecordError> {
    let setup_start = Instant::now();
    let setup_timeout = context
        .as_ref()
        .map(|c| c.deadline.saturating_duration_since(setup_start))
        .unwrap_or(Duration::from_secs(5))
        .min(Duration::from_secs(5));
    let setup_result = timeout(setup_timeout, async {
        let mut session = Session::describe(url, session_options)
            .await
            .map_err(|_| RecordError::Connection)?;
        let index = session
            .streams()
            .iter()
            .position(|s| s.media() == "video" && s.encoding_name() == "h264")
            .ok_or(RecordError::Unsupported)?;
        for stream_index in 0..session.streams().len().min(16) {
            if stream_index == index || session.streams()[stream_index].media() == "audio" {
                session
                    .setup(
                        stream_index,
                        SetupOptions::default().transport(
                            context
                                .as_ref()
                                .map(|c| c.transport.clone())
                                .unwrap_or_default(),
                        ),
                    )
                    .await
                    .map_err(|_| RecordError::Connection)?;
            }
        }
        let demuxed = session
            .play(PlayOptions::default())
            .await
            .map_err(|_| RecordError::Connection)?
            .demuxed()
            .map_err(|_| RecordError::Connection)?;
        Ok::<_, RecordError>((demuxed, index))
    })
    .await;
    timing.setup_millis = setup_start.elapsed().as_millis() as u64;
    let (mut demuxed, index) = setup_result.map_err(|_| RecordError::Timeout)??;
    timing.setup_succeeded = true;
    let finish = context
        .as_ref()
        .map(|c| c.deadline)
        .unwrap_or_else(|| Instant::now() + options.duration);
    let mut last_video = Instant::now();
    let mut config: Option<H264Config> = None;
    let mut sender = None;
    let mut worker = None;
    let mut received = 0;
    let mut end = RecordingEnd::Completed;
    tokio::pin!(shutdown);
    loop {
        let item = tokio::select! {
            _=&mut shutdown=>{end=RecordingEnd::Shutdown;break},
            item=timeout_at(finish.min(last_video+Duration::from_secs(3)),demuxed.next())=>item,
        };
        let frame = match item {
            Err(_) => {
                if Instant::now() < finish {
                    end = RecordingEnd::Stalled
                };
                break;
            }
            Ok(None) => {
                end = RecordingEnd::Disconnected;
                break;
            }
            Ok(Some(Err(_))) => {
                end = RecordingEnd::ProtocolError;
                break;
            }
            Ok(Some(Ok(CodecItem::VideoFrame(frame)))) if frame.stream_id() == index => frame,
            _ => continue,
        };
        last_video = Instant::now();
        let received_utc = unix_millis()?;
        timing.first_received_unix_ms.get_or_insert(received_utc);
        timing.last_received_unix_ms = Some(received_utc);
        received += 1;
        if frame.loss() != 0 {
            end = RecordingEnd::PacketLoss;
            break;
        }
        if frame.timestamp().clock_rate().get() != 90_000 {
            end = RecordingEnd::InvalidTimestamp;
            break;
        }
        let current = match parameters(&demuxed.streams()[index]) {
            Ok(p) => p,
            Err(_) => {
                end = RecordingEnd::ParametersChanged;
                break;
            }
        };
        if config.as_ref().is_some_and(|c| c != &current) {
            end = RecordingEnd::ParametersChanged;
            break;
        }
        if config.is_none() {
            let terminal_duration = match demuxed.streams()[index].parameters() {
                Some(ParametersRef::Video(p)) => p
                    .frame_rate()
                    .and_then(|(n, d)| (d > 0).then(|| u64::from(n) * 90_000 / u64::from(d)))
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(4500),
                _ => 4500,
            };
            let path = directory.to_path_buf();
            let cfg = current.clone();
            let utc_target = context.as_ref().map(|c| UtcMinuteTarget {
                camera_id,
                window_start_unix_ms: c.window_start_unix_ms,
                anchor_unix_ms: received_utc,
                anchor_rtp_ticks: frame.timestamp().elapsed(),
            });
            let rotate_utc_minutes = context.as_ref().is_some_and(|c| c.rotate_utc_minutes);
            let (tx, mut rx) = mpsc::channel::<VideoSample>(16);
            let storage_permit = match context.as_ref() {
                Some(c) => Some(
                    c.storage_slots
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| RecordError::Worker)?,
                ),
                None => None,
            };
            worker = Some(tokio::task::spawn_blocking(move || {
                let _storage_permit = storage_permit;
                let mut sink = match utc_target {
                    Some(target) if rotate_utc_minutes => {
                        Mp4Segments::new_utc_minutes(&path, cfg, terminal_duration, target)?
                    }
                    Some(target) => {
                        Mp4Segments::new_utc_minute(&path, cfg, terminal_duration, target)?
                    }
                    None => {
                        Mp4Segments::new(&path, cfg, options.segment_seconds, terminal_duration)?
                    }
                };
                while let Some(sample) = rx.blocking_recv() {
                    sink.push(sample)?
                }
                sink.finish()
            }));
            sender = Some(tx);
            config = Some(current);
        }
        let sample = VideoSample {
            timestamp: frame.timestamp().elapsed(),
            keyframe: frame.is_random_access_point(),
            data: frame.into_data(),
        };
        // Bounded queue and wait: slow storage terminates instead of unbounded memory or dropped frames.
        if !matches!(
            timeout(
                Duration::from_secs(3),
                sender.as_ref().unwrap().send(sample)
            )
            .await,
            Ok(Ok(()))
        ) {
            end = RecordingEnd::StorageError;
            break;
        }
        if Instant::now() >= finish {
            break;
        }
    }
    let keepalive = demuxed.keepalive_stats().into();
    drop(demuxed);
    drop(sender);
    let config = match config {
        Some(c) => c,
        None => return Err(RecordError::NoFrames(end)),
    };
    let worker = worker.ok_or(RecordError::Worker)?;
    let result = if context.is_some() {
        timeout(Duration::from_secs(5), worker)
            .await
            .map_err(|_| RecordError::Worker)?
    } else {
        worker.await
    };
    let (segments, discarded_before_keyframe) = match result.map_err(|_| RecordError::Worker)? {
        Ok(result) => result,
        Err(RecordingError::Timestamp) => {
            return Err(RecordError::Storage(RecordingError::Timestamp))
        }
        Err(e) => return Err(e.into()),
    };
    Ok(RecordingReport {
        camera_id,
        codec: "H264",
        resolution: (config.width, config.height),
        end,
        received_frames: received,
        discarded_before_keyframe,
        segments,
        clean_disconnect: false,
        keepalive,
        timing: timing.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CameraCredentials, SecretString};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn idle_video_session_reports_stall_and_acknowledges_teardown() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            loop {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let text = std::str::from_utf8(&request).unwrap();
                let sequence = text
                    .lines()
                    .find_map(|line| line.strip_prefix("CSeq: "))
                    .unwrap();
                let method = text.split_whitespace().next().unwrap();
                let (headers, body) = match method {
                    "DESCRIBE" => (
                        "Content-Type: application/sdp\r\n",
                        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=synthetic\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:trackID=0\r\n",
                    ),
                    "SETUP" => (
                        "Session: synthetic-session;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n",
                        "",
                    ),
                    "PLAY" | "TEARDOWN" => ("Session: synthetic-session\r\n", ""),
                    _ => panic!("unexpected test request"),
                };
                let response = format!(
                    "RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                if method == "TEARDOWN" {
                    return;
                }
            }
        });

        let endpoint = StreamEndpoint {
            scheme: "rtsp".into(),
            host: "127.0.0.1".into(),
            port,
            path: "/synthetic".into(),
            credentials: CameraCredentials {
                username: SecretString::new("example-user".into()),
                password: SecretString::new("synthetic-test-secret".into()),
            },
        };
        let directory = std::env::temp_dir().join(format!(
            "ferrissight-recording-test-{:?}",
            CameraId::generate()
        ));
        let result = record(
            &endpoint,
            CameraId::generate(),
            &directory,
            RecordingOptions {
                duration: Duration::from_secs(5),
                segment_seconds: 30,
            },
            std::future::pending(),
        )
        .await;

        assert!(matches!(
            result,
            Err(RecordError::NoFrames(RecordingEnd::Stalled))
        ));
        assert!(!directory.exists());
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

/// A bounded run may restart once. Each session has independent MP4 files and RTP time origin.
#[derive(Debug, Serialize)]
pub struct RecordingRunReport {
    pub sessions: Vec<RecordingReport>,
    pub reconnect_attempts: u32,
    pub completed: bool,
    /// Only fixed, redacted FerrisSight errors; never dependency error strings.
    pub terminal_error: Option<String>,
}
/// One restart after a transport, stall or protocol failure, with one-second backoff.
/// Failed sessions remain explicit in the report; segments are never joined across RTP resets.
/// Packet loss, changed parameters, timestamp and storage failures are not retried.
pub async fn record_reconnecting<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    shutdown: F,
) -> Result<RecordingRunReport, RecordError> {
    if options.duration.is_zero() || options.duration > Duration::from_secs(600) {
        return Err(RecordError::Configuration);
    }
    let finish = Instant::now() + options.duration;
    tokio::pin!(shutdown);
    let mut run = RecordingRunReport {
        sessions: Vec::new(),
        reconnect_attempts: 0,
        completed: false,
        terminal_error: None,
    };
    for attempt in 0..=1 {
        let remaining = finish.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if attempt > 0 {
            run.reconnect_attempts += 1;
        }
        let result = record(
            endpoint,
            camera_id,
            directory,
            RecordingOptions {
                duration: remaining,
                ..options
            },
            &mut shutdown,
        )
        .await;
        let retry = match result {
            Ok(report) => {
                let retry = matches!(
                    report.end,
                    RecordingEnd::Disconnected
                        | RecordingEnd::Stalled
                        | RecordingEnd::ProtocolError
                );
                run.completed =
                    matches!(report.end, RecordingEnd::Completed | RecordingEnd::Shutdown)
                        && report.clean_disconnect
                        && !report.segments.is_empty();
                run.terminal_error = None;
                run.sessions.push(report);
                retry
            }
            Err(error)
                if attempt == 0
                    && matches!(
                        error,
                        RecordError::Connection
                            | RecordError::Timeout
                            | RecordError::NoFrames(
                                RecordingEnd::Stalled
                                    | RecordingEnd::Disconnected
                                    | RecordingEnd::ProtocolError
                            )
                    ) =>
            {
                run.terminal_error = Some(error.to_string());
                true
            }
            Err(error) if run.sessions.is_empty() => return Err(error),
            Err(error) => {
                run.terminal_error = Some(error.to_string());
                false
            }
        };
        if !retry || attempt == 1 {
            break;
        }
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if Instant::now() >= finish {
            break;
        }
    }
    Ok(run)
}
