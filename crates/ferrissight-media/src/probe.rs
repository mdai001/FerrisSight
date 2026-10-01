//! Bounded RTSP-over-TCP probing. Frames are discarded without decoding or storage.
use ferrissight_core::{AudioCodec, StreamEndpoint, VideoCodec};
use futures_util::StreamExt;
use retina::{
    client::{PlayOptions, Session, SessionGroup, SessionOptions, SetupOptions},
    codec::{CodecItem, ParametersRef},
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tokio::time::{timeout, timeout_at, Instant};
use url::Url;

#[derive(Clone, Copy, Debug)]
pub struct ProbeOptions {
    pub duration: Duration,
    pub connect_timeout: Duration,
    pub stall_timeout: Duration,
    pub teardown_timeout: Duration,
}
impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(3),
            teardown_timeout: Duration::from_secs(5),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum ProbeEnd {
    Completed,
    Stalled,
    Disconnected,
    ProtocolError,
}
#[derive(Debug, Serialize)]
pub struct ProbeReport {
    pub codec: VideoCodec,
    /// Dimensions from codec parameters associated with received frames, not ONVIF config.
    pub resolution: Option<(u32, u32)>,
    pub declared_fps: Option<f64>,
    /// Received video frames / their RTP media-time span; not a decode benchmark.
    pub observed_fps: Option<f64>,
    pub video_frames: u64,
    pub audio_tracks: Vec<AudioCodec>,
    pub audio_frames: u64,
    pub readable: bool,
    pub end: ProbeEnd,
    pub clean_disconnect: bool,
}
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("invalid probe configuration")]
    InvalidConfiguration,
    #[error("RTSP connection failed")]
    Connection,
    #[error("RTSP connection timed out")]
    Timeout,
    #[error("no supported video stream")]
    NoVideo,
}

pub(crate) fn endpoint_url(endpoint: &StreamEndpoint) -> Result<Url, ProbeError> {
    if endpoint.scheme != "rtsp"
        || !endpoint.path.starts_with('/')
        || endpoint.host.contains(['/', '@', '?', '#'])
        || endpoint.port == 0
    {
        return Err(ProbeError::InvalidConfiguration);
    }
    let mut url =
        Url::parse("rtsp://example.invalid/").map_err(|_| ProbeError::InvalidConfiguration)?;
    url.set_host(Some(&endpoint.host))
        .map_err(|_| ProbeError::InvalidConfiguration)?;
    url.set_port(Some(endpoint.port))
        .map_err(|_| ProbeError::InvalidConfiguration)?;
    // The endpoint path may include a query supplied by ONVIF; never format it in reports.
    let (path, query) = endpoint
        .path
        .split_once('?')
        .map_or((endpoint.path.as_str(), None), |(p, q)| (p, Some(q)));
    if path.contains('#') {
        return Err(ProbeError::InvalidConfiguration);
    }
    url.set_path(path);
    url.set_query(query);
    Ok(url)
}
fn video_codec(name: &str) -> VideoCodec {
    match name {
        "h264" => VideoCodec::H264,
        "h265" => VideoCodec::H265,
        "jpeg" => VideoCodec::Mjpeg,
        _ => VideoCodec::Unknown,
    }
}
fn audio_codec(name: &str) -> AudioCodec {
    match name {
        "pcma" | "pcmu" => AudioCodec::G711,
        "mpeg4-generic" | "mp4a-latm" => AudioCodec::Aac,
        "opus" => AudioCodec::Opus,
        _ => AudioCodec::Unknown,
    }
}
#[derive(Default)]
struct FrameTiming {
    count: u64,
    first: Option<f64>,
    last: Option<f64>,
}
impl FrameTiming {
    fn observe(&mut self, seconds: f64) {
        self.count += 1;
        if seconds.is_finite() {
            self.first.get_or_insert(seconds);
            self.last = Some(seconds);
        }
    }
    fn fps(&self) -> Option<f64> {
        let span = self.last? - self.first?;
        (self.count > 1 && span > 0.0).then(|| (self.count - 1) as f64 / span)
    }
}

