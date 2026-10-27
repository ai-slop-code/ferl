use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    time::sleep,
};

const DATABASE_SECRET: &str = "pg-secret-not-for-logs";
const PRIVATE_PATH: &str = "/private-short-code";
const PRIVATE_QUERY: &str = "destination=https%3A%2F%2Fprivate.example%2Ffull%3Ftoken%3Dsecret";

#[tokio::test]
async fn executable_starts_without_database_and_stdout_excludes_secrets_and_raw_urls() {
    let reservation = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve a local listener port");
    let address = reservation
        .local_addr()
        .expect("listener has a local address");
    drop(reservation);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ferl-server"))
        .env_clear()
        .env(
            "FERL_DATABASE_URL",
            format!("postgres://user:{DATABASE_SECRET}@127.0.0.1:1/ferl"),
        )
        .env("FERL_PUBLIC_BASE_URL", "https://links.example.test/")
        .env(
            "FERL_OTEL_ENDPOINT",
            "https://collector.example.test/v1/otlp",
        )
        .env("FERL_OTEL_HEADERS", "authorization=collector-secret")
        .env("FERL_HTTP_DRAIN_TIMEOUT_SECS", "1")
        .env("FERL_SHUTDOWN_TIMEOUT_SECS", "2")
        .env("RUST_LOG", "debug")
        .env(
            "FERL_CURSOR_KEY",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .env("FERL_LISTEN_ADDR", address.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("server executable starts");

    wait_for_listener(address, &mut child).await;
    let liveness_result = send_request(address, "/api/health/live", true).await;
    let readiness_result = send_request(address, "/api/health/ready", true).await;
    let private_path_result =
        send_request(address, &format!("{PRIVATE_PATH}?{PRIVATE_QUERY}"), false).await;
    let shutdown_started = Instant::now();
    terminate_child(&mut child).await;
    let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output())
        .await
        .expect("server exits within its configured shutdown bound plus test margin")
        .expect("server process output is collected");
    assert!(
        output.status.success(),
        "server shuts down successfully after SIGTERM"
    );
    assert!(
        shutdown_started.elapsed() < Duration::from_secs(3),
        "server exit remains within the two-second shutdown bound plus margin"
    );

    let liveness = liveness_result.expect("liveness responds while PostgreSQL is unavailable");
    assert!(
        liveness.starts_with("HTTP/1.1 200"),
        "unexpected liveness response: {liveness}"
    );
    assert!(liveness.ends_with("{\"status\":\"ok\"}"));
    assert_eq!(header(&liveness, "cache-control"), Some("no-store"));
    let liveness_request_id = header(&liveness, "x-request-id").expect("request ID header");
    uuid::Uuid::parse_str(liveness_request_id).expect("server generated a UUID");
    assert_ne!(liveness_request_id, "client-controlled-value");

    let readiness = readiness_result.expect("readiness responds during database unavailability");
    assert!(
        readiness.starts_with("HTTP/1.1 503"),
        "unexpected readiness response: {readiness}"
    );
    assert!(readiness.ends_with("{\"status\":\"unavailable\"}"));
    assert_eq!(header(&readiness, "cache-control"), Some("no-store"));
    let readiness_request_id = header(&readiness, "x-request-id").expect("request ID header");
    uuid::Uuid::parse_str(readiness_request_id).expect("server generated a UUID");
    assert_ne!(readiness_request_id, "client-controlled-value");

    let private_path = private_path_result.expect("fallback responds");
    assert!(
        private_path.starts_with("HTTP/1.1 404"),
        "unexpected fallback response: {private_path}"
    );
    assert_eq!(header(&private_path, "cache-control"), Some("no-store"));
    let fallback_body = private_path
        .split_once("\r\n\r\n")
        .expect("HTTP response has headers and a body")
        .1;
    let fallback_json: serde_json::Value =
        serde_json::from_str(fallback_body).expect("fallback response is JSON");
    assert_eq!(
        fallback_json["request_id"],
        header(&private_path, "x-request-id").expect("request ID header")
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout logs are UTF-8");
    for line in stdout.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("each stdout log line is JSON");
    }
    assert!(stdout.contains("server_started"));
    assert!(stdout.contains("server_draining"));
    assert!(stdout.contains("server_shutdown_complete"));
    assert!(stdout.contains("/{unmatched}"));
    assert!(!stdout.contains(DATABASE_SECRET));
    assert!(!stdout.contains(PRIVATE_PATH));
    assert!(!stdout.contains(PRIVATE_QUERY));
    assert!(!stdout.contains("private.example"));
    assert!(!stdout.contains("token=secret"));
    assert!(!stdout.contains(".pgpass"));
    assert!(!stdout.contains("sqlx"));
    assert!(!stdout.contains("127.0.0.1:1"));
    assert!(!stdout.contains("collector-secret"));
}

#[tokio::test]
async fn unavailable_operator_commands_and_invalid_startup_fail_safely() {
    let version = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(env!("CARGO_BIN_EXE_ferl-server"))
            .env_clear()
            .arg("--version")
            .output(),
    )
    .await
    .expect("version command returns promptly")
    .expect("server executable runs");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));

    for arguments in [
        vec!["migrate"],
        vec!["bootstrap-admin", "--username", "operator"],
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(env!("CARGO_BIN_EXE_ferl-server"))
                .env_clear()
                .args(arguments)
                .output(),
        )
        .await
        .expect("operator command returns promptly")
        .expect("operator executable runs");
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).expect("CLI error is UTF-8");
        assert!(stderr.contains("not available in this implementation stage"));
        assert!(stderr.contains("no database was modified"));
    }

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(env!("CARGO_BIN_EXE_ferl-server"))
            .env_clear()
            .env(
                "FERL_DATABASE_URL",
                "postgres://user:database-secret@localhost/ferl",
            )
            .env("FERL_PUBLIC_BASE_URL", "https://links.example.test")
            .env("FERL_CURSOR_KEY", "cursor-secret-invalid")
            .output(),
    )
    .await
    .expect("invalid startup returns promptly")
    .expect("server executable runs");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("structured log is UTF-8");
    assert!(stdout.contains("configuration_invalid"));
    assert!(stdout.contains("FERL_CURSOR_KEY"));
    assert!(!stdout.contains("database-secret"));
    assert!(!stdout.contains("cursor-secret-invalid"));

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(env!("CARGO_BIN_EXE_ferl-server"))
            .env_clear()
            .env(
                "FERL_DATABASE_URL",
                "postgres://user:database-secret@localhost/ferl",
            )
            .env(
                "FERL_PUBLIC_BASE_URL",
                "https://links.example.test/%2e?token=url-secret",
            )
            .env(
                "FERL_CURSOR_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .output(),
    )
    .await
    .expect("normalized invalid origin fails promptly")
    .expect("server executable runs");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("structured log is UTF-8");
    assert!(stdout.contains("FERL_PUBLIC_BASE_URL"));
    assert!(!stdout.contains("database-secret"));
    assert!(!stdout.contains("url-secret"));
}

