use std::sync::{Arc, Mutex};
use std::{collections::BTreeMap, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;

pub struct ServingProcess {
    child: Child,
    pub address: std::net::SocketAddr,
    signalled_at: Option<tokio::time::Instant>,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    stdout_shared: Arc<Mutex<Vec<u8>>>,
    stderr_shared: Arc<Mutex<Vec<u8>>>,
}

pub struct HttpResult {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[allow(dead_code)]
pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
    pub shutdown_elapsed: Duration,
    pub exit_success: bool,
}

impl ServingProcess {
    #[allow(dead_code)]
    pub async fn start(database_url: &str, cursor_key: &str) -> Self {
        Self::start_with(database_url, cursor_key, &[]).await
    }

    pub async fn start_with(database_url: &str, cursor_key: &str, extra: &[(&str, &str)]) -> Self {
        Self::start_binary_with(
            env!("CARGO_BIN_EXE_ferl-server"),
            database_url,
            cursor_key,
            extra,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn start_binary_with(
        binary: &str,
        database_url: &str,
        cursor_key: &str,
        extra: &[(&str, &str)],
    ) -> Self {
        Self::start_binary(binary, database_url, cursor_key, extra, true).await
    }

    #[allow(dead_code)]
    pub async fn start_binary_without_readiness(
        binary: &str,
        database_url: &str,
        cursor_key: &str,
        extra: &[(&str, &str)],
    ) -> Self {
        Self::start_binary(binary, database_url, cursor_key, extra, false).await
    }

    async fn start_binary(
        binary: &str,
        database_url: &str,
        cursor_key: &str,
        extra: &[(&str, &str)],
        wait_for_ready: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve a local process-test port");
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut child = Command::new(binary);
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in extra {
            child.env(name, value);
        }
        let child = child.spawn().expect("ferl-server process starts");
        let mut child = child;
        let stdout_shared = Arc::new(Mutex::new(Vec::new()));
        let stderr_shared = Arc::new(Mutex::new(Vec::new()));
        let stdout = child
            .stdout
            .take()
            .map(|pipe| tokio::spawn(capture_output(pipe, Arc::clone(&stdout_shared))));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| tokio::spawn(capture_output(pipe, Arc::clone(&stderr_shared))));
        let mut process = Self {
            child,
            address,
            signalled_at: None,
            stdout,
            stderr,
            stdout_shared,
            stderr_shared,
        };
        if wait_for_ready {
            process.wait_until_ready().await;
        }
        process
    }

    #[allow(dead_code)]
    pub fn stdout_snapshot(&self) -> String {
        String::from_utf8_lossy(
            &self
                .stdout_shared
                .lock()
                .expect("stdout capture lock is not poisoned"),
        )
        .into_owned()
    }

    #[allow(dead_code)]
    pub fn stderr_snapshot(&self) -> String {
        String::from_utf8_lossy(
            &self
                .stderr_shared
                .lock()
                .expect("stderr capture lock is not poisoned"),
        )
        .into_owned()
    }

    #[allow(dead_code)]
    pub async fn signal_terminate(&mut self) {
        if self
            .child
            .try_wait()
            .expect("child status is readable")
            .is_some()
        {
            return;
        }
        if self.signalled_at.is_none() {
            self.signalled_at = Some(tokio::time::Instant::now());
        }
        let pid = self.child.id().expect("running child has a process ID");
        let status = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .expect("SIGTERM command starts");
        assert!(status.success(), "SIGTERM reaches the production process");
    }

    #[allow(dead_code)]
    pub fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("child status is readable")
            .is_none()
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

    #[allow(dead_code)]
    pub async fn stop(self) {
        let _ = self.stop_and_capture().await;
    }

    pub async fn stop_and_capture(mut self) -> CapturedOutput {
        if self
            .child
            .try_wait()
            .expect("child status is readable")
            .is_none()
            && self.signalled_at.is_none()
        {
            self.signalled_at = Some(tokio::time::Instant::now());
            let pid = self.child.id().expect("running child has a process ID");
            let mut signal = Command::new("/bin/kill");
            signal.kill_on_drop(true).args(["-TERM", &pid.to_string()]);
            let signalled = matches!(
                timeout(Duration::from_secs(2), signal.status()).await,
                Ok(Ok(status)) if status.success()
            );
            if !signalled {
                self.child
                    .start_kill()
                    .expect("serving child accepts bounded fallback termination");
            }
        }
        let status = match timeout(Duration::from_secs(25), self.child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => panic!("serving child exit is collected: {error}"),
            Err(_) => {
                self.child
                    .start_kill()
                    .expect("overdue serving child accepts bounded fallback termination");
                timeout(Duration::from_secs(3), self.child.wait())
                    .await
                    .expect("serving child is forcibly reaped before the cleanup deadline")
                    .expect("serving child exit is collected")
            }
        };
        let shutdown_elapsed = self
            .signalled_at
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let stdout = take_capture(&mut self.stdout).await;
        let stderr = take_capture(&mut self.stderr).await;
        CapturedOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            shutdown_elapsed,
            exit_success: status.success(),
        }
    }
}

impl Drop for ServingProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.start_kill();
        }
        if let Some(task) = self.stdout.take() {
            task.abort();
        }
        if let Some(task) = self.stderr.take() {
            task.abort();
        }
    }
}

async fn capture_output<R>(mut reader: R, shared: Arc<Mutex<Vec<u8>>>) -> Vec<u8>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    const CAPTURE_LIMIT: usize = 1_048_576;
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match timeout(Duration::from_secs(30), reader.read(&mut buffer)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
            Ok(Ok(read)) => read,
        };
        let remaining = CAPTURE_LIMIT.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
        if remaining > 0 {
            shared
                .lock()
                .expect("test output buffer lock is not poisoned")
                .extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    captured
}

async fn take_capture(task: &mut Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    let Some(mut task) = task.take() else {
        return Vec::new();
    };
    match timeout(Duration::from_secs(3), &mut task).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) | Err(_) => {
            task.abort();
            Vec::new()
        }
    }
}

pub async fn send(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: &str,
) -> std::io::Result<HttpResult> {
    send_with_headers(address, method, path, credential, body, &[]).await
}

pub async fn send_with_headers(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: &str,
    extra_headers: &[(&str, &str)],
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
        for (name, value) in extra_headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
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

/// Send an actual HTTP/1.1 chunked body over TCP, with no Content-Length. The
/// chunk boundaries are transport framing, not application body frames.
#[allow(dead_code)]
pub async fn send_chunked(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: &[u8],
    chunk_size: usize,
    extra_headers: &[(&str, &str)],
) -> std::io::Result<HttpResult> {
    timeout(Duration::from_secs(8), async {
        let mut stream = TcpStream::connect(address).await?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n"
        );
        if let Some(credential) = credential {
            request.push_str(&format!("Authorization: Bearer {credential}\r\n"));
        }
        for (name, value) in extra_headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await?;
        for chunk in body.chunks(chunk_size.max(1)) {
            stream
                .write_all(format!("{:X}\r\n", chunk.len()).as_bytes())
                .await?;
            stream.write_all(chunk).await?;
            stream.write_all(b"\r\n").await?;
        }
        stream.write_all(b"0\r\n\r\n").await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        parse_response(&response)
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "chunked HTTP deadline exceeded"))?
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
