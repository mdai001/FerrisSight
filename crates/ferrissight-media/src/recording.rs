//! Bounded, video-only RTSP recording. Dependency diagnostics must be suppressed by callers.
use ferrissight_core::{CameraId, StreamEndpoint};
use ferrissight_storage::mp4::{
    H264Config, Mp4Segments, RecordingError, SegmentReport, VideoSample,
};
use futures_util::StreamExt;
use retina::{
    client::{
        Credentials, PlayOptions, Session, SessionGroup, SessionOptions, SetupOptions,
        TeardownPolicy,
    },
    codec::{CodecItem, ParametersRef, VideoParametersCodec},
};
use serde::Serialize;
use std::{future::Future, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::mpsc,
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
    if options.duration.is_zero()
        || options.duration > Duration::from_secs(300)
        || options.segment_seconds == 0
        || options.segment_seconds > 60
    {
        return Err(RecordError::Configuration);
    }
    let url = crate::probe::endpoint_url(endpoint).map_err(|_| RecordError::Configuration)?;
    let group = Arc::new(SessionGroup::default());
    let session_options = SessionOptions::default()
        .session_group(group.clone())
        .teardown(TeardownPolicy::Auto)
        .creds(Some(Credentials {
            username: endpoint.credentials.username.expose_secret().into(),
            password: endpoint.credentials.password.expose_secret().into(),
        }));
    let result = record_inner(
        url,
        session_options,
        camera_id,
        directory,
        options,
        shutdown,
    )
    .await;
    let clean = matches!(
        timeout(Duration::from_secs(5), group.await_teardown()).await,
        Ok(Ok(()))
    );
    result.map(|mut report| {
        report.clean_disconnect = clean;
        report
    })
}
async fn record_inner<F: Future<Output = ()>>(
    url: url::Url,
    session_options: SessionOptions,
    camera_id: CameraId,
    directory: &Path,
    options: RecordingOptions,
    shutdown: F,
) -> Result<RecordingReport, RecordError> {
    let (mut demuxed, index) = timeout(Duration::from_secs(5), async {
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
                    .setup(stream_index, SetupOptions::default())
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
    .await
    .map_err(|_| RecordError::Timeout)??;
    let finish = Instant::now() + options.duration;
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
            let (tx, mut rx) = mpsc::channel::<VideoSample>(16);
            worker = Some(tokio::task::spawn_blocking(move || {
                let mut sink =
                    Mp4Segments::new(&path, cfg, options.segment_seconds, terminal_duration)?;
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
    drop(demuxed);
    drop(sender);
    let config = match config {
        Some(c) => c,
        None => return Err(RecordError::NoFrames(end)),
    };
    let (segments, discarded_before_keyframe) = match worker
        .ok_or(RecordError::Worker)?
        .await
        .map_err(|_| RecordError::Worker)?
    {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrissight_core::{CameraCredentials, SecretString};
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
