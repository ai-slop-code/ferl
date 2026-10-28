use std::{collections::BTreeMap, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    time::{sleep, timeout},
};
use uuid::Uuid;

pub struct ServingProcess {
    child: Child,
    pub address: std::net::SocketAddr,
}

pub struct HttpResult {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl ServingProcess {
    pub async fn start(database_url: &str, cursor_key: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve a local process-test port");
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut child = Command::new(env!("CARGO_BIN_EXE_ferl-server"));
        child
            .env_clear()
            .env("FERL_DATABASE_URL", database_url)
            .env("FERL_PUBLIC_BASE_URL", "https://short.example.test")
            .env("FERL_CURSOR_KEY", cursor_key)
            .env("FERL_LISTEN_ADDR", address.to_string())
            .env("FERL_DB_POOL_MAX", "4")
            .env("FERL_DB_ACQUIRE_TIMEOUT_SECS", "2")
            .env("FERL_DB_LOCK_TIMEOUT_SECS", "10")
            .env("FERL_HTTP_MAX_IN_FLIGHT", "32")
            .env("FERL_HTTP_MAX_WAITING", "16")
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = child.spawn().expect("ferl-server process starts");
        let mut process = Self { child, address };
        process.wait_until_ready().await;
        process
    }

    async fn wait_until_ready(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().expect("child status is readable") {
                panic!("serving process exited before readiness: {status}");
            }
            if matches!(
                timeout(
                    Duration::from_secs(1),
                    send(self.address, "GET", "/api/health/ready", None, ""),
                )
                .await,
                Ok(Ok(response)) if response.status == 200
            ) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "serving process did not become ready before the deadline"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn stop(mut self) {
        if self
            .child
            .try_wait()
            .expect("child status is readable")
            .is_none()
        {
            self.child
                .start_kill()
                .expect("serving child accepts termination");
        }
        timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("serving child is reaped before its deadline")
            .expect("serving child exit is collected");
    }
}

pub async fn send(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: &str,
) -> std::io::Result<HttpResult> {
    timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(credential) = credential {
            request.push_str(&format!("Authorization: Bearer {credential}\r\n"));
        }
        if !body.is_empty() {
            request.push_str("Content-Type: application/json\r\n");
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).await?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await?;
        parse_response(&bytes)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP test deadline exceeded"))?
}

fn parse_response(bytes: &[u8]) -> std::io::Result<HttpResult> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("HTTP response is missing its header boundary"))?;
    let head = std::str::from_utf8(&bytes[..split])
        .map_err(|_| std::io::Error::other("HTTP response headers are not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| std::io::Error::other("HTTP status line is missing"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("HTTP status is invalid"))?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Ok(HttpResult {
        status,
        headers,
        body: bytes[split + 4..].to_vec(),
    })
}

pub fn json(response: &HttpResult) -> serde_json::Value {
    serde_json::from_slice(&response.body).expect("HTTP response body is JSON")
}

pub fn assert_headers(response: &HttpResult) {
    assert_eq!(
        response.headers.get("cache-control").map(String::as_str),
        Some("no-store")
    );
    let request_id = response
        .headers
        .get("x-request-id")
        .expect("every response has a server request ID");
    Uuid::parse_str(request_id).expect("request ID is a UUID");
    if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&response.body)
        && let Some(body_id) = body.get("request_id").and_then(serde_json::Value::as_str)
    {
        assert_eq!(request_id, body_id);
    }
}
