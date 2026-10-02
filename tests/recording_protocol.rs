use chrono::DateTime;
use ferrissight::{
    core::CameraId,
    recording_source::{protocol::*, query, *},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn wire(body: &[u8], kind: &str, extra: &str) -> Vec<u8> {
    let mut bytes = format!("--synthetic\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nX-If-Encrypt: 0\r\n{extra}\r\n", body.len()).into_bytes();
    bytes.extend(body);
    bytes.extend(b"\r\n");
    bytes
}
fn part(body: &[u8], kind: &str, extra: &str) -> Part {
    MultipartDecoder::new("synthetic")
        .unwrap()
        .feed(&wire(body, kind, extra))
        .unwrap()
        .remove(0)
}
fn opened(router: &mut StreamRouter) {
    assert!(matches!(
        router.accept(part(
            br#"{"type":"response","seq":7,"params":{"error_code":0,"session_id":42}}"#,
            "application/json",
            ""
        )),
        Ok(StreamEvent::Opened)
    ));
}
#[test]
fn every_split_preserves_exact_media_and_redacts_payload() {
    let bytes = wire(
        b"synthetic-private-media",
        "video/mp2t",
        "X-Session-Id: 42\r\nX-Data-Sequence: 1\r\n",
    );
    for split in 0..=bytes.len() {
        let mut parser = MultipartDecoder::new("synthetic").unwrap();
        let mut results = parser.feed(&bytes[..split]).unwrap();
        results.extend(parser.feed(&bytes[split..]).unwrap());
        assert_eq!(results.len(), 1);
        assert_eq!(format!("{:?}", results[0]), "Part(<redacted>)");
        let mut router = StreamRouter::new(7);
        opened(&mut router);
        let StreamEvent::Media(chunk) = router.accept(results.remove(0)).unwrap() else {
            panic!("expected media");
        };
        assert_eq!(chunk.as_bytes(), b"synthetic-private-media");
        parser.finish().unwrap();
        assert_eq!(router.finish(), Err(SourceError::Protocol));
    }
}
#[test]
fn malformed_lengths_headers_and_limits_fail_closed() {
    let headers = [
        "Content-Length: -1",
        "Content-Length: 18446744073709551616",
        "Content-Length: 1\r\ncontent-length: 1",
        "Content-Length: 0",
        "Content-Length: 1\r\n folded-header",
        "Content-Length: 1\r\nX-If-Encrypt: 1",
    ];
    for header in headers {
        let bytes = format!(
            "--synthetic\r\nContent-Type: video/mp2t\r\nX-If-Encrypt: 0\r\n{header}\r\n\r\nx\r\n"
        );
        let mut parser = MultipartDecoder::new("synthetic").unwrap();
        assert!(parser.feed(bytes.as_bytes()).is_err());
        assert_eq!(parser.buffered_bytes(), 0);
        assert!(parser.feed(b"").is_err());
    }
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    let huge = format!(
        "--synthetic\r\nContent-Type: video/mp2t\r\nContent-Length: {}\r\nX-If-Encrypt: 0\r\n\r\n",
        MAX_PART + 1
    );
    assert!(matches!(
        parser.feed(huge.as_bytes()),
        Err(SourceError::ResourceLimit)
    ));
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    assert!(matches!(
        parser.feed(&vec![0; MAX_PART + MAX_HEADER + 129]),
        Err(SourceError::ResourceLimit)
    ));
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    let oversized = format!("--synthetic\r\n{}", "x".repeat(MAX_HEADER + 1));
    assert!(matches!(
        parser.feed(oversized.as_bytes()),
        Err(SourceError::ResourceLimit)
    ));
}
#[test]
fn truncation_boundary_confusion_and_encryption_are_rejected() {
    let bytes = wire(b"synthetic", "video/mp2t", "");
    for end in 1..bytes.len() {
        let mut parser = MultipartDecoder::new("synthetic").unwrap();
        parser.feed(&bytes[..end]).unwrap();
        assert!(parser.finish().is_err());
    }
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    assert!(parser.feed(b"--wrong\r\n").is_err());
    let encrypted = String::from_utf8(wire(b"synthetic", "video/mp2t", ""))
        .unwrap()
        .replace("X-If-Encrypt: 0", "X-If-Encrypt: 1");
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    let p = parser.feed(encrypted.as_bytes()).unwrap().remove(0);
    assert!(matches!(
        StreamRouter::new(7).accept(p),
        Err(SourceError::Decryption)
    ));
}
#[test]
fn session_sequence_and_explicit_completion_are_required() {
    for extra in [
        "X-Session-Id: 99\r\nX-Data-Sequence: 1\r\n",
        "X-Data-Sequence: 1\r\n",
    ] {
        let mut router = StreamRouter::new(7);
        opened(&mut router);
        assert!(router.accept(part(b"media", "video/mp2t", extra)).is_err());
    }
    let mut router = StreamRouter::new(7);
    opened(&mut router);
    router
        .accept(part(
            b"media",
            "video/mp2t",
            "X-Session-Id: 42\r\nX-Data-Sequence: 1\r\n",
        ))
        .unwrap();
    assert!(router
        .accept(part(
            b"media",
            "video/mp2t",
            "X-Session-Id: 42\r\nX-Data-Sequence: 3\r\n"
        ))
        .is_err());
    let finish=br#"{"type":"notification","params":{"event_type":"stream_status","status":"finished","session_id":42}}"#;
    let mut router = StreamRouter::new(7);
    opened(&mut router);
    assert!(matches!(
        router.accept(part(finish, "application/json", "")),
        Ok(StreamEvent::Complete)
    ));
    router.finish().unwrap();
    assert!(router.accept(part(finish, "application/json", "")).is_err());
    let mut router = StreamRouter::new(8);
    assert!(router
        .accept(part(
            br#"{"type":"response","seq":7,"params":{"error_code":0,"session_id":42}}"#,
            "application/json",
            ""
        ))
        .is_err());
}
#[test]
fn bounded_queries_validate_utc_and_do_not_guess_kind() {
    assert_eq!(
        query::dates(br#"["20200102","20200101","20200101"]"#)
            .unwrap()
            .len(),
        2
    );
    assert!(query::dates(br#"["20200230"]"#).is_err());
    let range = UtcRange::new(
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(120, 0).unwrap(),
    )
    .unwrap();
    let q = RangeQuery::new(range, 2).unwrap();
    let camera = CameraId::generate();
    let data=br#"[{"search_video_results_1":{"startTime":0,"endTime":60,"vedio_type":"2","private_field":"synthetic-secret"}}]"#;
    let ranges = query::ranges(data, camera, &q, 1).unwrap();
    assert_eq!(ranges[0].utc.start().timestamp(), 1);
    assert_eq!(ranges[0].kind, RecordingKind::Unknown);
    assert!(!format!("{ranges:?}").contains("synthetic-secret"));
    assert!(query::ranges(data, camera, &q, i64::MAX).is_err());
    assert!(query::ranges(br#"[{"x":{"startTime":60,"endTime":0}}]"#, camera, &q, 0).is_err());
    assert!(query::ranges(
        br#"[{"x":{"startTime":"local-time","endTime":60}}]"#,
        camera,
        &q,
        0
    )
    .is_err());
    assert!(query::ranges(br#"[{"x":{"startTime":180,"endTime":200}}]"#, camera, &q, 0).is_err());
    assert!(query::ranges(b"[]", camera, &q, 0).unwrap().is_empty());
    let id = OpaqueRecordingId::new("synthetic-cursor".into()).unwrap();
    let mut guard = query::PaginationGuard::new(2, 4).unwrap();
    guard.observe(2, Some(&id)).unwrap();
    assert_eq!(guard.observe(2, Some(&id)), Err(SourceError::Protocol));
    let mut guard = query::PaginationGuard::new(1, 1).unwrap();
    assert_eq!(guard.observe(2, None), Err(SourceError::ResourceLimit));
}
#[tokio::test]
async fn synthetic_server_fragmentation_and_abrupt_disconnect() {
    let (mut server, mut client) = tokio::io::duplex(32);
    let bytes = wire(
        b"synthetic",
        "video/mp2t",
        "X-Session-Id: 42\r\nX-Data-Sequence: 1\r\n",
    );
    let writer = tokio::spawn(async move {
        for byte in bytes {
            server.write_all(&[byte]).await.unwrap();
        }
    });
    let mut parser = MultipartDecoder::new("synthetic").unwrap();
    let mut router = StreamRouter::new(7);
    opened(&mut router);
    let mut buf = [0; 13];
    let mut chunks = 0;
    loop {
        let n = client.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        for p in parser.feed(&buf[..n]).unwrap() {
            router.accept(p).unwrap();
            chunks += 1;
        }
    }
    writer.await.unwrap();
    assert_eq!(chunks, 1);
    parser.finish().unwrap();
    assert!(router.finish().is_err());
}
struct Steady;
#[async_trait::async_trait]
impl RecordingMediaSession for Steady {
    async fn next_chunk(&mut self) -> Result<Option<MediaChunk>, SourceError> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        Ok(Some(MediaChunk::new(vec![1])?))
    }
    async fn close(&mut self) -> Result<(), SourceError> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn steady_sender_cannot_extend_total_deadline_or_cleanup() {
    let mut stream = RecordingDownload::with_deadline(
        RecordingMedia {
            container: MediaContainer::MpegTs,
            video: None,
            audio: None,
        },
        Box::new(Steady),
        Cancellation::default(),
        Duration::from_secs(1),
        Duration::from_millis(30),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match stream.next_chunk().await {
                Ok(Some(_)) => {}
                Err(SourceError::Timeout) => break,
                _ => panic!("unexpected completion"),
            }
        }
        assert!(matches!(
            stream.next_chunk().await,
            Err(SourceError::Timeout)
        ));
        assert_eq!(stream.cancel().await, Err(SourceError::Timeout));
    })
    .await
    .unwrap();
}
#[test]
fn deterministic_arbitrary_bytes_never_panic_or_exceed_buffer_budget() {
    let mut seed = 7u64;
    for size in 0..2048 {
        let mut parser = MultipartDecoder::new("synthetic").unwrap();
        let bytes: Vec<_> = (0..size)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as u8
            })
            .collect();
        let _ = parser.feed(&bytes);
        assert!(parser.buffered_bytes() <= MAX_PART + MAX_HEADER + 128);
        let _ = query::dates(&bytes);
    }
}

#[test]
fn ambiguous_completion_and_duplicate_json_fields_are_rejected() {
    let mut router = StreamRouter::new(7);
    assert!(router
        .accept(part(
            br#"{"type":"response","seq":8,"seq":7,"params":{"error_code":0,"session_id":42}}"#,
            "application/json",
            ""
        ))
        .is_err());
    for finish in [
        br#"{"type":"notification","params":{"event_type":"stream_status","status":"finished"}}"#.as_slice(),
        br#"{"type":"notification","params":{"event_type":"stream_status","status":"finished","session_id":42,"session_id":99}}"#.as_slice(),
    ] {
        let mut router=StreamRouter::new(7); opened(&mut router);
        assert!(router.accept(part(finish,"application/json","")).is_err());
        assert!(router.finish().is_err());
    }
}
#[test]
fn payload_boundary_bytes_and_closing_delimiter_do_not_confuse_parser() {
    let mut bytes = wire(b"body\r\n--synthetic\r\ninside", "video/mp2t", "");
    bytes.extend(b"--synthetic--\r\n");
    let mut decoder = MultipartDecoder::new("synthetic").unwrap();
    let mut count = 0;
    for byte in bytes {
        count += decoder.feed(&[byte]).unwrap().len();
    }
    assert_eq!(count, 1);
    decoder.finish().unwrap();
    assert!(decoder.feed(b"unexpected").is_err());
}
#[test]
fn same_bounds_are_not_proof_that_two_discovered_recordings_are_identical() {
    let utc = UtcRange::new(
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(120, 0).unwrap(),
    )
    .unwrap();
    let data=br#"[{"a":{"startTime":0,"endTime":60,"vedio_type":"1"}},{"b":{"startTime":0,"endTime":60,"vedio_type":"2"}}]"#;
    assert_eq!(
        query::ranges(
            data,
            CameraId::generate(),
            &RangeQuery::new(utc, 2).unwrap(),
            0
        )
        .unwrap()
        .len(),
        2
    );
}
#[test]
fn repeated_authoritative_timestamp_is_not_silently_overwritten() {
    let utc = UtcRange::new(
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(120, 0).unwrap(),
    )
    .unwrap();
    let data = br#"[{"a":{"startTime":0,"startTime":30,"endTime":60}}]"#;
    assert!(query::ranges(
        data,
        CameraId::generate(),
        &RangeQuery::new(utc, 2).unwrap(),
        0
    )
    .is_err());
}
