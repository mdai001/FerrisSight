use std::{io, sync::Arc, time::Duration};

use futures_util::StreamExt;
use retina::client::{
    KeepalivePolicy, PacketItem, PlayOptions, Session, SessionGroup, SessionOptions, SetupOptions,
    TeardownPolicy,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use url::Url;

const SESSION: &str = "synthetic-session";
const SDP: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=synthetic\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:96 H264/90000\r\na=control:trackID=0\r\n";

#[derive(Clone, Copy)]
enum Scenario {
    StaleSetRejection,
    ExactGetRejection,
    GetParameterSuccess,
    OptionsOnly,
    OptionsOnlyWrongCseq,
    MatchedSetUnauthorized,
    WrongCseqSuccess,
    UnknownSetRejection,
    MissingSetRejection,
    FailedOptions,
    AutoStaleSetRejection,
}

impl Scenario {
    fn policy(self) -> KeepalivePolicy {
        match self {
            Self::AutoStaleSetRejection => KeepalivePolicy::Auto,
            Self::OptionsOnly | Self::OptionsOnlyWrongCseq => KeepalivePolicy::OptionsOnly,
            _ => KeepalivePolicy::Adaptive,
        }
    }
    fn method(self) -> &'static str {
        if matches!(self, Self::ExactGetRejection | Self::GetParameterSuccess) {
            "GET_PARAMETER"
        } else {
            "SET_PARAMETER"
        }
    }
    fn expected_error(self) -> bool {
        !matches!(
            self,
            Self::StaleSetRejection
                | Self::ExactGetRejection
                | Self::GetParameterSuccess
                | Self::OptionsOnly
        )
    }
}

async fn read_request(stream: &mut TcpStream) -> io::Result<(String, String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let mut buf = [0; 1024];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended",
            ));
        }
        bytes.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = head.lines();
    let method = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let cseq = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("CSeq"))
        .map(|(_, value)| value.trim().to_owned())
        .unwrap_or_default();
    let content_length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let mut buf = [0; 1024];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request body ended",
            ));
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    Ok((
        method,
        cseq,
        bytes[header_end..header_end + content_length].to_vec(),
    ))
}

