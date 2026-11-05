//! Local stalled upstreams; no process environment mutation or model accounts.

pub(super) async fn stalled_upstream(
    partial_body: bool,
) -> (
    String,
    tokio::sync::oneshot::Receiver<Vec<u8>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            stream.readable().await.unwrap();
            let mut buffer = [0; 4096];
            match stream.try_read(&mut buffer) {
                Ok(0) => panic!("client closed before sending headers"),
                Ok(count) => request.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("read fixture request: {error}"),
            }
            assert!(request.len() <= 64 * 1024);
        }
        if partial_body {
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nx";
            let mut written = 0;
            while written < response.len() {
                stream.writable().await.unwrap();
                match stream.try_write(&response[written..]) {
                    Ok(0) => panic!("fixture response made no progress"),
                    Ok(count) => written += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => panic!("write fixture response: {error}"),
                }
            }
        }
        started.send(request).unwrap();
        std::future::pending::<()>().await;
        drop(stream);
    });
    (format!("http://{address}"), ready, server)
}