/// Credentials go through Retina's authentication options, never URL userinfo.
/// Caller must suppress dependency logs (the standalone example disables all logs).
/// Cancellation drops the session; normal completion also awaits bounded TEARDOWN.
pub async fn probe(
    endpoint: &StreamEndpoint,
    options: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    if options.duration.is_zero()
        || options.duration > Duration::from_secs(60)
        || options.connect_timeout.is_zero()
        || options.connect_timeout > Duration::from_secs(30)
        || options.stall_timeout.is_zero()
        || options.stall_timeout > Duration::from_secs(30)
        || options.teardown_timeout.is_zero()
        || options.teardown_timeout > Duration::from_secs(30)
    {
        return Err(ProbeError::InvalidConfiguration);
    }
    let url = endpoint_url(endpoint)?;
    let group = Arc::new(SessionGroup::default());
    let session_options = crate::rtsp::session_options(endpoint, group.clone());
    let result = probe_inner(url, session_options, options).await;
    let clean = matches!(
        timeout(options.teardown_timeout, group.await_teardown()).await,
        Ok(Ok(()))
    );
    result.map(|mut report| {
        report.clean_disconnect = clean;
        report
    })
}
async fn probe_inner(
    url: Url,
    session_options: SessionOptions,
    options: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    let mut demuxed = timeout(options.connect_timeout, async {
        let mut session = Session::describe(url, session_options)
            .await
            .map_err(|_| ProbeError::Connection)?;
        let indexes: Vec<_> = session
            .streams()
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.media(), "video" | "audio"))
            .map(|(i, _)| i)
            .take(16)
            .collect();
        if !indexes
            .iter()
            .any(|&i| session.streams()[i].media() == "video")
        {
            return Err(ProbeError::NoVideo);
        }
        for i in indexes {
            session
                .setup(i, SetupOptions::default())
                .await
                .map_err(|_| ProbeError::Connection)?;
        }
        session
            .play(PlayOptions::default())
            .await
            .map_err(|_| ProbeError::Connection)?
            .demuxed()
            .map_err(|_| ProbeError::Connection)
    })
    .await
    .map_err(|_| ProbeError::Timeout)??;
    let video_index = demuxed
        .streams()
        .iter()
        .position(|s| s.media() == "video")
        .ok_or(ProbeError::NoVideo)?;
    let video = &demuxed.streams()[video_index];
    let mut report = ProbeReport {
        codec: video_codec(video.encoding_name()),
        resolution: None,
        declared_fps: video
            .framerate()
            .map(f64::from)
            .filter(|f| f.is_finite() && *f > 0.0),
        observed_fps: None,
        video_frames: 0,
        audio_tracks: demuxed
            .streams()
            .iter()
            .filter(|s| s.media() == "audio")
            .map(|s| audio_codec(s.encoding_name()))
            .collect(),
        audio_frames: 0,
        readable: false,
        end: ProbeEnd::Completed,
        clean_disconnect: false,
    };
    let mut timing = FrameTiming::default();
    let started = Instant::now();
    let finish = started + options.duration;
    let mut last_video = started;
    loop {
        let deadline = finish.min(last_video + options.stall_timeout);
        match timeout_at(deadline, demuxed.next()).await {
            Err(_) => {
                if Instant::now() < finish {
                    report.end = ProbeEnd::Stalled;
                }
                break;
            }
            Ok(None) => {
                report.end = ProbeEnd::Disconnected;
                break;
            }
            Ok(Some(Err(_))) => {
                report.end = ProbeEnd::ProtocolError;
                break;
            }
            Ok(Some(Ok(CodecItem::VideoFrame(frame)))) if frame.stream_id() == video_index => {
                timing.observe(frame.timestamp().elapsed_secs());
                last_video = Instant::now();
                if let Some(ParametersRef::Video(p)) = demuxed.streams()[video_index].parameters() {
                    report.resolution = Some(p.pixel_dimensions());
                    if report.declared_fps.is_none() {
                        report.declared_fps = p.frame_rate().and_then(|(n, d)| {
                            (n > 0 && d > 0).then_some(f64::from(d) / f64::from(n))
                        });
                    }
                }
            }
            Ok(Some(Ok(CodecItem::AudioFrame(_)))) => {
                report.audio_frames += 1;
            }
            Ok(Some(Ok(_))) => {}
        }
        if Instant::now() >= finish {
            break;
        }
    }
    report.video_frames = timing.count;
    report.observed_fps = timing.fps();
    report.readable = report.end == ProbeEnd::Completed
        && timing.count > 1
        && last_video.elapsed() < options.stall_timeout;
    // Frames, SDP and transport buffers are dropped here; no sink or media files exist.
    drop(demuxed);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrissight_core::{CameraCredentials, SecretString};
    fn endpoint() -> StreamEndpoint {
        StreamEndpoint {
            scheme: "rtsp".into(),
            host: "192.0.2.10".into(),
            port: 554,
            path: "/synthetic".into(),
            credentials: CameraCredentials {
                username: SecretString::new("example-user".into()),
                password: SecretString::new("synthetic-test-secret".into()),
            },
        }
    }
    #[test]
    fn credential_bearing_endpoint_is_rejected_and_debug_redacts() {
        let mut e = endpoint();
        assert!(!format!("{e:?}").contains("synthetic-test-secret"));
        e.host = "example-user:synthetic-test-secret@example.invalid".into();
        assert!(matches!(
            endpoint_url(&e),
            Err(ProbeError::InvalidConfiguration)
        ));
    }
    #[test]
    fn frame_rate_uses_intervals_and_handles_degenerate_timestamps() {
        let mut timing = FrameTiming::default();
        timing.observe(10.0);
        assert_eq!(timing.fps(), None);
        timing.observe(10.0);
        assert_eq!(timing.fps(), None);
        let mut timing = FrameTiming::default();
        for i in 0..21 {
            timing.observe(10.0 + f64::from(i) / 20.0);
        }
        assert!((timing.fps().unwrap() - 20.0).abs() < 0.001);
    }
    #[tokio::test]
    async fn unresponsive_rtsp_peer_times_out_without_leaking_details() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let mut e = endpoint();
        e.host = "127.0.0.1".into();
        e.port = port;
        let options = ProbeOptions {
            connect_timeout: Duration::from_millis(50),
            ..Default::default()
        };
        let error = probe(&e, options).await.unwrap_err();
        assert!(matches!(error, ProbeError::Timeout));
        assert_eq!(error.to_string(), "RTSP connection timed out");
        peer.abort();
    }
    #[tokio::test]
    async fn idle_video_session_stalls_and_acknowledges_teardown() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
                let (headers,body)=match method {
                    "DESCRIBE" => ("Content-Type: application/sdp\r\n", "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=synthetic\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:trackID=0\r\n"),
                    "SETUP" => ("Session: synthetic-session;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n", ""),
                    "PLAY" | "TEARDOWN" => ("Session: synthetic-session\r\n", ""),
                    _ => panic!("unexpected test request"),
                };
                let response=format!("RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
                if method == "TEARDOWN" {
                    return;
                }
            }
        });
        let mut e = endpoint();
        e.host = "127.0.0.1".into();
        e.port = port;
        let options = ProbeOptions {
            duration: Duration::from_secs(1),
            stall_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let report = probe(&e, options).await.unwrap();
        assert_eq!(report.end, ProbeEnd::Stalled);
        assert!(!report.readable);
        assert!(report.clean_disconnect);
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
