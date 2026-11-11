#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/podman_server.rs"]
#[allow(dead_code)]
mod podman_server;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use serde_json::Value;
use tokio::{process::Command as TokioCommand, time::timeout};
use url::Url;
use uuid::Uuid;

use podman_server::{ContainerServer, IMAGE};
use serving_process::send;

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const HARNESS_LABEL: &str = "io.ferl.hardening";
const HARNESS_VALUE: &str = "post-release-tls";
const COLLECTOR_IMAGE: &str = "docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b";
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
static FIXTURE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TlsDirectory {
    path: PathBuf,
}

impl TlsDirectory {
    fn new(run_id: &str) -> Self {
        let path = std::env::current_dir()
            .expect("repository working directory is available")
            .join("target/tls-verification")
            .join(run_id);
        fs::create_dir_all(&path).expect("run-isolated TLS fixture directory is created");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("private-key fixture directory is owner-only during generation");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TlsDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct OwnedPodmanRun {
    network: String,
    network_created: bool,
    containers: Vec<(String, &'static str, &'static str)>,
    cleaned: bool,
}

impl OwnedPodmanRun {
    fn new(run_id: &str) -> Self {
        Self {
            network: format!("ferl-hardening-tls-{run_id}"),
            network_created: false,
            containers: Vec::new(),
            cleaned: false,
        }
    }

    fn register_container(&mut self, name: String, label_key: &'static str, label: &'static str) {
        self.containers.push((name, label_key, label));
    }

    async fn cleanup(&mut self) {
        for (name, label_key, expected_label) in &self.containers {
            remove_owned_container(name, label_key, expected_label).await;
        }
        if self.network_created {
            remove_owned_network(&self.network, HARNESS_LABEL, HARNESS_VALUE).await;
        }
        self.cleaned = true;
    }
}

impl Drop for OwnedPodmanRun {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        // Drop is the cancellation/panic fallback. Do the bounded, label-checked
        // work synchronously so test-process exit is not mistaken for completed
        // cleanup while a detached thread or Podman client remains active.
        for (name, label_key, expected_label) in &self.containers {
            cleanup_owned_container_sync(name, label_key, expected_label);
        }
        if self.network_created {
            cleanup_owned_network_sync(&self.network, HARNESS_LABEL, HARNESS_VALUE);
        }
    }
}

async fn remove_owned_container(name: &str, label_key: &str, expected_label: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let label_format = format!("{{{{ index .Config.Labels \"{label_key}\" }}}}");
        let inspect = podman(
            &["inspect", "--format", &label_format, name],
            Duration::from_secs(5),
        )
        .await;
        if inspect.status.success() {
            assert_eq!(
                String::from_utf8_lossy(&inspect.stdout).trim(),
                expected_label,
                "refuse cleanup of an unrelated container with a colliding name"
            );
            let removed = podman(
                &["rm", "--force", "--time", "2", name],
                Duration::from_secs(10),
            )
            .await;
            assert!(
                removed.status.success(),
                "owned TLS fixture container is removed"
            );
            break;
        }
        let exists = podman(&["container", "exists", name], Duration::from_secs(5)).await;
        if !exists.status.success() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned container becomes inspectable or absent before cleanup deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let exists = podman(&["container", "exists", name], Duration::from_secs(5)).await;
    assert!(
        !exists.status.success(),
        "owned TLS container is absent before test exit"
    );
}

async fn remove_owned_network(name: &str, label_key: &str, expected_label: &str) {
    let inspect = podman(
        &[
            "network",
            "inspect",
            "--format",
            &format!("{{{{ index .Labels \"{label_key}\" }}}}"),
            name,
        ],
        Duration::from_secs(5),
    )
    .await;
    if inspect.status.success() {
        assert_eq!(
            String::from_utf8_lossy(&inspect.stdout).trim(),
            expected_label,
            "refuse cleanup of an unrelated network with a colliding name"
        );
        let removed = podman(&["network", "rm", name], Duration::from_secs(10)).await;
        assert!(
            removed.status.success(),
            "owned TLS fixture network is removed"
        );
    }
    let remains = podman(&["network", "inspect", name], Duration::from_secs(5)).await;
    assert!(
        !remains.status.success(),
        "owned TLS network is absent before test exit"
    );
}

fn cleanup_owned_container_sync(name: &str, label_key: &str, expected_label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut absent_since = None;
    let label_format = format!("{{{{ index .Config.Labels \"{label_key}\" }}}}");
    loop {
        if let Some(output) = bounded_podman_output(
            &["inspect", "--format", &label_format, name],
            Duration::from_secs(2),
        ) && output.status.success()
        {
            if String::from_utf8_lossy(&output.stdout).trim() != expected_label {
                return;
            }
            bounded_podman_cleanup(&["rm", "--force", "--time", "2", name]);
            absent_since = None;
        } else {
            let exists =
                bounded_podman_output(&["container", "exists", name], Duration::from_secs(2));
            if exists.is_some_and(|output| output.status.success()) {
                absent_since = None;
            } else {
                let absent_since = absent_since.get_or_insert_with(Instant::now);
                if absent_since.elapsed() >= Duration::from_millis(300)
                    || Instant::now() >= deadline
                {
                    return;
                }
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn cleanup_owned_network_sync(name: &str, label_key: &str, expected_label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut absent_since = None;
    let label_format = format!("{{{{ index .Labels \"{label_key}\" }}}}");
    loop {
        if let Some(output) = bounded_podman_output(
            &["network", "inspect", "--format", &label_format, name],
            Duration::from_secs(2),
        ) && output.status.success()
        {
            if String::from_utf8_lossy(&output.stdout).trim() != expected_label {
                return;
            }
            bounded_podman_cleanup(&["network", "rm", name]);
            absent_since = None;
        } else {
            let remains =
                bounded_podman_output(&["network", "inspect", name], Duration::from_secs(2));
            if remains.is_some_and(|output| output.status.success()) {
                absent_since = None;
            } else {
                let absent_since = absent_since.get_or_insert_with(Instant::now);
                if absent_since.elapsed() >= Duration::from_millis(300)
                    || Instant::now() >= deadline
                {
                    return;
                }
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test]
async fn packaged_binary_verifies_postgres_tls_and_otlp_https() {
    let _fixture_guard = FIXTURE_TEST_LOCK.lock().await;
    let binary = PathBuf::from(
        std::env::var_os("FERL_RELEASE_BINARY")
            .expect("run scripts/check-release.sh; the packaged release binary is required"),
    )
    .canonicalize()
    .expect("packaged server executable exists");
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("{}-{counter}", Uuid::new_v4().simple());
    let tls = TlsDirectory::new(&run_id);
    let mut owned = OwnedPodmanRun::new(&run_id);
    let database_name = format!("ferl-hardening-tls-pg-{run_id}");
    let collector_name = format!("ferl-hardening-tls-otel-{run_id}");
    let server_name = format!("ferl-hardening-tls-server-{run_id}");
    let untrusted_server_name = format!("ferl-hardening-tls-untrusted-{run_id}");
    let hostname_server_name = format!("ferl-hardening-tls-hostname-{run_id}");
    let migrate_cli_name = format!("ferl-hardening-tls-migrate-{run_id}");
    let untrusted_cli_name = format!("ferl-hardening-tls-untrusted-cli-{run_id}");
    let hostname_cli_name = format!("ferl-hardening-tls-hostname-cli-{run_id}");
    let bootstrap_cli_name = format!("ferl-hardening-tls-bootstrap-{run_id}");
    let port = podman_server::host_port();
    let result = AssertUnwindSafe(async {
        generate_certificates(tls.path()).await;
        verify_fixture_certificates(tls.path()).await;
        create_network(&mut owned).await;
        start_tls_postgres(&mut owned, &database_name, tls.path()).await;

        let binary_path = binary.as_path();
        let valid_database_url = database_url("pg-tls.local", "/tls/root-ca.pem");
        let migrated = release_cli(
            &mut owned,
            &migrate_cli_name,
            binary_path,
            tls.path(),
            &valid_database_url,
            &["migrate"],
        )
        .await;
        assert!(
            migrated.status.success(),
            "packaged migration verifies the trusted PostgreSQL chain and hostname; exit={:?}, diagnostic={}",
            migrated.status.code(),
            sanitized_migration_diagnostic(&migrated.stderr)
        );

        let untrusted_url = database_url("pg-tls.local", "/tls/untrusted-ca.pem");
        let untrusted = release_cli(
            &mut owned,
            &untrusted_cli_name,
            binary_path,
            tls.path(),
            &untrusted_url,
            &["migrate"],
        )
        .await;
        assert_eq!(untrusted.status.code(), Some(1), "untrusted PostgreSQL CA is rejected");
        assert!(sanitized_migration_diagnostic(&untrusted.stderr)
            .contains("migration_connection_unavailable"),
            "untrusted CA rejection reaches the client's TLS connection failure category");
        let valid_after_untrusted = release_cli(
            &mut owned,
            &format!("{migrate_cli_name}-recovery-ca"),
            binary_path,
            tls.path(),
            &valid_database_url,
            &["migrate"],
        ).await;
        assert!(valid_after_untrusted.status.success(),
            "valid PostgreSQL TLS recovery brackets untrusted-CA rejection");

        let mismatched_hostname_url = database_url("ferl-postgres", "/tls/root-ca.pem");
        let mismatched_hostname = release_cli(
            &mut owned,
            &hostname_cli_name,
            binary_path,
            tls.path(),
            &mismatched_hostname_url,
            &["migrate"],
        )
        .await;
        assert_eq!(
            mismatched_hostname.status.code(),
            Some(1),
            "PostgreSQL certificate chain trusted under the CA is rejected for a mismatched hostname"
        );
        assert!(sanitized_migration_diagnostic(&mismatched_hostname.stderr)
            .contains("migration_connection_unavailable"),
            "hostname rejection reaches the client's TLS connection failure category");
        let valid_after_hostname = release_cli(
            &mut owned,
            &format!("{migrate_cli_name}-recovery-host"),
            binary_path,
            tls.path(),
            &valid_database_url,
            &["migrate"],
        ).await;
        assert!(valid_after_hostname.status.success(),
            "valid PostgreSQL TLS recovery brackets hostname rejection");

        let bootstrap = release_cli(
            &mut owned,
            &bootstrap_cli_name,
            binary_path,
            tls.path(),
            &valid_database_url,
            &["bootstrap-admin", "--username", "tls_release_admin"],
        )
        .await;
        assert!(bootstrap.status.success(), "packaged bootstrap completes over verified PostgreSQL TLS");
        let bootstrap_result: Value = serde_json::from_slice(&bootstrap.stdout)
            .expect("bootstrap result is one JSON document");
        let api_key = bootstrap_result["api_key"]
            .as_str()
            .expect("one-time bootstrap key is returned")
            .to_owned();
        assert!(api_key.starts_with("ferl_ak_v1_"));
        assert_eq!(bootstrap.stdout.iter().filter(|byte| **byte == b'\n').count(), 1);

        start_tls_collector(&mut owned, &collector_name, tls.path()).await;
        let tls_network = owned.network.clone();
        verify_tls_fixture_paths_and_dns(&mut owned, &run_id, &tls_network, tls.path()).await;
        let mount = [(tls.path(), "/tls")];
        let valid_endpoint = "https://ferl-collector:4318";
        let valid_settings = [
            ("FERL_OTEL_ENDPOINT", valid_endpoint),
            ("FERL_OTEL_TIMEOUT_SECS", "1"),
            ("FERL_OTEL_QUEUE_CAPACITY", "64"),
            ("FERL_OTEL_BATCH_SIZE", "16"),
            ("SSL_CERT_FILE", "/tls/root-ca.pem"),
        ];
        owned.register_container(
            server_name.clone(),
            "io.ferl.harness",
            "part11-server",
        );
        let server = ContainerServer::start_with_mounts(
            binary_path,
            &server_name,
            &owned.network,
            &valid_database_url,
            CURSOR_KEY,
            "",
            port,
            &mount,
            &valid_settings,
        )
        .await;
        let identity = send(
            server.address,
            "GET",
            "/api/v1/me",
            Some(&api_key),
            "",
        )
        .await
        .expect("production API request reaches the TLS-backed server");
        assert_eq!(identity.status, 200);
        let created = send(
            server.address,
            "POST",
            "/api/v1/links",
            Some(&api_key),
            r#"{"destination_url":"https://tls-smoke.example/path","short_code":"tls-smoke"}"#,
        )
        .await
        .expect("production mutation reaches the TLS-backed server");
        assert_eq!(created.status, 201);
        let successful_exit = server.stop_naturally().await;
        assert_eq!(successful_exit.exit_code, 0);
        assert!(!successful_exit.oom_killed);
        assert!(successful_exit.shutdown_elapsed < Duration::from_secs(20));

        let capture = tls.path().join("capture/telemetry.json");
        let successful_instance = instance_id_from_logs(&successful_exit.logs);
        let (traces, metrics, logs) =
            wait_for_decoded_signals(&capture, &successful_instance, &successful_exit.logs).await;
        assert!(traces > 0, "Collector decoded actual production OTLP traces");
        assert!(metrics > 0, "Collector decoded actual production OTLP metrics");
        assert!(logs > 0, "Collector decoded actual production OTLP log records");
        println!("TLS_OTLP_RECEIVED instance={successful_instance} traces={traces} metrics={metrics} logs={logs}");
        let untrusted_settings = [
            ("FERL_OTEL_ENDPOINT", valid_endpoint),
            ("FERL_OTEL_TIMEOUT_SECS", "1"),
            ("FERL_OTEL_QUEUE_CAPACITY", "64"),
            ("FERL_OTEL_BATCH_SIZE", "16"),
            ("SSL_CERT_FILE", "/tls/untrusted-ca.pem"),
        ];
        owned.register_container(
            untrusted_server_name.clone(),
            "io.ferl.harness",
            "part11-server",
        );
        let untrusted_server = ContainerServer::start_with_mounts(
            binary_path,
            &untrusted_server_name,
            &owned.network,
            &valid_database_url,
            CURSOR_KEY,
            "",
            podman_server::host_port(),
            &mount,
            &untrusted_settings,
        )
        .await;
        let untrusted_health = send(
            untrusted_server.address,
            "GET",
            "/api/health/ready",
            None,
            "",
        )
        .await
        .expect("readiness remains available when the OTLP CA is untrusted");
        assert_eq!(untrusted_health.status, 200);
        let untrusted_identity = send(
            untrusted_server.address,
            "GET",
            "/api/v1/me",
            Some(&api_key),
            "",
        )
        .await
        .expect("request remains available when the OTLP CA is untrusted");
        assert_eq!(untrusted_identity.status, 200);
        let untrusted_exit = untrusted_server.stop_naturally().await;
        assert_eq!(untrusted_exit.exit_code, 0);
        assert!(untrusted_exit.shutdown_elapsed < Duration::from_secs(20));
        assert!(
            untrusted_exit.logs.contains("otel_export_failed"),
            "untrusted OTLP certificate is rejected by the exporter"
        );
        let untrusted_instance = instance_id_from_logs(&untrusted_exit.logs);
        let recovery_ca_instance = run_valid_otlp_recovery(
            &mut owned,
            binary_path,
            &valid_database_url,
            &api_key,
            &mount,
            &valid_settings,
            &capture,
        ).await;
        assert_ne!(untrusted_instance, recovery_ca_instance);
        assert!(!capture_has_instance(&capture, &untrusted_instance),
            "same-Collector positive recovery flushes before absence is assessed; rejected run has no received telemetry");

        let mismatched_endpoint = "https://collector-wrong-name:4318";
        let hostname_settings = [
            ("FERL_OTEL_ENDPOINT", mismatched_endpoint),
            ("FERL_OTEL_TIMEOUT_SECS", "1"),
            ("FERL_OTEL_QUEUE_CAPACITY", "64"),
            ("FERL_OTEL_BATCH_SIZE", "16"),
            ("SSL_CERT_FILE", "/tls/root-ca.pem"),
        ];
        owned.register_container(
            hostname_server_name.clone(),
            "io.ferl.harness",
            "part11-server",
        );
        let hostname_server = ContainerServer::start_with_mounts(
            binary_path,
            &hostname_server_name,
            &owned.network,
            &valid_database_url,
            CURSOR_KEY,
            "",
            podman_server::host_port(),
            &mount,
            &hostname_settings,
        )
        .await;
        let hostname_health = send(
            hostname_server.address,
            "GET",
            "/api/health/ready",
            None,
            "",
        )
        .await
        .expect("readiness is independent of OTLP certificate hostname validation");
        assert_eq!(hostname_health.status, 200);
        let hostname_identity = send(
            hostname_server.address,
            "GET",
            "/api/v1/me",
            Some(&api_key),
            "",
        )
        .await
        .expect("request remains available despite OTLP hostname rejection");
        assert_eq!(hostname_identity.status, 200);
        let hostname_exit = hostname_server.stop_naturally().await;
        assert_eq!(hostname_exit.exit_code, 0);
        assert!(hostname_exit.shutdown_elapsed < Duration::from_secs(20));
        assert!(
            hostname_exit.logs.contains("otel_export_failed"),
            "OTLP certificate hostname mismatch is rejected"
        );
        let hostname_instance = instance_id_from_logs(&hostname_exit.logs);
        let recovery_host_instance = run_valid_otlp_recovery(
            &mut owned,
            binary_path,
            &valid_database_url,
            &api_key,
            &mount,
            &valid_settings,
            &capture,
        ).await;
        assert_ne!(hostname_instance, recovery_host_instance);
        assert!(!capture_has_instance(&capture, &hostname_instance),
            "same-Collector positive recovery flushes before hostname-rejection absence is assessed");

        assert!(!successful_exit.logs.contains(&api_key));
        assert!(!untrusted_exit.logs.contains(&api_key));
        assert!(!hostname_exit.logs.contains(&api_key));
        let database_url_canary = valid_database_url.as_str();
        let privacy_canaries = [
            api_key.as_str(),
            "https://tls-smoke.example/path",
            database_url_canary,
            valid_endpoint,
            mismatched_endpoint,
        ];
        for output in [&successful_exit.logs, &untrusted_exit.logs, &hostname_exit.logs] {
            assert_privacy_canaries(output.as_bytes(), &privacy_canaries);
        }
        assert_privacy_canaries(&fs::read(&capture).unwrap_or_default(), &privacy_canaries);
    })
    .catch_unwind()
    .await;

    owned.cleanup().await;
    drop(tls);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn owned_tls_fixture_cleanup_completes_after_cancellation_and_preserves_selected_postgres() {
    let _fixture_guard = FIXTURE_TEST_LOCK.lock().await;
    let run_id = format!("cleanup-{}", Uuid::new_v4().simple());
    let container_name = format!("ferl-hardening-tls-cancel-{run_id}");
    let container_for_task = container_name.clone();
    let postgres_before = podman(
        &[
            "inspect",
            "--format",
            "{{.Id}}|{{.ImageDigest}}|{{range .Mounts}}{{.Name}}:{{.Destination}}{{end}}",
            database_support::selected_container_name(),
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        postgres_before.status.success(),
        "preserved Stage 01 fixture exists before cleanup regression"
    );
    let postgres_identity = String::from_utf8_lossy(&postgres_before.stdout)
        .trim()
        .to_owned();
    let volume_before = podman(
        &[
            "volume",
            "inspect",
            "--format",
            "{{.Name}}|{{.Mountpoint}}",
            database_support::selected_postgres().volume,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        volume_before.status.success(),
        "preserved Stage 01 volume exists before cleanup regression"
    );
    let volume_identity = String::from_utf8_lossy(&volume_before.stdout)
        .trim()
        .to_owned();

    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut owned = OwnedPodmanRun::new(&run_id);
        create_network(&mut owned).await;
        owned.register_container(container_for_task.clone(), HARNESS_LABEL, HARNESS_VALUE);
        let output = podman(
            &[
                "run",
                "--detach",
                "--pull=never",
                "--name",
                &container_for_task,
                "--label",
                &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
                "--network",
                &owned.network,
                "--entrypoint",
                "/bin/sleep",
                IMAGE,
                "60",
            ],
            Duration::from_secs(30),
        )
        .await;
        assert!(
            output.status.success(),
            "run-owned cancellation fixture starts"
        );
        ready_sender.send(owned.network.clone()).unwrap();
        std::future::pending::<()>().await;
    });
    let network = tokio::time::timeout(Duration::from_secs(40), ready_receiver)
        .await
        .expect("owned fixture reaches the controlled cancellation point")
        .expect("fixture task reports its owned network");
    task.abort();
    assert!(
        task.await
            .expect_err("fixture task was cancelled")
            .is_cancelled()
    );

    let remains = podman(
        &["container", "exists", &container_name],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        !remains.status.success(),
        "synchronous cancellation cleanup removed the owned container before task exit"
    );
    let network_remains = podman(&["network", "inspect", &network], Duration::from_secs(10)).await;
    assert!(
        !network_remains.status.success(),
        "synchronous cancellation cleanup removed the owned network before task exit"
    );
    let postgres_after = podman(
        &[
            "inspect",
            "--format",
            "{{.Id}}|{{.ImageDigest}}|{{range .Mounts}}{{.Name}}:{{.Destination}}{{end}}",
            database_support::selected_container_name(),
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(postgres_after.status.success());
    assert_eq!(
        String::from_utf8_lossy(&postgres_after.stdout).trim(),
        postgres_identity,
        "unrelated pre-existing PostgreSQL container and volume survive fixture cancellation"
    );
    let volume_after = podman(
        &[
            "volume",
            "inspect",
            "--format",
            "{{.Name}}|{{.Mountpoint}}",
            database_support::selected_postgres().volume,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(volume_after.status.success());
    assert_eq!(
        String::from_utf8_lossy(&volume_after.stdout).trim(),
        volume_identity,
        "unrelated Stage 01 volume identity survives fixture cancellation"
    );
}

async fn generate_certificates(directory: &Path) {
    let root_key = directory.join("root-ca.key");
    let root_cert = directory.join("root-ca.pem");
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        path(&root_key),
        "-out",
        path(&root_cert),
        "-days",
        "2",
        "-sha256",
        "-subj",
        "/CN=ferl-hardening-root",
        "-addext",
        "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
        "-addext",
        "subjectKeyIdentifier=hash",
    ])
    .await;

    let intermediate_key = directory.join("intermediate.key");
    let intermediate_csr = directory.join("intermediate.csr");
    let intermediate_cert = directory.join("intermediate.pem");
    openssl(&[
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        path(&intermediate_key),
        "-out",
        path(&intermediate_csr),
        "-subj",
        "/CN=ferl-hardening-intermediate",
    ])
    .await;
    let intermediate_extensions = directory.join("intermediate.ext");
    fs::write(
        &intermediate_extensions,
        "basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n",
    )
    .expect("intermediate CA extensions are written to the run-isolated directory");
    openssl(&[
        "x509",
        "-req",
        "-in",
        path(&intermediate_csr),
        "-CA",
        path(&root_cert),
        "-CAkey",
        path(&root_key),
        "-CAcreateserial",
        "-out",
        path(&intermediate_cert),
        "-days",
        "2",
        "-sha256",
        "-extfile",
        path(&intermediate_extensions),
    ])
    .await;

    let server_key = directory.join("server.key");
    let server_csr = directory.join("server.csr");
    let leaf_cert = directory.join("server-leaf.pem");
    openssl(&[
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        path(&server_key),
        "-out",
        path(&server_csr),
        "-subj",
        "/CN=pg-tls.local",
    ])
    .await;
    let server_extensions = directory.join("server.ext");
    fs::write(
        &server_extensions,
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:pg-tls.local,DNS:ferl-collector\nauthorityKeyIdentifier=keyid,issuer\n",
    )
    .expect("server SANs are written to the run-isolated directory");
    openssl(&[
        "x509",
        "-req",
        "-in",
        path(&server_csr),
        "-CA",
        path(&intermediate_cert),
        "-CAkey",
        path(&intermediate_key),
        "-CAcreateserial",
        "-out",
        path(&leaf_cert),
        "-days",
        "2",
        "-sha256",
        "-extfile",
        path(&server_extensions),
    ])
    .await;
    let chain = directory.join("server-chain.pem");
    let mut chain_bytes = fs::read(&leaf_cert).expect("server certificate is readable");
    chain_bytes.extend_from_slice(&fs::read(&intermediate_cert).expect("intermediate is readable"));
    fs::write(&chain, chain_bytes).expect("complete server certificate chain is written");

    let untrusted_key = directory.join("untrusted-ca.key");
    let untrusted_cert = directory.join("untrusted-ca.pem");
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        path(&untrusted_key),
        "-out",
        path(&untrusted_cert),
        "-days",
        "2",
        "-sha256",
        "-subj",
        "/CN=ferl-hardening-untrusted-root",
        "-addext",
        "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
        "-addext",
        "subjectKeyIdentifier=hash",
    ])
    .await;

    fs::set_permissions(&server_key, fs::Permissions::from_mode(0o644))
        .expect("server private key is owner-readable only on the host");
    for certificate in [&root_cert, &intermediate_cert, &chain, &untrusted_cert] {
        fs::set_permissions(certificate, fs::Permissions::from_mode(0o644))
            .expect("public test certificate is container-readable");
    }
    for private_or_temporary in [
        root_key,
        intermediate_key,
        intermediate_csr,
        intermediate_extensions,
        server_csr,
        server_extensions,
        leaf_cert,
        untrusted_key,
    ] {
        fs::remove_file(private_or_temporary)
            .expect("temporary CA signing material is removed after certificate generation");
    }
    for serial in ["root-ca.srl", "intermediate.srl"] {
        let serial = directory.join(serial);
        if serial.exists() {
            fs::remove_file(serial).expect("temporary certificate serial is removed");
        }
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755))
        .expect("fixture directory permits certificate-only container traversal");
}

async fn openssl(arguments: &[&str]) {
    let mut command = TokioCommand::new("openssl");
    command.args(arguments).kill_on_drop(true);
    let output = timeout(Duration::from_secs(20), command.output())
        .await
        .expect("short-lived TLS fixture certificate command is bounded")
        .expect("OpenSSL starts for local certificate generation");
    assert!(
        output.status.success(),
        "local TLS fixture certificate generation succeeds"
    );
}

async fn verify_fixture_certificates(directory: &Path) {
    let root_path = directory.join("root-ca.pem");
    let untrusted_path = directory.join("untrusted-ca.pem");
    let intermediate_path = directory.join("intermediate.pem");
    let chain_path = directory.join("server-chain.pem");
    let root = path(&root_path);
    let untrusted = path(&untrusted_path);
    let intermediate = path(&intermediate_path);
    let chain = path(&chain_path);
    openssl(&["x509", "-in", root, "-noout"]).await;
    for hostname in ["pg-tls.local", "ferl-collector"] {
        openssl(&[
            "verify",
            "-CAfile",
            root,
            "-untrusted",
            intermediate,
            "-verify_hostname",
            hostname,
            chain,
        ])
        .await;
    }
    openssl(&["x509", "-in", untrusted, "-noout"]).await;
    let untrusted_result = openssl_output(&[
        "verify",
        "-CAfile",
        untrusted,
        "-untrusted",
        intermediate,
        "-verify_hostname",
        "ferl-collector",
        chain,
    ])
    .await;
    assert!(
        !untrusted_result.status.success(),
        "the readable, parsed negative CA does not authenticate the intended TLS server"
    );
    let mismatched = openssl_output(&[
        "verify",
        "-CAfile",
        root,
        "-untrusted",
        intermediate,
        "-verify_hostname",
        "ferl-postgres",
        chain,
    ])
    .await;
    assert!(
        !mismatched.status.success(),
        "the parsed fixture chain is valid for both intended aliases and not the PostgreSQL mismatch alias"
    );
}

async fn openssl_output(arguments: &[&str]) -> Output {
    let mut command = TokioCommand::new("openssl");
    command.args(arguments).kill_on_drop(true);
    timeout(Duration::from_secs(20), command.output())
        .await
        .expect("OpenSSL TLS control is bounded")
        .expect("OpenSSL starts for the local fixture")
}

async fn create_network(owned: &mut OwnedPodmanRun) {
    // Set before invoking Podman so Drop can label-check and clean a network if
    // the client is cancelled after creation but before the command returns.
    owned.network_created = true;
    let output = podman(
        &[
            "network",
            "create",
            "--label",
            &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
            &owned.network,
        ],
        Duration::from_secs(30),
    )
    .await;
    assert!(output.status.success(), "run-owned TLS network is created");
}

async fn start_tls_postgres(owned: &mut OwnedPodmanRun, name: &str, tls_dir: &Path) {
    owned.register_container(name.to_owned(), HARNESS_LABEL, HARNESS_VALUE);
    let tls_volume = format!("{}:/tls:ro", tls_dir.display());
    let script = "set -eu; cp /tls/server.key /tmp/ferl-postgres.key; chown postgres:postgres /tmp/ferl-postgres.key; chmod 0600 /tmp/ferl-postgres.key; exec /usr/local/bin/docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tls/server-chain.pem -c ssl_key_file=/tmp/ferl-postgres.key";
    let output = podman(
        &[
            "run",
            "--rm",
            "--detach",
            "--pull=never",
            "--name",
            name,
            "--label",
            &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
            "--network",
            &owned.network,
            "--network-alias",
            "pg-tls.local",
            "--network-alias",
            "ferl-postgres",
            "--memory",
            "768m",
            "--cpus",
            "1",
            "--pids-limit",
            "128",
            "--tmpfs",
            &format!(
                "{}:rw,nosuid,nodev,size=256m",
                database_support::selected_postgres().mount
            ),
            "--volume",
            &tls_volume,
            "--env",
            "POSTGRES_USER=ferl",
            "--env",
            "POSTGRES_PASSWORD=ferl",
            "--env",
            "POSTGRES_DB=ferl_test",
            "--entrypoint",
            "/bin/sh",
            database_support::selected_postgres().image,
            "-ec",
            script,
        ],
        Duration::from_secs(60),
    )
    .await;
    assert!(
        output.status.success(),
        "run-isolated TLS PostgreSQL starts"
    );

    timeout(Duration::from_secs(60), async {
        loop {
            let ready = podman(
                &[
                    "exec",
                    name,
                    "pg_isready",
                    "-q",
                    "-t",
                    "1",
                    "-h",
                    "127.0.0.1",
                    "-p",
                    "5432",
                    "-U",
                    "ferl",
                    "-d",
                    "ferl_test",
                ],
                Duration::from_secs(5),
            )
            .await;
            if ready.status.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("run-isolated PostgreSQL reaches readiness before the deadline");

    let version = podman(
        &[
            "exec",
            name,
            "psql",
            "-U",
            "ferl",
            "-d",
            "ferl_test",
            "-X",
            "-tAc",
            "SHOW server_version_num",
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        version.status.success(),
        "run-isolated PostgreSQL reports its server version"
    );
    let version_num = String::from_utf8_lossy(&version.stdout)
        .trim()
        .parse::<i32>()
        .expect("PostgreSQL server_version_num is numeric");
    assert_eq!(
        version_num / 10_000,
        database_support::selected_postgres().major,
        "the packaged TLS fixture runs the selected PostgreSQL major"
    );
}

async fn start_tls_collector(owned: &mut OwnedPodmanRun, name: &str, tls_dir: &Path) {
    let capture_dir = tls_dir.join("capture");
    fs::create_dir_all(&capture_dir).expect("run-isolated OTLP capture directory exists");
    fs::set_permissions(&capture_dir, fs::Permissions::from_mode(0o777))
        .expect("collector can write its private fixture capture");
    let config = tls_dir.join("collector-tls.yaml");
    fs::write(
        &config,
        "receivers:\n  otlp:\n    protocols:\n      http:\n        endpoint: 0.0.0.0:4318\n        tls:\n          cert_file: /tls/server-chain.pem\n          key_file: /tls/server.key\nprocessors:\n  batch:\n    timeout: 1s\n    send_batch_size: 64\n    send_batch_max_size: 128\nexporters:\n  file/ferl:\n    path: /capture/telemetry.json\n    format: json\nservice:\n  telemetry:\n    logs:\n      level: warn\n  pipelines:\n    traces:\n      receivers: [otlp]\n      processors: [batch]\n      exporters: [file/ferl]\n    metrics:\n      receivers: [otlp]\n      processors: [batch]\n      exporters: [file/ferl]\n    logs:\n      receivers: [otlp]\n      processors: [batch]\n      exporters: [file/ferl]\n",
    )
    .expect("TLS Collector config is written only in the run-isolated directory");
    let tls_volume = format!("{}:/tls:ro", tls_dir.display());
    let capture_volume = format!("{}:/capture:rw", capture_dir.display());
    let config_volume = format!("{}:/etc/otelcol-contrib/config.yaml:ro", config.display());
    owned.register_container(name.to_owned(), HARNESS_LABEL, HARNESS_VALUE);
    let output = podman(
        &[
            "run",
            "--detach",
            "--pull=never",
            "--name",
            name,
            "--label",
            &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
            "--network",
            &owned.network,
            "--network-alias",
            "ferl-collector",
            "--network-alias",
            "collector-wrong-name",
            "--user",
            "0:0",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=32m",
            "--memory",
            "512m",
            "--cpus",
            "1",
            "--pids-limit",
            "128",
            "--volume",
            &tls_volume,
            "--volume",
            &capture_volume,
            "--volume",
            &config_volume,
            COLLECTOR_IMAGE,
            "--config=/etc/otelcol-contrib/config.yaml",
        ],
        Duration::from_secs(60),
    )
    .await;
    assert!(output.status.success(), "pinned TLS Collector starts");
    timeout(Duration::from_secs(20), async {
        loop {
            let status = podman(
                &["inspect", "--format", "{{.State.Running}}", name],
                Duration::from_secs(5),
            )
            .await;
            if status.status.success() && String::from_utf8_lossy(&status.stdout).trim() == "true" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("TLS Collector remains running after configuration load");
}

async fn verify_tls_fixture_paths_and_dns(
    owned: &mut OwnedPodmanRun,
    run_id: &str,
    network: &str,
    tls_dir: &Path,
) {
    let name = format!("ferl-hardening-tls-preflight-{run_id}");
    owned.register_container(name.clone(), HARNESS_LABEL, HARNESS_VALUE);
    let tls_volume = format!("{}:/tls:ro", tls_dir.display());
    let checks = "test -s /tls/root-ca.pem && test -r /tls/root-ca.pem && test -s /tls/untrusted-ca.pem && test -r /tls/untrusted-ca.pem && test -s /tls/server-chain.pem && pg_ip=$(getent hosts pg-tls.local | awk 'NR==1 {print $1}') && test -n \"$pg_ip\" && test \"$(getent hosts ferl-postgres | awk 'NR==1 {print $1}')\" = \"$pg_ip\" && collector_ip=$(getent hosts ferl-collector | awk 'NR==1 {print $1}') && test -n \"$collector_ip\" && test \"$(getent hosts collector-wrong-name | awk 'NR==1 {print $1}')\" = \"$collector_ip\" && test \"$pg_ip\" != \"$collector_ip\"";
    let script = format!(
        "set -eu; attempt=0; until ({checks}); do attempt=$((attempt + 1)); test \"$attempt\" -lt 30 || {{ getent hosts pg-tls.local ferl-postgres ferl-collector collector-wrong-name >&2; exit 1; }}; sleep 1; done"
    );
    let output = podman(
        &[
            "run",
            "--rm",
            "--pull=never",
            "--name",
            &name,
            "--label",
            &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
            "--network",
            network,
            "--volume",
            &tls_volume,
            "--user",
            "65532:65532",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--entrypoint",
            "/bin/sh",
            IMAGE,
            "-ec",
            script.as_str(),
        ],
        Duration::from_secs(60),
    )
    .await;
    assert!(
        output.status.success(),
        "untrusted-root file and both certificate hostnames exist before negative controls: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn database_url(host: &str, ca_file: &str) -> String {
    let mut url = Url::parse(&format!("postgres://ferl:ferl@{host}:5432/ferl_test"))
        .expect("TLS PostgreSQL fixture URL parses");
    url.query_pairs_mut()
        .append_pair("sslmode", "verify-full")
        .append_pair("sslrootcert", ca_file);
    url.into()
}

async fn release_cli(
    owned: &mut OwnedPodmanRun,
    name: &str,
    binary: &Path,
    tls_dir: &Path,
    database_url: &str,
    arguments: &[&str],
) -> Output {
    owned.register_container(name.to_owned(), HARNESS_LABEL, HARNESS_VALUE);
    let network = owned.network.clone();
    let binary = binary
        .canonicalize()
        .expect("packaged release executable resolves");
    let binary_volume = format!("{}:/opt/ferl-server:ro", binary.display());
    let tls_volume = format!("{}:/tls:ro", tls_dir.display());
    let mut command = TokioCommand::new("podman");
    command
        .args([
            "run",
            "--rm",
            "--pull=never",
            "--name",
            name,
            "--label",
            &format!("{HARNESS_LABEL}={HARNESS_VALUE}"),
            "--network",
            &network,
            "--volume",
            &binary_volume,
            "--volume",
            &tls_volume,
            "--user",
            "65532:65532",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=16m",
            "--env",
        ])
        .arg(format!("FERL_DATABASE_URL={database_url}"))
        .args(["--entrypoint", "/opt/ferl-server", IMAGE])
        .args(arguments)
        .kill_on_drop(true);
    timeout(Duration::from_secs(120), command.output())
        .await
        .expect("packaged PostgreSQL TLS CLI invocation is bounded")
        .expect("Podman starts the packaged release CLI")
}

async fn podman(arguments: &[&str], limit: Duration) -> Output {
    let mut command = TokioCommand::new("podman");
    command.args(arguments).kill_on_drop(true);
    timeout(limit, command.output())
        .await
        .unwrap_or_else(|_| panic!("run-owned TLS Podman operation exceeds its bound"))
        .expect("Podman command starts")
}

async fn run_valid_otlp_recovery(
    owned: &mut OwnedPodmanRun,
    binary: &Path,
    database_url: &str,
    api_key: &str,
    mounts: &[(&Path, &str)],
    settings: &[(&str, &str)],
    capture: &Path,
) -> String {
    let name = format!("ferl-tls-recovery-{}", Uuid::new_v4().simple());
    owned.register_container(name.clone(), "io.ferl.harness", "part11-server");
    let server = ContainerServer::start_with_mounts(
        binary,
        &name,
        &owned.network,
        database_url,
        CURSOR_KEY,
        "",
        podman_server::host_port(),
        mounts,
        settings,
    )
    .await;
    let identity = send(server.address, "GET", "/api/v1/me", Some(api_key), "")
        .await
        .expect("positive OTLP recovery reaches the same production fixture");
    assert_eq!(identity.status, 200);
    let exit = server.stop_naturally().await;
    assert_eq!(exit.exit_code, 0);
    assert!(exit.shutdown_elapsed < Duration::from_secs(20));
    assert_privacy_canaries(
        exit.logs.as_bytes(),
        &[api_key, database_url, "https://tls-smoke.example/path"],
    );
    let instance = instance_id_from_logs(&exit.logs);
    let counts = wait_for_decoded_signals(capture, &instance, &exit.logs).await;
    assert!(
        counts.0 > 0 && counts.1 > 0 && counts.2 > 0,
        "same-Collector recovery receives this process's decoded traces, metrics, and logs"
    );
    instance
}

fn instance_id_from_logs(logs: &str) -> String {
    logs.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            (value["fields"]["category"] == "server_started")
                .then(|| value["fields"]["instance_id"].as_str().map(str::to_owned))
                .flatten()
        })
        .expect("production startup logs its run-specific instance UUID")
}

async fn wait_for_decoded_signals(
    path: &Path,
    instance_id: &str,
    logs: &str,
) -> (usize, usize, usize) {
    let waited = timeout(Duration::from_secs(20), async {
        loop {
            let counts = fs::read_to_string(path)
                .ok()
                .map(|contents| decode_signal_counts(&contents, instance_id))
                .unwrap_or_default();
            if counts.0 > 0 && counts.1 > 0 && counts.2 > 0 {
                return counts;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    match waited {
        Ok(counts) => counts,
        Err(_) => {
            let capture = fs::read_to_string(path).unwrap_or_default();
            let telemetry_lines: Vec<&str> = logs
                .lines()
                .filter(|line| {
                    line.contains("otlp") || line.contains("telemetry") || line.contains("export")
                })
                .take(12)
                .collect();
            let collectors = std::process::Command::new("podman")
                .args([
                    "ps",
                    "-a",
                    "--filter",
                    "name=ferl-hardening-tls-otel",
                    "--format",
                    "{{.Names}}",
                ])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default();
            let mut collector_logs = String::new();
            for name in collectors.lines().filter(|name| !name.is_empty()) {
                let output = std::process::Command::new("podman")
                    .args(["logs", "--tail", "60", name])
                    .output();
                if let Ok(output) = output {
                    collector_logs.push_str(&String::from_utf8_lossy(&output.stdout));
                    collector_logs.push_str(&String::from_utf8_lossy(&output.stderr));
                }
            }
            panic!(
                "Collector receives and decodes each OTLP signal before the deadline: capture_bytes={} counts={:?} server_telemetry_lines={:?} collector_logs={:?}",
                capture.len(),
                decode_signal_counts(&capture, instance_id),
                telemetry_lines,
                collector_logs
            );
        }
    }
}

fn decode_signal_counts(capture: &str, instance_id: &str) -> (usize, usize, usize) {
    let mut traces = 0;
    let mut metrics = 0;
    let mut logs = 0;
    for line in capture.lines() {
        let Ok(document) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        traces += signal_resources(
            &document,
            "resourceSpans",
            "scopeSpans",
            "spans",
            instance_id,
        );
        metrics += signal_resources(
            &document,
            "resourceMetrics",
            "scopeMetrics",
            "metrics",
            instance_id,
        );
        logs += signal_resources(
            &document,
            "resourceLogs",
            "scopeLogs",
            "logRecords",
            instance_id,
        );
    }
    (traces, metrics, logs)
}

fn signal_resources(
    document: &Value,
    resource_key: &str,
    scope_key: &str,
    data_key: &str,
    instance_id: &str,
) -> usize {
    document
        .get(resource_key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|resource| {
            let attributes = resource["resource"]["attributes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let service_is_ferl = attributes.iter().any(|attribute| {
                attribute["key"] == "service.name" && attribute["value"]["stringValue"] == "ferl"
            });
            let belongs_to_run = attributes.iter().any(|attribute| {
                attribute["key"] == "service.instance.id"
                    && attribute["value"]["stringValue"] == instance_id
            });
            if !service_is_ferl || !belongs_to_run {
                return 0;
            }
            resource[scope_key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|scope| {
                    scope[data_key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|record| meaningful_record(data_key, record))
                        .count()
                })
                .sum::<usize>()
        })
        .sum()
}

fn meaningful_record(signal: &str, record: &Value) -> bool {
    match signal {
        "spans" => {
            record["name"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                && record["traceId"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
                && record["spanId"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
        }
        "metrics" => {
            record["name"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                && [
                    "gauge",
                    "sum",
                    "histogram",
                    "exponentialHistogram",
                    "summary",
                ]
                .iter()
                .any(|kind| {
                    record[*kind]["dataPoints"]
                        .as_array()
                        .is_some_and(|points| !points.is_empty())
                })
        }
        "logRecords" => {
            let body = &record["body"];
            body["stringValue"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                || body["kvlistValue"]["values"]
                    .as_array()
                    .is_some_and(|values| !values.is_empty())
                || body["arrayValue"]["values"]
                    .as_array()
                    .is_some_and(|values| !values.is_empty())
        }
        _ => false,
    }
}

fn capture_has_instance(path: &Path, instance_id: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|contents| decode_signal_counts(&contents, instance_id) != (0, 0, 0))
}

fn assert_privacy_canaries(payload: &[u8], canaries: &[&str]) {
    for canary in canaries {
        assert!(
            !payload
                .windows(canary.len())
                .any(|window| window == canary.as_bytes()),
            "TLS process/Collector payload excludes sensitive canaries"
        );
    }
}

fn path(path: &Path) -> &str {
    path.to_str().expect("temporary fixture paths are UTF-8")
}

fn sanitized_migration_diagnostic(stderr: &[u8]) -> &str {
    std::str::from_utf8(stderr)
        .ok()
        .and_then(|output| {
            output
                .lines()
                .find(|line| line.starts_with("migrate failed ("))
        })
        .unwrap_or("no sanitized migration category emitted")
}

fn bounded_podman_cleanup(arguments: &[&str]) -> bool {
    bounded_podman_output(arguments, Duration::from_secs(4))
        .is_some_and(|output| output.status.success())
}

fn bounded_podman_output(arguments: &[&str], bound: Duration) -> Option<Output> {
    let mut child = Command::new("podman")
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + bound;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    }?;
    Some(Output {
        status,
        stdout: reader.join().ok()?,
        stderr: Vec::new(),
    })
}
