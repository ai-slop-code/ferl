#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use account_support::{migrated_database, seed_account};
use serving_process::{HttpResult, ServingProcess, send, send_with_headers};

const POSTGRES_CONTAINER: &str = "ferl-stage01-postgres";

#[derive(Clone)]
struct ExportedSpan {
    resource: Value,
    span: Value,
}

#[derive(Clone)]
struct ExportedLog {
    resource: Value,
    log: Value,
}

#[derive(Clone)]
struct ExportedMetric {
    resource: Value,
    name: String,
    point: Value,
}

#[derive(Default)]
struct Signals {
    spans: Vec<ExportedSpan>,
    logs: Vec<ExportedLog>,
    metrics: Vec<ExportedMetric>,
}

struct StalledReceiver {
    address: std::net::SocketAddr,
    accepted_paths: Arc<std::sync::Mutex<Vec<String>>>,
    accepted: mpsc::Receiver<String>,
    release: Arc<Semaphore>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

struct NoEndpointSentinel {
    accepted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl NoEndpointSentinel {
    async fn bind_sdk_default() -> Self {
        let listener = TcpListener::bind("127.0.0.1:4318")
            .await
            .expect("standard OTLP default port is available to observe no-endpoint mode");
        let accepted = Arc::new(AtomicUsize::new(0));
        let task_accepted = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            while let Ok((_stream, _peer)) = listener.accept().await {
                task_accepted.fetch_add(1, Ordering::AcqRel);
            }
        });
        Self { accepted, task }
    }

    async fn stop(self) -> usize {
        let accepted = self.accepted.load(Ordering::Acquire);
        self.task.abort();
        accepted
    }
}

impl StalledReceiver {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stalled OTLP receiver binds a loopback port");
        let address = listener
            .local_addr()
            .expect("stalled receiver has an address");
        let accepted_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let release = Arc::new(Semaphore::new(0));
        let cancellation = CancellationToken::new();
        let (accepted_tx, accepted) = mpsc::channel(256);
        let task_paths = Arc::clone(&accepted_paths);
        let task_release = Arc::clone(&release);
        let task_cancel = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => break,
                    result = listener.accept() => {
                        if let Ok((stream, _peer)) = result {
                            connections.spawn(handle_stalled_connection(
                                stream,
                                Arc::clone(&task_paths),
                                accepted_tx.clone(),
                                Arc::clone(&task_release),
                            ));
                        }
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            address,
            accepted_paths,
            accepted,
            release,
            cancellation,
            task,
        }
    }

    async fn wait_for_paths(&mut self, expected: &[&'static str]) {
        let mut remaining = expected.iter().copied().collect::<BTreeSet<_>>();
        timeout(Duration::from_secs(15), async {
            while !remaining.is_empty() {
                let Some(path) = self.accepted.recv().await else {
                    break;
                };
                remaining.remove(path.as_str());
            }
        })
        .await
        .unwrap_or_else(|_| panic!("stalled receiver did not observe OTLP paths {remaining:?}"));
        assert!(
            remaining.is_empty(),
            "stalled receiver observed all signals"
        );
    }

    fn count(&self, path: &str) -> usize {
        self.accepted_paths
            .lock()
            .expect("stalled receiver path lock is not poisoned")
            .iter()
            .filter(|accepted| accepted.as_str() == path)
            .count()
    }

    async fn stop(mut self) {
        self.release.add_permits(10_000);
        self.cancellation.cancel();
        timeout(Duration::from_secs(5), &mut self.task)
            .await
            .expect("stalled receiver handlers close after release")
            .expect("stalled receiver task joins");
    }
}

impl Drop for StalledReceiver {
    fn drop(&mut self) {
        self.release.add_permits(10_000);
        self.cancellation.cancel();
        self.task.abort();
    }
}

async fn handle_stalled_connection(
    mut stream: TcpStream,
    accepted_paths: Arc<std::sync::Mutex<Vec<String>>>,
    accepted: mpsc::Sender<String>,
    release: Arc<Semaphore>,
) {
    let Some(path) = read_http_path(&mut stream).await else {
        return;
    };
    accepted_paths
        .lock()
        .expect("stalled receiver path lock is not poisoned")
        .push(path.clone());
    let _ = accepted.send(path).await;
    if let Ok(permit) = release.acquire().await {
        permit.forget();
        let _ = stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
    }
}

async fn read_http_path(stream: &mut TcpStream) -> Option<String> {
    const MAX_REQUEST_BYTES: usize = 1_048_576;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 || bytes.len().saturating_add(read) > MAX_REQUEST_BYTES {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).ok()?.to_owned();
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let total = header_end.checked_add(content_length)?;
    if total > MAX_REQUEST_BYTES {
        return None;
    }
    while bytes.len() < total {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 || bytes.len().saturating_add(read) > MAX_REQUEST_BYTES {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    header_text
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
}

fn load_signals(directory: &Path) -> Signals {
    let mut files = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("collector capture directory is readable: {error}"))
        .map(|entry| entry.expect("capture directory entry is readable").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    files.sort();

    let mut output = Signals::default();
    for path in files {
        let contents = fs::read(&path).expect("machine-readable collector capture is readable");
        for line in contents.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(document) = serde_json::from_slice::<Value>(line) else {
                // A concurrent collector write can leave one final partial
                // line; the next bounded poll will see the completed record.
                continue;
            };
            if let Some(resource_spans) = document.get("resourceSpans").and_then(Value::as_array) {
                for resource_span in resource_spans {
                    let resource = resource_span.get("resource").cloned().unwrap_or_default();
                    for scope in resource_span
                        .get("scopeSpans")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        for span in scope
                            .get("spans")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            output.spans.push(ExportedSpan {
                                resource: resource.clone(),
                                span: span.clone(),
                            });
                        }
                    }
                }
            }
            if let Some(resource_logs) = document.get("resourceLogs").and_then(Value::as_array) {
                for resource_log in resource_logs {
                    let resource = resource_log.get("resource").cloned().unwrap_or_default();
                    for scope in resource_log
                        .get("scopeLogs")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        for log in scope
                            .get("logRecords")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            output.logs.push(ExportedLog {
                                resource: resource.clone(),
                                log: log.clone(),
                            });
                        }
                    }
                }
            }
            if let Some(resource_metrics) =
                document.get("resourceMetrics").and_then(Value::as_array)
            {
                for resource_metric in resource_metrics {
                    let resource = resource_metric.get("resource").cloned().unwrap_or_default();
                    for scope in resource_metric
                        .get("scopeMetrics")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        for metric in scope
                            .get("metrics")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            let Some(name) = metric.get("name").and_then(Value::as_str) else {
                                continue;
                            };
                            for data_point in metric_data_points(metric) {
                                output.metrics.push(ExportedMetric {
                                    resource: resource.clone(),
                                    name: name.to_owned(),
                                    point: data_point.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    output
}

fn metric_data_points(metric: &Value) -> Vec<&Value> {
    [
        "sum",
        "gauge",
        "histogram",
        "exponentialHistogram",
        "summary",
    ]
    .into_iter()
    .filter_map(|kind| metric.get(kind).and_then(|value| value.get("dataPoints")))
    .flat_map(|points| points.as_array().into_iter().flatten())
    .collect()
}

fn metric_samples(signals: &Signals, name: &str) -> u64 {
    signals
        .metrics
        .iter()
        .filter(|metric| metric.name == name)
        .flat_map(|metric| {
            ["asInt", "count"]
                .into_iter()
                .filter_map(|field| metric.point.get(field))
        })
        .filter_map(|value| {
            value
                .as_str()
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| value.as_u64())
        })
        .sum()
}

fn resource_attribute<'a>(resource: &'a Value, key: &str) -> Option<&'a str> {
    resource
        .get("attributes")?
        .as_array()?
        .iter()
        .find(|attribute| attribute.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")?
        .get("stringValue")?
        .as_str()
}

fn data_attribute<'a>(attributes: &'a Value, key: &str) -> Option<&'a Value> {
    attributes
        .as_array()?
        .iter()
        .find(|attribute| attribute.get("key").and_then(Value::as_str) == Some(key))?
        .get("value")
}