async fn response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    cseq: Option<&str>,
    extra: &str,
    body: &[u8],
) -> io::Result<()> {
    let cseq = cseq.map_or(String::new(), |n| format!("CSeq: {n}\r\n"));
    let head = format!(
        "RTSP/1.0 {status} {reason}\r\n{cseq}{extra}Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await
}

async fn expect(stream: &mut TcpStream, method: &str) -> io::Result<(String, Vec<u8>)> {
    let (got, cseq, body) = read_request(stream).await?;
    assert_eq!(got, method);
    Ok((cseq, body))
}

async fn serve(mut stream: TcpStream, scenario: Scenario) -> io::Result<()> {
    let (cseq, _) = expect(&mut stream, "DESCRIBE").await?;
    response(
        &mut stream,
        200,
        "OK",
        Some(&cseq),
        "Content-Type: application/sdp\r\nContent-Base: rtsp://127.0.0.1/stream/\r\n",
        SDP.as_bytes(),
    )
    .await?;
    let (cseq, _) = expect(&mut stream, "SETUP").await?;
    response(
        &mut stream,
        200,
        "OK",
        Some(&cseq),
        &format!(
            "Session: {SESSION};timeout=2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n"
        ),
        b"",
    )
    .await?;
    let (cseq, _) = expect(&mut stream, "PLAY").await?;
    response(
        &mut stream,
        200,
        "OK",
        Some(&cseq),
        &format!("Session: {SESSION}\r\n"),
        b"",
    )
    .await?;

    let (previous_cseq, _) = expect(&mut stream, "OPTIONS").await?;
    if matches!(scenario, Scenario::FailedOptions) {
        response(
            &mut stream,
            400,
            "Bad Request",
            Some(&previous_cseq),
            "",
            b"",
        )
        .await?;
    } else {
        response(
            &mut stream,
            200,
            "OK",
            Some(&previous_cseq),
            if matches!(scenario, Scenario::ExactGetRejection) {
                "Public: GET_PARAMETER\r\n"
            } else if matches!(
                scenario,
                Scenario::OptionsOnly | Scenario::GetParameterSuccess
            ) {
                "Public: SET_PARAMETER, GET_PARAMETER\r\n"
            } else {
                "Public: SET_PARAMETER\r\n"
            },
            b"",
        )
        .await?;
        if matches!(
            scenario,
            Scenario::OptionsOnly | Scenario::OptionsOnlyWrongCseq
        ) {
            let (second_cseq, _) = expect(&mut stream, "OPTIONS").await?;
            let response_cseq = if matches!(scenario, Scenario::OptionsOnlyWrongCseq) {
                previous_cseq.as_str()
            } else {
                second_cseq.as_str()
            };
            response(
                &mut stream,
                200,
                "OK",
                Some(response_cseq),
                "Public: SET_PARAMETER, GET_PARAMETER\r\n",
                b"",
            )
            .await?;
            if matches!(scenario, Scenario::OptionsOnly) {
                stream
                    .write_all(b"$\x00\x00\x0d\x80\x60\x00\x01\x00\x00\x00\x01\x12\x34\x56\x78\x65")
                    .await?;
                let (cseq, _) = expect(&mut stream, "TEARDOWN").await?;
                response(
                    &mut stream,
                    200,
                    "OK",
                    Some(&cseq),
                    &format!("Session: {SESSION}\r\n"),
                    b"",
                )
                .await?;
                return Ok(());
            }
            return Ok(());
        }

        let (cseq, _) = expect(&mut stream, scenario.method()).await?;
        match scenario {
            Scenario::StaleSetRejection | Scenario::AutoStaleSetRejection => {
                response(
                    &mut stream,
                    400,
                    "Bad Request",
                    Some(&previous_cseq),
                    "",
                    b"",
                )
                .await?;
            }
            Scenario::ExactGetRejection => {
                response(&mut stream, 501, "Not Implemented", Some(&cseq), "", b"").await?;
            }
            Scenario::GetParameterSuccess => {
                response(&mut stream, 200, "OK", Some(&cseq), "", b"").await?;
            }
            Scenario::MatchedSetUnauthorized => {
                response(&mut stream, 401, "Unauthorized", Some(&cseq), "", b"").await?;
            }
            Scenario::WrongCseqSuccess => {
                response(&mut stream, 200, "OK", Some(&previous_cseq), "", b"").await?;
            }
            Scenario::UnknownSetRejection => {
                response(&mut stream, 400, "Bad Request", Some("9999"), "", b"").await?;
            }
            Scenario::MissingSetRejection => {
                response(&mut stream, 400, "Bad Request", None, "", b"").await?;
            }
            Scenario::FailedOptions => unreachable!(),
            Scenario::OptionsOnly | Scenario::OptionsOnlyWrongCseq => unreachable!(),
        }
    }

    if matches!(
        scenario,
        Scenario::StaleSetRejection | Scenario::ExactGetRejection | Scenario::GetParameterSuccess
    ) {
        if !matches!(scenario, Scenario::GetParameterSuccess) {
            let (options_cseq, _) = expect(&mut stream, "OPTIONS").await?;
            response(
                &mut stream,
                200,
                "OK",
                Some(&options_cseq),
                "Public: SET_PARAMETER, GET_PARAMETER\r\n",
                b"",
            )
            .await?;
            // The fallback is latched despite OPTIONS re-advertising parameter methods.
            let (latched_cseq, _) = expect(&mut stream, "OPTIONS").await?;
            response(
                &mut stream,
                200,
                "OK",
                Some(&latched_cseq),
                "Public: SET_PARAMETER, GET_PARAMETER\r\n",
                b"",
            )
            .await?;
        }
        // Valid single-packet RTP on interleaved channel 0.
        stream
            .write_all(b"$\x00\x00\x0d\x80\x60\x00\x01\x00\x00\x00\x01\x12\x34\x56\x78\x65")
            .await?;
        let (cseq, _) = expect(&mut stream, "TEARDOWN").await?;
        response(
            &mut stream,
            200,
            "OK",
            Some(&cseq),
            &format!("Session: {SESSION}\r\n"),
            b"",
        )
        .await?;
    } else {
        // Dropping a playing session with Always teardown may still write TEARDOWN after its
        // stream returns an error. Acknowledge it if emitted, while allowing the task to end.
        if let Ok(Ok((method, cseq, _))) =
            timeout(Duration::from_secs(2), read_request(&mut stream)).await
        {
            if method == "TEARDOWN" {
                response(
                    &mut stream,
                    200,
                    "OK",
                    Some(&cseq),
                    &format!("Session: {SESSION}\r\n"),
                    b"",
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn run_case(scenario: Scenario) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        serve(stream, scenario).await
    });

    let group = Arc::new(SessionGroup::default());
    let options = SessionOptions::default()
        .session_group(group.clone())
        .teardown(TeardownPolicy::Always)
        .keepalive_policy(scenario.policy());
    let url = Url::parse(&format!("rtsp://{addr}/stream")).unwrap();
    let mut described = timeout(Duration::from_secs(3), Session::describe(url, options))
        .await
        .unwrap()
        .unwrap();
    described.setup(0, SetupOptions::default()).await.unwrap();
    let mut playing = described.play(PlayOptions::default()).await.unwrap();
    assert_eq!(playing.streams().len(), 1);

    if scenario.expected_error() {
        let item = timeout(Duration::from_secs(7), playing.next())
            .await
            .unwrap();
        assert!(matches!(item, Some(Err(_))));
    } else {
        let item = timeout(Duration::from_secs(7), playing.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(item, PacketItem::Rtp(_)));
        let stats = playing.keepalive_stats();
        if matches!(scenario, Scenario::OptionsOnly) {
            assert_eq!(stats.fallbacks, 0);
            assert_eq!(stats.set_parameter_succeeded, 0);
            assert_eq!(stats.get_parameter_succeeded, 0);
            assert_eq!(stats.options_succeeded, 2);
        } else if matches!(scenario, Scenario::GetParameterSuccess) {
            assert_eq!(stats.fallbacks, 0);
            assert_eq!(stats.get_parameter_succeeded, 1);
            assert_eq!(stats.set_parameter_succeeded, 0);
        } else {
            assert_eq!(stats.fallbacks, 1);
            assert_eq!(
                stats.malformed_fallbacks,
                u64::from(matches!(scenario, Scenario::StaleSetRejection))
            );
        }
    }

    drop(playing);
    let _ = timeout(Duration::from_secs(3), group.await_teardown()).await;
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn adaptive_keepalive_falls_back_only_for_supported_rejections() {
    for scenario in [
        Scenario::StaleSetRejection,
        Scenario::ExactGetRejection,
        Scenario::GetParameterSuccess,
        Scenario::OptionsOnly,
        Scenario::OptionsOnlyWrongCseq,
        Scenario::MatchedSetUnauthorized,
        Scenario::WrongCseqSuccess,
        Scenario::UnknownSetRejection,
        Scenario::MissingSetRejection,
        Scenario::FailedOptions,
        Scenario::AutoStaleSetRejection,
    ] {
        run_case(scenario).await;
    }
}
