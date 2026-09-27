use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn listener_serves_health_and_shuts_down() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(ferrissight_server::serve(listener, async {
        let _ = stopped.await;
    }));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /health HTTP/1.1\r\nHost: example.invalid\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.ends_with(r#"{"status":"ok","service":"ferrissight"}"#));
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    })
    .await
    .expect("server smoke test timed out");
}