fn data_string<'a>(attributes: &'a Value, key: &str) -> Option<&'a str> {
    data_attribute(attributes, key)?
        .get("stringValue")?
        .as_str()
}

fn request_id_in_span(span: &Value) -> Option<&str> {
    data_string(span.get("attributes")?, "request_id")
}

async fn wait_for_capture(
    directory: &Path,
    predicate: impl Fn(&Signals) -> bool,
    label: &str,
) -> Signals {
    timeout(Duration::from_secs(30), async {
        loop {
            let current = load_signals(directory);
            if predicate(&current) {
                return current;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("collector did not capture {label} before the deadline"))
}

async fn wait_until_ready(process: &ServingProcess) {
    timeout(Duration::from_secs(35), async {
        loop {
            if let Ok(response) = send(process.address, "GET", "/api/health/ready", None, "").await
                && response.status == 200
            {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("replacement process becomes ready after PostgreSQL/schema recovery");
}

async fn collector_command(run_id: &str, port: &str, action: &str) {
    let status = Command::new("sh")
        .args(["scripts/collector.sh", action])
        .env("FERL_COLLECTOR_RUN_ID", run_id)
        .env("FERL_COLLECTOR_PORT", port)
        .status()
        .await
        .expect("collector harness command starts");
    assert!(status.success(), "collector harness {action} succeeds");
}

fn cursor_key() -> String {
    let mut bytes = [0x55_u8; 32];
    bytes[..14].copy_from_slice(b"cursor-canary-");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn add_database_url_canary(database_url: &str) -> String {
    let mut parsed = url::Url::parse(database_url).expect("isolated DB URL parses");
    parsed
        .query_pairs_mut()
        .append_pair("application_name", "database-url-secret-canary");
    parsed.to_string()
}

fn request_status(result: std::io::Result<HttpResult>, status: u16) -> HttpResult {
    let result = result.expect("production TCP request succeeds");
    serving_process::assert_headers(&result);
    assert_eq!(result.status, status, "unexpected production HTTP response");
    result
}

fn read_json(response: &HttpResult) -> Value {
    serde_json::from_slice(&response.body).expect("production response is valid JSON")
}

#[tokio::test]
async fn production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified() {
    let database = migrated_database().await;
    database
        .verify(|database| Box::pin(run_telemetry_scenario(database)))
        .await;
}

async fn run_telemetry_scenario(database: &database_support::IsolatedTestDatabase) {
    let endpoint = std::env::var("FERL_TEST_OTEL_ENDPOINT")
        .expect("scripts/check.sh starts the pinned Collector and provides its endpoint");
    let capture_dir = PathBuf::from(
        std::env::var("FERL_TEST_OTEL_CAPTURE_DIR")
            .expect("scripts/check.sh provides this run's isolated collector capture path"),
    );
    let run_id = std::env::var("FERL_COLLECTOR_RUN_ID").expect("collector run ID is set");
    let collector_port = std::env::var("FERL_COLLECTOR_PORT").expect("collector host port is set");

    assert!(
        capture_dir.is_dir(),
        "the owned file-exporter capture directory exists"
    );

    let database_url = add_database_url_canary(database.schema_database_url());
    let (_admin_id, admin_key) =
        seed_account(database.pool(), "telemetry-admin", "admin", true).await;
    let invalid_key = format!("ferl_ak_v1_{}", URL_SAFE_NO_PAD.encode([0xC1_u8; 32]));
    let invalid_management_token = format!("ferl_mt_v1_{}", URL_SAFE_NO_PAD.encode([0xD2_u8; 32]));
    let cursor_key = cursor_key();
    let common = [
        ("FERL_OTEL_ENDPOINT", endpoint.as_str()),
        (
            "FERL_OTEL_HEADERS",
            "x-export-secret=collector-header-secret-canary",
        ),
        ("FERL_OTEL_TIMEOUT_SECS", "1"),
        ("FERL_OTEL_TRACE_SAMPLE_RATIO", "1.0"),
        ("FERL_OTEL_QUEUE_CAPACITY", "16"),
        ("FERL_OTEL_BATCH_SIZE", "4"),
        ("FERL_ANONYMOUS_CREATION", "true"),
        ("FERL_ANONYMOUS_CREATE_LIMIT", "1"),
        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_SHUTDOWN_TIMEOUT_SECS", "10"),
    ];

    let first = ServingProcess::start_with(&database_url, &cursor_key, &common).await;

    let me = request_status(
        send(first.address, "GET", "/api/v1/me", Some(&admin_key), "").await,
        200,
    );
    assert_eq!(read_json(&me)["username"], "telemetry-admin");
    let invalid_auth = request_status(
        send(first.address, "GET", "/api/v1/me", Some(&invalid_key), "").await,
        401,
    );
    assert_eq!(
        read_json(&invalid_auth)["error"]["code"],
        "invalid_credentials"
    );

    let account = request_status(
        send(
            first.address,
            "POST",
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"telemetry-user-canary","role":"user"}"#,
        )
        .await,
        201,
    );
    let account_body = read_json(&account);
    let user_key = account_body["api_key"]
        .as_str()
        .expect("created account returns its one-time key")
        .to_owned();
    assert_eq!(
        request_status(
            send(first.address, "GET", "/api/v1/me", Some(&user_key), "").await,
            200,
        )
        .status,
        200
    );

    let destination_a = "https://destination-canary.invalid/a?token=destination-query-canary";
    let destination_b =
        "https://historical-destination-canary.invalid/b?token=historical-query-canary";
    let created_link = request_status(
        send(
            first.address,
            "POST",
            "/api/v1/links",
            Some(&admin_key),
            &format!(
                r#"{{"destination_url":"{destination_a}","short_code":"telemetry-link-canary","title":"title-canary-private","tags":["tag-canary-private"]}}"#
            ),
        )
        .await,
        201,
    );
    assert_eq!(
        read_json(&created_link)["link"]["short_code"],
        "telemetry-link-canary"
    );

    let remote_trace_id = "11111111111111111111111111111111";
    let remote_span_id = "2222222222222222";
    let sampled_parent = format!("00-{remote_trace_id}-{remote_span_id}-01");
    let redirect = request_status(
        send_with_headers(
            first.address,
            "GET",
            "/telemetry-link-canary",
            None,
            "",
            &[
                ("traceparent", &sampled_parent),
                ("tracestate", "vendor=tracestate-canary-private"),
                ("baggage", "secret=baggage-canary-private"),
                (
                    "Referer",
                    "https://referrer-canary.invalid/r?secret=referrer-query-canary",
                ),
                ("User-Agent", "ferl-user-agent-canary/91.0"),
                ("X-Forwarded-For", "203.0.113.77,forwarded-header-canary"),
            ],
        )
        .await,
        302,
    );
    assert_eq!(
        redirect.headers.get("location").map(String::as_str),
        Some(destination_a)
    );
    let redirect_request_id = redirect.headers["x-request-id"].clone();

    let patch = request_status(
        send(
            first.address,
            "PATCH",
            "/api/v1/links/telemetry-link-canary",
            Some(&admin_key),
            &format!(r#"{{"destination_url":"{destination_b}"}}"#),
        )
        .await,
        200,
    );
    assert_eq!(read_json(&patch)["destination_url"], destination_b);
    let second_redirect = request_status(
        send(first.address, "HEAD", "/telemetry-link-canary", None, "").await,
        302,
    );
    assert_eq!(
        second_redirect.headers.get("location").map(String::as_str),
        Some(destination_b)
    );

    let anonymous = request_status(
        send(
            first.address,
            "POST",
            "/api/v1/links",
            None,
            r#"{"destination_url":"https://anonymous-destination-canary.invalid/entry"}"#,
        )
        .await,
        201,
    );
    let anonymous_body = read_json(&anonymous);
    let anonymous_code = anonymous_body["link"]["short_code"]
        .as_str()
        .unwrap()
        .to_owned();
    let management_token = anonymous_body["management_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let rate_rejected = request_status(
        send(
            first.address,
            "POST",
            "/api/v1/links",
            None,
            r#"{"destination_url":"https://rate-rejection-canary.invalid/entry"}"#,
        )
        .await,
        429,
    );
    assert_eq!(read_json(&rate_rejected)["error"]["code"], "rate_limited");
    let token_report = request_status(
        send(
            first.address,
            "GET",
            &format!("/api/v1/links/{anonymous_code}/stats"),
            Some(&management_token),
            "",
        )
        .await,
        200,
    );
    assert_eq!(read_json(&token_report)["lifetime_visits"], 0);
    let invalid_token = request_status(
        send(
            first.address,
            "GET",
            &format!("/api/v1/links/{anonymous_code}/stats"),
            Some(&invalid_management_token),
            "",
        )
        .await,
        401,
    );
    assert_eq!(
        read_json(&invalid_token)["error"]["code"],
        "invalid_credentials"
    );

    // Eligible malformed/invalid operations still produce safe rejection logs;
    // their bodies and query/cursor contents are never telemetry attributes.
    let bad_link = request_status(
        send(
            first.address,
            "POST",
            "/api/v1/links",
            Some(&admin_key),
            r#"{"destination_url":"javascript:validation-url-canary"}"#,
        )
        .await,
        400,
    );
    assert_eq!(read_json(&bad_link)["error"]["code"], "invalid_request");
    let list = request_status(
        send(first.address, "GET", "/api/v1/links", Some(&admin_key), "").await,
        200,
    );
    assert!(
        read_json(&list)["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["short_code"] == "telemetry-link-canary" })
    );
    let query_canary = request_status(
        send(
            first.address,
            "GET",
            "/api/v1/links?q=search-canary-private&tag=tag-canary-private",
            Some(&admin_key),
            "",
        )
        .await,
        200,
    );
    assert!(read_json(&query_canary)["items"].as_array().is_some());
    let bad_cursor = request_status(
        send(
            first.address,
            "GET",
            "/api/v1/links?cursor=cursor-canary-private",
            Some(&admin_key),
            "",
        )
        .await,
        400,
    );
    assert_eq!(read_json(&bad_cursor)["error"]["code"], "invalid_request");

    let unsampled_trace_id = "33333333333333333333333333333333";
    let unsampled_parent = format!("00-{unsampled_trace_id}-4444444444444444-00");
    let _ = request_status(
        send_with_headers(
            first.address,
            "GET",
            "/api/health/live",
            None,
            "",
            &[
                ("traceparent", &unsampled_parent),
                ("baggage", "baggage-canary-unsampled"),
            ],
        )
        .await,
        200,
    );
    let _ = request_status(
        send_with_headers(
            first.address,
            "GET",
            "/api/health/live",
            None,
            "",
            &[("traceparent", "malformed-traceparent-canary")],
        )
        .await,
        200,
    );
    let _ = request_status(
        send_with_headers(
            first.address,
            "GET",
            "/api/v1/me",
            Some(&invalid_key),
            "",
            &[("traceparent", &sampled_parent)],
        )
        .await,
        401,
    );

    timeout(Duration::from_secs(10), async {
        loop {
            let report = send(
                first.address,
                "GET",
                "/api/v1/links/telemetry-link-canary/stats",
                Some(&admin_key),
                "",
            )
            .await
            .expect("report poll reaches the serving process");
            if report.status == 200
                && read_json(&report)["lifetime_visits"]
                    .as_i64()
                    .unwrap_or_default()
                    >= 2
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("analytics worker persists the two queued redirect visits");

    let raw_visits = request_status(
        send(
            first.address,
            "GET",
            "/api/v1/links/telemetry-link-canary/visits",
            Some(&admin_key),
            "",
        )
        .await,
        200,
    );
    let raw = read_json(&raw_visits);
    let items = raw["items"]
        .as_array()
        .expect("authorized report returns visit rows");
    assert!(
        items
            .iter()
            .any(|visit| visit["redirect_url"] == destination_a)
    );
    assert!(
        items
            .iter()
            .any(|visit| visit["redirect_url"] == destination_b)
    );

    let second = ServingProcess::start_with(&database_url, &cursor_key, &common).await;
    let through_second = request_status(
        send(second.address, "GET", "/telemetry-link-canary", None, "").await,
        302,
    );
    assert_eq!(
        through_second.headers.get("location").map(String::as_str),
        Some(destination_b)
    );

    let first_output = first.stop_and_capture().await;
    assert!(first_output.exit_success);
    assert!(first_output.shutdown_elapsed < Duration::from_secs(10));
    assert!(first_output.stdout.contains("server_draining"));
    assert!(first_output.stdout.contains("server_shutdown_complete"));
    let first_instance_id = started_instance_id(&first_output.stdout);
    let first_signals = wait_for_capture(
        &capture_dir,
        |signals| {
            signals.spans.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.instance.id")
                    == Some(first_instance_id.as_str())
            }) && signals.logs.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.instance.id")
                    == Some(first_instance_id.as_str())
            }) && signals.metrics.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.instance.id")
                    == Some(first_instance_id.as_str())
            })
        },
        "A process spans, metrics, and LogRecords after provider shutdown flush",
    )
    .await;
    assert!(!first_signals.spans.is_empty());
    assert!(!first_signals.logs.is_empty());
    assert!(!first_signals.metrics.is_empty());

    // Collector refusal is exercised after successful traffic. The same process
    // remains ready and continues API/redirect work; restart reuses the bounded
    // file capture and periodic exporters recover without a server restart.
    collector_command(&run_id, &collector_port, "stop").await;
    assert_eq!(
        request_status(
            send(second.address, "GET", "/api/health/ready", None, "").await,
            200
        )
        .status,
        200,
    );
    assert_eq!(
        request_status(
            send(second.address, "GET", "/api/v1/me", Some(&admin_key), "").await,
            200
        )
        .status,
        200,
    );
    assert_eq!(
        request_status(
            send(second.address, "GET", "/telemetry-link-canary", None, "").await,
            302
        )
        .status,
        302,
    );
    collector_command(&run_id, &collector_port, "restart").await;
    let recovered = request_status(
        send(second.address, "GET", "/api/health/live", None, "").await,
        200,
    );
    let recovered_request_id = recovered.headers["x-request-id"].clone();
    let after_recovery = wait_for_capture(
        &capture_dir,
        |signals| {
            signals.logs.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.name") == Some("ferl")
                    && data_string(&entry.log["attributes"], "request_id")
                        == Some(recovered_request_id.as_str())
            })
        },
        "a correlated application LogRecord after collector restart",
    )
    .await;
    assert!(!after_recovery.logs.is_empty());

    // The instance removed from service becomes unready before listener removal.
    // A replacement process uses the same PostgreSQL state without state transfer.
    let second_live = request_status(
        send(second.address, "GET", "/api/health/ready", None, "").await,
        200,
    );
    assert_eq!(second_live.status, 200);
    let replacement = ServingProcess::start_with(&database_url, &cursor_key, &common).await;
    let existing_link = request_status(
        send(
            replacement.address,
            "GET",
            "/api/v1/links/telemetry-link-canary",
            Some(&admin_key),
            "",
        )
        .await,
        200,
    );
    assert_eq!(read_json(&existing_link)["destination_url"], destination_b);
    let replacement_report = request_status(
        send(
            replacement.address,
            "GET",
            "/api/v1/links/telemetry-link-canary/stats",
            Some(&admin_key),
            "",
        )
        .await,
        200,
    );
    assert!(
        read_json(&replacement_report)["lifetime_visits"]
            .as_i64()
            .unwrap()
            >= 2
    );

    let second_output = second.stop_and_capture().await;
    let replacement_output = replacement.stop_and_capture().await;
    assert!(second_output.exit_success && replacement_output.exit_success);
    assert!(second_output.shutdown_elapsed < Duration::from_secs(10));
    assert!(replacement_output.shutdown_elapsed < Duration::from_secs(10));

    // Root sampling zero suppresses root spans, but real metrics and warning
    // LogRecords continue to export. The process uses the same database state.
    let ratio_zero = [
        ("FERL_OTEL_ENDPOINT", endpoint.as_str()),
        ("FERL_OTEL_TRACE_SAMPLE_RATIO", "0.0"),
        ("FERL_OTEL_TIMEOUT_SECS", "1"),
        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_SHUTDOWN_TIMEOUT_SECS", "10"),
    ];
    let no_root_sampling =
        ServingProcess::start_with(&database_url, &cursor_key, &ratio_zero).await;
    let zero_error = request_status(
        send(
            no_root_sampling.address,
            "GET",
            "/api/v1/me",
            Some(&invalid_key),
            "",
        )
        .await,
        401,
    );
    let zero_request_id = zero_error.headers["x-request-id"].clone();
    let zero_output = no_root_sampling.stop_and_capture().await;
    assert!(zero_output.exit_success);
    assert!(zero_output.shutdown_elapsed < Duration::from_secs(10));

    let zero_instance_id = started_instance_id(&zero_output.stdout);
    let zero_signals = wait_for_capture(
        &capture_dir,
        |signals| {
            signals.logs.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.instance.id")
                    == Some(&zero_instance_id)
                    && data_string(&entry.log["attributes"], "request_id")
                        == Some(zero_request_id.as_str())
            }) && signals.metrics.iter().any(|entry| {
                resource_attribute(&entry.resource, "service.instance.id")
                    == Some(&zero_instance_id)
            })
        },
        "root-sampling-zero metrics and error LogRecord",
    )
    .await;
    assert!(zero_signals.spans.iter().all(|span| {
        resource_attribute(&span.resource, "service.instance.id") != Some(&zero_instance_id)
    }));

    let withdrawal_settings = [
        ("FERL_OTEL_ENDPOINT", endpoint.as_str()),
        ("FERL_OTEL_TIMEOUT_SECS", "1"),
        ("FERL_READINESS_WITHDRAWAL_SECS", "2"),
        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_SHUTDOWN_TIMEOUT_SECS", "10"),
    ];
    let mut withdrawal_process =
        ServingProcess::start_with(&database_url, &cursor_key, &withdrawal_settings).await;
    let charges_before_stall = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
        .fetch_one(database.pool())
        .await
        .expect("test observes the creation ledger before stalling an HTTP body");
    let mut withdrawal_body = TcpStream::connect(withdrawal_process.address)
        .await
        .expect("withdrawal test opens a real application TCP connection");
    let withdrawal_head = format!(
        "POST /api/v1/links HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nAuthorization: Bearer {admin_key}\r\nContent-Type: application/json\r\nContent-Length: 4096\r\n\r\n",
        withdrawal_process.address
    );
    withdrawal_body
        .write_all(withdrawal_head.as_bytes())
        .await
        .expect("stalled body request headers reach the server");
    timeout(Duration::from_secs(5), async {
        loop {
            let charges = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                .fetch_one(database.pool())
                .await
                .expect("test observes the committed pre-body creation charge");
            if charges > charges_before_stall {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("stalled HTTP body remains active only after production admission");
    let withdrawal_started = Instant::now();
    withdrawal_process.signal_terminate().await;
    let withdrawal_probe = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = send(
                withdrawal_process.address,
                "GET",
                "/api/health/ready",
                None,
                "",
            )
            .await
                && response.status == 503
            {
                break response;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("TCP readiness observes draining before the withdrawal deadline");
    let readiness_withdrawal_observed = withdrawal_started.elapsed();
    assert_eq!(withdrawal_probe.body, br#"{"status":"unavailable"}"#);
    let withdrawal_liveness = request_status(
        send(
            withdrawal_process.address,
            "GET",
            "/api/health/live",
            None,
            "",
        )
        .await,
        200,
    );
    assert_eq!(
        read_json(&withdrawal_liveness),
        serde_json::json!({"status":"ok"})
    );
    timeout(Duration::from_secs(8), async {
        loop {
            if TcpStream::connect(withdrawal_process.address)
                .await
                .is_err()
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("listener removal is observed after readiness withdrawal");
    let listener_removal_observed = withdrawal_started.elapsed();
    let withdrawal_output = withdrawal_process.stop_and_capture().await;
    drop(withdrawal_body);
    assert!(withdrawal_output.exit_success);
    assert!(withdrawal_output.shutdown_elapsed < Duration::from_secs(10));
    let withdrawal_instance_id = started_instance_id(&withdrawal_output.stdout);
    let withdrawal_signals = wait_for_capture(
        &capture_dir,
        |signals| {
            signals.metrics.iter().any(|metric| {
                metric.name == "ferl.http.server.active_requests"
                    && resource_attribute(&metric.resource, "service.instance.id")
                        == Some(&withdrawal_instance_id)
                    && data_string(&metric.point["attributes"], "method") == Some("POST")
                    && data_string(&metric.point["attributes"], "route") == Some("/api/v1/links")
                    && metric.point.get("asInt").and_then(Value::as_str) == Some("0")
            })
        },
        "cancelled HTTP request active-count metric",
    )
    .await;
    let active_requests = withdrawal_signals
        .metrics
        .iter()
        .rev()
        .find(|metric| {
            metric.name == "ferl.http.server.active_requests"
                && resource_attribute(&metric.resource, "service.instance.id")
                    == Some(&withdrawal_instance_id)
                && data_string(&metric.point["attributes"], "method") == Some("POST")
                && data_string(&metric.point["attributes"], "route") == Some("/api/v1/links")
        })
        .expect("active-count stream includes the cancelled creation request");
    assert_eq!(
        active_requests.point["asInt"], "0",
        "cancelled HTTP admission returned its guard; instance={} stdout={}",
        withdrawal_instance_id, withdrawal_output.stdout
    );

    // No endpoint means no provider/export attempt. If the Collector is not on
    // the SDK default port, a TCP sentinel watches localhost:4318 instead.
    let baseline = load_signals(&capture_dir);
    let default_endpoint_sentinel = if collector_port == "4318" {
        None
    } else {
        Some(NoEndpointSentinel::bind_sdk_default().await)
    };
    let no_endpoint: [(&str, &str); 3] = [
        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_SHUTDOWN_TIMEOUT_SECS", "10"),
    ];
    let mut no_endpoint_settings = no_endpoint.to_vec();
    no_endpoint_settings.push(("FERL_HTTP_MAX_IN_FLIGHT", "128"));
    no_endpoint_settings.push(("FERL_HTTP_MAX_WAITING", "128"));
    let stdout_only =
        ServingProcess::start_with(&database_url, &cursor_key, &no_endpoint_settings).await;
    for _ in 0..140 {
        let _ = request_status(
            send(stdout_only.address, "GET", "/api/health/live", None, "").await,
            200,
        );
    }
    let stdout_only_output = stdout_only.stop_and_capture().await;
    assert!(stdout_only_output.exit_success);
    assert!(stdout_only_output.stdout.contains("request completed"));
    let stdout_only_instance = started_instance_id(&stdout_only_output.stdout);
    if let Some(sentinel) = default_endpoint_sentinel {
        assert!(
            timeout(Duration::from_secs(2), async {
                loop {
                    if sentinel.accepted.load(Ordering::Acquire) > 0 {
                        break;
                    }
                    sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .is_err(),
            "no-endpoint mode makes no request to the SDK default localhost receiver"
        );
        assert_eq!(sentinel.stop().await, 0);
    }
    let after_no_endpoint = load_signals(&capture_dir);
    assert!(!after_no_endpoint.spans.iter().any(|span| {
        resource_attribute(&span.resource, "service.instance.id") == Some(&stdout_only_instance)
    }));
    assert!(!after_no_endpoint.logs.iter().any(|log| {
        resource_attribute(&log.resource, "service.instance.id") == Some(&stdout_only_instance)
    }));
    assert!(!after_no_endpoint.metrics.iter().any(|metric| {
        resource_attribute(&metric.resource, "service.instance.id") == Some(&stdout_only_instance)
    }));
    assert!(after_no_endpoint.logs.len() >= baseline.logs.len());

    // The database outage path is observed over TCP while Collector state is
    // irrelevant. Liveness stays up; completed readiness and DB-dependent work
    // use the exact OpenAPI health/error responses.
    let outage_process = ServingProcess::start_with(&database_url, &cursor_key, &common).await;
    let stopped =
        database_support::command_output("podman", &["stop", "--time", "5", POSTGRES_CONTAINER])
            .await;
    assert!(
        stopped.status.success(),
        "only the owned PostgreSQL harness stops"
    );
    let outage_live = request_status(
        send(outage_process.address, "GET", "/api/health/live", None, "").await,
        200,
    );
    assert_eq!(read_json(&outage_live), serde_json::json!({"status":"ok"}));
    let outage_ready = request_status(
        send(outage_process.address, "GET", "/api/health/ready", None, "").await,
        503,
    );
    assert_eq!(outage_ready.body, br#"{"status":"unavailable"}"#);
    let dependent = request_status(
        send(
            outage_process.address,
            "GET",
            "/api/v1/me",
            Some(&admin_key),
            "",
        )
        .await,
        503,
    );
    assert_eq!(
        read_json(&dependent)["error"]["code"],
        "storage_unavailable"
    );
    let started = database_support::command_output("podman", &["start", POSTGRES_CONTAINER]).await;
    assert!(
        started.status.success(),
        "the same owned PostgreSQL container restarts"
    );
    database_support::wait_until_postgres_is_ready().await;
    wait_until_ready(&outage_process).await;
    let outage_output = outage_process.stop_and_capture().await;
    assert!(outage_output.exit_success);
    assert!(outage_output.shutdown_elapsed < Duration::from_secs(10));

    // Controlled slow receiver: accept the complete OTLP bodies but withhold
    // every response until after the production process has shut down. Unlike
    // connection refusal, this holds exporter work in flight while requests
    // continue and the bounded SDK queues fill.
    let mut stalled = StalledReceiver::start().await;
    let stalled_endpoint = format!("http://{}", stalled.address);
    let pressure_settings = [
        ("FERL_OTEL_ENDPOINT", stalled_endpoint.as_str()),
        ("FERL_OTEL_TIMEOUT_SECS", "30"),
        ("FERL_OTEL_QUEUE_CAPACITY", "8"),
        ("FERL_OTEL_BATCH_SIZE", "8"),
        ("FERL_HTTP_MAX_IN_FLIGHT", "128"),
        ("FERL_HTTP_MAX_WAITING", "128"),
        ("FERL_READINESS_WITHDRAWAL_SECS", "2"),
        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", "2"),
        ("FERL_SHUTDOWN_TIMEOUT_SECS", "60"),
    ];
    let mut pressured =
        ServingProcess::start_with(&database_url, &cursor_key, &pressure_settings).await;
    for _ in 0..10 {
        assert_eq!(
            request_status(
                send(pressured.address, "GET", "/api/health/live", None, "").await,
                200,
            )
            .status,
            200,
        );
    }
    stalled.wait_for_paths(&["/v1/traces", "/v1/logs"]).await;

    let prior_charge_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
        .fetch_one(database.pool())
        .await
        .expect("test observes the current durable creation-charge count");
    let mut held_body = TcpStream::connect(pressured.address)
        .await
        .expect("TCP connection establishes a request with an intentionally withheld body");
    let held_head = format!(
        "POST /api/v1/links HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nAuthorization: Bearer {admin_key}\r\nContent-Type: application/json\r\nContent-Length: 4096\r\n\r\n",
        pressured.address
    );
    held_body
        .write_all(held_head.as_bytes())
        .await
        .expect("held request headers reach the production TCP listener");
    timeout(Duration::from_secs(5), async {
        loop {
            let charges = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                .fetch_one(database.pool())
                .await
                .expect("test observes committed creation admission");
            if charges > prior_charge_count {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("body is stalled only after the production route commits its allowance charge");

    let mut public_calls = tokio::task::JoinSet::new();
    for _ in 0..80 {
        let address = pressured.address;
        public_calls.spawn(async move { send(address, "GET", "/api/health/live", None, "").await });
    }
    while let Some(result) = public_calls.join_next().await {
        let response = request_status(result.expect("concurrent TCP probe task joins"), 200);
        assert_eq!(read_json(&response), serde_json::json!({"status":"ok"}));
    }

    pressured.signal_terminate().await;
    let draining = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) =
                send(pressured.address, "GET", "/api/health/ready", None, "").await
                && response.status == 503
            {
                break response;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("readiness fails while the listener serves its withdrawal period");
    assert_eq!(draining.body, br#"{"status":"unavailable"}"#);
    assert_eq!(
        request_status(
            send(pressured.address, "GET", "/api/health/live", None, "").await,
            200,
        )
        .status,
        200,
        "liveness remains available during readiness withdrawal"
    );
    let pressure_output = pressured.stop_and_capture().await;
    drop(held_body);
    assert!(pressure_output.exit_success);
    assert!(
        pressure_output.shutdown_elapsed < Duration::from_secs(60),
        "stalled HTTP body, bounded analytics, and exporter work exit within the one total deadline"
    );
    assert!(stalled.count("/v1/traces") <= 6);
    assert!(stalled.count("/v1/logs") <= 6);
    assert!(stalled.count("/v1/traces") > 0 && stalled.count("/v1/logs") > 0);
    let diagnostic_count = pressure_output.stdout.matches("otel_export_failed").count();
    assert!(
        diagnostic_count > 0,
        "stalled exporter emits a sanitized stdout diagnostic"
    );
    assert!(
        diagnostic_count <= 7,
        "exporter diagnostic path is rate-limited with bounded state"
    );
    assert!(
        pressure_output.stdout.contains("otel_export_queue_full"),
        "bounded trace/log queues report queue-full through the sanitized diagnostic path"
    );
    stalled.stop().await;

    // All signals carry one allowlisted process resource; the three serving
    // instances are independently generated and differ across replacement.
    let final_signals = wait_for_capture(
        &capture_dir,
        |signals| {
            signals.spans.iter().any(|span| {
                resource_attribute(&span.resource, "service.instance.id")
                    .is_some_and(|id| id != zero_instance_id)
            }) && !signals.metrics.is_empty()
                && !signals.logs.is_empty()
        },
        "application spans, metrics, and LogRecords",
    )
    .await;
    assert!(!final_signals.spans.is_empty());
    assert!(!final_signals.metrics.is_empty());
    assert!(!final_signals.logs.is_empty());

    let sampled_span = final_signals
        .spans
        .iter()
        .find(|span| {
            span.span.get("traceId").and_then(Value::as_str) == Some(remote_trace_id)
                && request_id_in_span(&span.span) == Some(redirect_request_id.as_str())
        })
        .expect("sampled W3C remote parent exports its correlated HTTP span");
    assert_eq!(sampled_span.span["parentSpanId"], remote_span_id);
    assert_eq!(
        resource_attribute(&sampled_span.resource, "service.name"),
        Some("ferl")
    );
    assert_eq!(
        resource_attribute(&sampled_span.resource, "service.version"),
        Some(env!("CARGO_PKG_VERSION"))
    );
    let sampled_instance_id = resource_attribute(&sampled_span.resource, "service.instance.id")
        .expect("instance ID is a resource attribute")
        .to_owned();
    Uuid::parse_str(&sampled_instance_id).expect("instance ID is an opaque UUID");
    let correlated_log = final_signals
        .logs
        .iter()
        .find(|entry| {
            data_string(&entry.log["attributes"], "request_id")
                == Some(redirect_request_id.as_str())
        })
        .expect("request completion exists as an actual OTLP LogRecord");
    assert_eq!(correlated_log.log["traceId"], sampled_span.span["traceId"]);
    assert_eq!(correlated_log.log["spanId"], sampled_span.span["spanId"]);

    assert!(!final_signals.spans.iter().any(|span| {
        span.span.get("traceId").and_then(Value::as_str) == Some(unsampled_trace_id)
    }));
    assert!(final_signals.logs.iter().any(|entry| {
        entry.log.get("traceId").and_then(Value::as_str) == Some(unsampled_trace_id)
    }));
    assert!(
        final_signals.spans.iter().any(|span| {
            span.span.get("traceId").and_then(Value::as_str) != Some(remote_trace_id)
                && span.span.get("parentSpanId").is_none_or(Value::is_null)
        }),
        "malformed remote context is ignored and the request becomes a root span"
    );

    let mut service_instances = BTreeSet::new();
    for span in &final_signals.spans {
        if resource_attribute(&span.resource, "service.name") == Some("ferl") {
            service_instances.insert(
                resource_attribute(&span.resource, "service.instance.id")
                    .unwrap()
                    .to_owned(),
            );
            assert_safe_span(span);
        }
    }
    for log in &final_signals.logs {
        if resource_attribute(&log.resource, "service.name") == Some("ferl") {
            service_instances.insert(
                resource_attribute(&log.resource, "service.instance.id")
                    .unwrap()
                    .to_owned(),
            );
            assert_safe_log(log);
        }
    }
    for metric in &final_signals.metrics {
        if resource_attribute(&metric.resource, "service.name") == Some("ferl") {
            service_instances.insert(
                resource_attribute(&metric.resource, "service.instance.id")
                    .unwrap()
                    .to_owned(),
            );
            assert_safe_metric(metric);
        }
    }
    assert!(service_instances.contains(&withdrawal_instance_id));
    assert!(
        service_instances.len() >= 5,
        "A, B, replacement C, root-zero D, and withdrawal E have unique instances: {service_instances:?}"
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.http.server.request.count")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.http.server.request.duration")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.http.server.active_requests")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.redirect.outcomes")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.link.creation.outcomes")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.link.creation.rate_rejections")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.authentication.failures")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.database.operation.duration")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.database.operation.failures")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.analytics.events")
    );
    assert!(
        final_signals
            .metrics
            .iter()
            .any(|metric| metric.name == "ferl.analytics.queue.depth")
    );
    for (name, label) in [
        ("ferl.http.server.request.count", "HTTP request counter"),
        ("ferl.redirect.outcomes", "redirect outcomes"),
        ("ferl.link.creation.outcomes", "link creation outcomes"),
        (
            "ferl.link.creation.rate_rejections",
            "rate rejection counter",
        ),
        (
            "ferl.authentication.failures",
            "authentication failure counter",
        ),
        (
            "ferl.database.operation.duration",
            "database latency histogram",
        ),
        (
            "ferl.database.operation.failures",
            "database failure counter",
        ),
        ("ferl.analytics.events", "analytics event counter"),
        ("ferl.telemetry.log_records", "sanitized LogRecord counter"),
    ] {
        assert!(
            metric_samples(&final_signals, name) > 0,
            "actual exported data points carry a nonzero {label}"
        );
    }

    // Privacy scan happens only after proving nonempty actual signals/diagnostics.
    let stdout = [
        first_output.stdout,
        second_output.stdout,
        replacement_output.stdout,
        zero_output.stdout,
        withdrawal_output.stdout,
        stdout_only_output.stdout,
        outage_output.stdout,
        pressure_output.stdout,
    ]
    .join("\n");
    let stderr = [
        first_output.stderr,
        second_output.stderr,
        replacement_output.stderr,
        zero_output.stderr,
        withdrawal_output.stderr,
        stdout_only_output.stderr,
        outage_output.stderr,
        pressure_output.stderr,
    ]
    .join("\n");
    assert!(
        stdout.contains("otel_export_failed"),
        "outage uses sanitized exporter diagnostic path"
    );
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<Value>(line).expect("production stdout remains structured JSON");
    }
    let capture_text = fs::read_dir(&capture_dir)
        .expect("capture directory remains available")
        .filter_map(Result::ok)
        .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
        .collect::<String>();
    let canaries = [
        "ferl_ak_v1_",
        "ferl_mt_v1_",
        "database-url-secret-canary",
        "destination-canary.invalid",
        "destination-query-canary",
        "historical-destination-canary.invalid",
        "historical-query-canary",
        "anonymous-destination-canary.invalid",
        "referrer-canary.invalid",
        "referrer-query-canary",
        "ferl-user-agent-canary",
        "203.0.113.77",
        "forwarded-header-canary",
        "title-canary-private",
        "tag-canary-private",
        "search-canary-private",
        "cursor-canary-private",
        "tracestate-canary-private",
        "baggage-canary-private",
        "baggage-canary-unsampled",
        "malformed-traceparent-canary",
        "collector-header-secret-canary",
        &cursor_key,
        &admin_key,
        &user_key,
        &management_token,
        &invalid_management_token,
        &invalid_key,
        &digest_hex(&admin_key),
        &digest_hex(&user_key),
    ];
    for canary in canaries {
        assert!(!stdout.contains(canary), "stdout leaked canary {canary}");
        assert!(!stderr.contains(canary), "stderr leaked canary {canary}");
        assert!(
            !capture_text.contains(canary),
            "OTLP capture leaked canary {canary}"
        );
    }
    assert!(
        !capture_text.contains("otel_export_failed"),
        "sanitized exporter diagnostics use stdout only and are excluded from OTLP"
    );

    eprintln!(
        "Part 10 observed shutdown milliseconds: A={} B={} replacement-C={} root-zero-D={} no-endpoint={} DB-outage={} withdrawal={} stalled-HTTP+exporter={}; readiness-withdrawal={} listener-removal={}",
        first_output.shutdown_elapsed.as_millis(),
        second_output.shutdown_elapsed.as_millis(),
        replacement_output.shutdown_elapsed.as_millis(),
        zero_output.shutdown_elapsed.as_millis(),
        stdout_only_output.shutdown_elapsed.as_millis(),
        outage_output.shutdown_elapsed.as_millis(),
        withdrawal_output.shutdown_elapsed.as_millis(),
        pressure_output.shutdown_elapsed.as_millis(),
        readiness_withdrawal_observed.as_millis(),
        listener_removal_observed.as_millis(),
    );

    collector_command(&run_id, &collector_port, "stop").await;
}

fn digest_hex(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn started_instance_id(stdout: &str) -> String {
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["fields"]["category"].as_str() == Some("server_started")
            && let Some(instance) = value["fields"]["instance_id"].as_str()
        {
            return instance.to_owned();
        }
    }
    panic!("production startup stdout contains its generated instance UUID; output={stdout}");
}

fn assert_safe_span(span: &ExportedSpan) {
    let allowed_resource = [
        "service.name",
        "service.version",
        "service.instance.id",
        "deployment.environment.name",
        "deployment.name",
        "service.namespace",
        "cloud.region",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    for attribute in span.resource["attributes"].as_array().into_iter().flatten() {
        let key = attribute["key"].as_str().unwrap_or_default();
        assert!(
            allowed_resource.contains(key),
            "unexpected trace resource key: {key}"
        );
    }
    let name = span.span["name"].as_str().unwrap_or_default();
    if name == "http.server.request" {
        let allowed_span = ["request_id", "method", "route", "otel.kind"]
            .into_iter()
            .collect::<BTreeSet<_>>();
        for attribute in span.span["attributes"].as_array().into_iter().flatten() {
            let key = attribute["key"].as_str().unwrap_or_default();
            assert!(allowed_span.contains(key), "unexpected span key: {key}");
        }
    } else {
        let allowed_names = [
            "credential.authenticate",
            "redirect.follow",
            "account.current",
            "account.get",
            "account.list",
            "account.create",
            "account.update",
            "account.delete",
            "credential.rotate_account_key",
            "credential.reset_account_key",
            "link.create",
            "link.list",
            "link.get",
            "link.update",
            "link.delete",
            "credential.rotate_management_token",
            "db.account.shared_authority_guard",
            "db.account.exclusive_authority_guard",
            "db.link.anonymous_authority_guard",
            "db.analytics.persist_visit",
            "analytics.process_visit",
            "db.report.authorize",
            "db.report.visits",
            "db.report.summary",
            "db.report.breakdown",
        ];
        assert!(
            allowed_names.contains(&name),
            "unexpected app span name {name}"
        );
        assert!(span.span["attributes"].as_array().is_none_or(Vec::is_empty));
    }
}

fn assert_safe_log(log: &ExportedLog) {
    let allowed_log = [
        "category",
        "request_id",
        "trace_id",
        "span_id",
        "operation",
        "outcome",
        "method",
        "route",
        "status_code",
        "status_class",
        "sampled",
        "setting",
        "schema_compatible",
        "queue_depth",
        "retry",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    for attribute in log.log["attributes"].as_array().into_iter().flatten() {
        let key = attribute["key"].as_str().unwrap_or_default();
        assert!(
            allowed_log.contains(key),
            "unexpected exported log key: {key}"
        );
    }
    assert_eq!(log.log["body"]["stringValue"], "ferl application event");
}

fn assert_safe_metric(metric: &ExportedMetric) {
    let allowed = match metric.name.as_str() {
        "ferl.http.server.request.count" | "ferl.http.server.request.duration" => {
            ["method", "route", "status_class", "outcome"].as_slice()
        }
        "ferl.http.server.active_requests" => ["method", "route"].as_slice(),
        "ferl.redirect.outcomes" | "ferl.link.creation.outcomes" => {
            ["operation", "outcome"].as_slice()
        }
        "ferl.authentication.failures" => ["category"].as_slice(),
        "ferl.database.operation.duration" | "ferl.database.operation.failures" => {
            ["operation", "outcome"].as_slice()
        }
        "ferl.analytics.events" => ["phase", "outcome"].as_slice(),
        "ferl.telemetry.log_records" => ["severity"].as_slice(),
        "ferl.link.creation.rate_rejections" | "ferl.analytics.queue.depth" => [].as_slice(),
        _ => panic!("unexpected application metric name: {}", metric.name),
    };
    for attribute in metric.point["attributes"].as_array().into_iter().flatten() {
        let key = attribute["key"].as_str().unwrap_or_default();
        assert!(
            allowed.contains(&key),
            "unexpected metric dimension {} on {}",
            key,
            metric.name
        );
    }
}
