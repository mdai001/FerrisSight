use ferrissight::{
    core::{CameraCredentials, CameraId, SecretString, StreamEndpoint},
    media::persistent::{record_persistent, PersistentRecordingOptions},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn persistent_stalled_session_recovers_without_resetting_overall_deadline() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            loop {
                let mut request = vec![];
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
                    "DESCRIBE" => ("Content-Type: application/sdp\r\n",
                        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=synthetic\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=control:trackID=0\r\n"),
                    "SETUP" => ("Session: synthetic-session;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n", ""),
                    "PLAY" | "TEARDOWN" => ("Session: synthetic-session\r\n", ""),
                    _ => panic!("unexpected synthetic request"),
                };
                socket.write_all(format!("RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                if method == "TEARDOWN" {
                    break;
                }
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
    let camera = CameraId::generate();
    let directory = std::env::temp_dir().join(format!("ferrissight-test-{camera}"));
    let started = tokio::time::Instant::now();
    let report = record_persistent(
        &endpoint,
        camera,
        &directory,
        PersistentRecordingOptions {
            duration: Duration::from_secs(7),
            transport: Default::default(),
        },
        std::future::pending(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(report.setup_attempts, 2);
    assert_eq!(report.setup_successes, 2);
    assert_eq!(report.teardown_successes, 2);
    assert_eq!(report.reconnect_attempts, 1);
    assert!(report
        .attempts
        .iter()
        .all(|a| a.error.as_deref() == Some("recording ended before usable video: Stalled")));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(report.missing_media_seconds, 7.0);
    assert!(!directory.exists());
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}