async fn wait_for_listener(address: std::net::SocketAddr, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                send_request(address, "/api/health/live", true),
            )
            .await,
            Ok(Ok(response)) if response.starts_with("HTTP/1.1 200")
        ) {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut stdout = Vec::new();
            if let Some(mut stream) = child.stdout.take() {
                let _ = stream.read_to_end(&mut stdout).await;
            }
            let mut stderr = Vec::new();
            if let Some(mut stream) = child.stderr.take() {
                let _ = stream.read_to_end(&mut stderr).await;
            }
            panic!(
                "server exited before listening with status {status}; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "server did not listen before the deadline"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn send_request(
    address: std::net::SocketAddr,
    path: &str,
    invalid_credentials: bool,
) -> std::io::Result<String> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = TcpStream::connect(address).await?;
        let authorization = if invalid_credentials {
            "Authorization: Bearer invalid-health-credential\r\n"
        } else {
            ""
        };
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: local.test\r\nX-Request-ID: client-controlled-value\r\n{authorization}Connection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<String, std::io::Error>(String::from_utf8_lossy(&response).into_owned())
    })
    .await
    .map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP process request timed out")
    })?
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            header_name
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
}

async fn terminate_child(child: &mut Child) {
    let Some(pid) = child.id() else {
        return;
    };
    let mut command = Command::new("/bin/kill");
    command.kill_on_drop(true).args(["-TERM", &pid.to_string()]);
    let result = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .expect("SIGTERM command completes before its deadline")
        .expect("send SIGTERM to the test-owned server process");
    assert!(result.status.success(), "test-owned server accepts SIGTERM");
}
