//! Agent-specific replay protocol bridges and resume transport validation.

pub(crate) mod claude;
pub(crate) mod claude_resume;
pub(crate) mod codex;
pub(crate) mod opencode;

// Buffering preserves agent compatibility; responses are limited to 64 MiB.
async fn read_response_limited(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    const LIMIT: usize = 64 * 1024 * 1024;
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= LIMIT as u64),
        "bridge response too large"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= LIMIT,
            "bridge response too large"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oversized_upstream_is_rejected_before_buffering() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Consume the request before closing, otherwise unread bytes can
            // reset the connection before the client receives the response.
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 67108865\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let error = read_response_limited(response).await.unwrap_err();
        assert_eq!(error.to_string(), "bridge response too large");
        worker.join().unwrap();
    }
}
