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
const POSTGRES_IMAGE: &str = "docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94";
const COLLECTOR_IMAGE: &str = "docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b";
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

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
            let inspect = podman(
                &[
                    "inspect",
                    "--format",
                    &format!("{{{{ index .Config.Labels \"{label_key}\" }}}}"),
                    name,
                ],
                Duration::from_secs(10),
            )
            .await;
            if inspect.status.success()
                && String::from_utf8_lossy(&inspect.stdout).trim() == *expected_label
            {
                let removed = podman(
                    &["rm", "--force", "--time", "2", name],
                    Duration::from_secs(15),
                )
                .await;
                assert!(
                    removed.status.success(),
                    "owned TLS fixture container is removed"
                );
            }
        }
        if self.network_created {
            let inspect = podman(
                &[
                    "network",
                    "inspect",
                    "--format",
                    &format!("{{{{ index .Labels \"{HARNESS_LABEL}\" }}}}"),
                    &self.network,
                ],
                Duration::from_secs(10),
            )
            .await;
            if inspect.status.success()
                && String::from_utf8_lossy(&inspect.stdout).trim() == HARNESS_VALUE
            {
                let removed =
                    podman(&["network", "rm", &self.network], Duration::from_secs(15)).await;
                assert!(
                    removed.status.success(),
                    "owned TLS fixture network is removed"
                );
            }
        }
        self.cleaned = true;
    }
}

impl Drop for OwnedPodmanRun {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let network = self.network.clone();
        let network_created = self.network_created;
        let containers = self.containers.clone();
        let _ = thread::Builder::new()
            .name("ferl-tls-fixture-cleanup".to_owned())
            .spawn(move || {
                for (name, label_key, expected_label) in containers {
                    let label_format = format!("{{{{ index .Config.Labels \"{label_key}\" }}}}");
                    if bounded_podman_output(&["inspect", "--format", &label_format, &name])
                        .is_some_and(|output| {
                            output.status.success()
                                && String::from_utf8_lossy(&output.stdout).trim() == expected_label
                        })
                    {
                        bounded_podman_cleanup(&["rm", "--force", "--time", "2", &name]);
                    }
                }
                let network_label_format = format!("{{{{ index .Labels \"{HARNESS_LABEL}\" }}}}");
                if network_created
                    && bounded_podman_output(&[
                        "network",
                        "inspect",
                        "--format",
                        &network_label_format,
                        &network,
                    ])
                    .is_some_and(|output| {
                        output.status.success()
                            && String::from_utf8_lossy(&output.stdout).trim() == HARNESS_VALUE
                    })
                {
                    bounded_podman_cleanup(&["network", "rm", &network]);
                }
            });
    }
}

#[tokio::test]
async fn packaged_binary_verifies_postgres_tls_and_otlp_https() {
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
        let (traces, metrics, logs) = wait_for_decoded_signals(&capture).await;
        assert!(traces > 0, "Collector decoded actual production OTLP traces");
        assert!(metrics > 0, "Collector decoded actual production OTLP metrics");
        assert!(logs > 0, "Collector decoded actual production OTLP log records");
        println!("TLS_OTLP_RECEIVED traces={traces} metrics={metrics} logs={logs}");

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let before_invalid_exports = fs::read(&capture).unwrap_or_default();
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
        assert_eq!(
            fs::read(&capture).unwrap_or_default(),
            before_invalid_exports,
            "Collector receives no OTLP payload from a client rejecting its certificate"
        );

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
        assert_eq!(
            fs::read(&capture).unwrap_or_default(),
            before_invalid_exports,
            "Collector receives no OTLP payload from a hostname-mismatched client"
        );

        assert!(!successful_exit.logs.contains(&api_key));
        assert!(!untrusted_exit.logs.contains(&api_key));
        assert!(!hostname_exit.logs.contains(&api_key));
    })
    .catch_unwind()
    .await;

    owned.cleanup().await;
    drop(tls);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
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

    fs::set_permissions(&server_key, fs::Permissions::from_mode(0o600))
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
            "/var/lib/postgresql/data:rw,nosuid,nodev,size=256m",
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
            POSTGRES_IMAGE,
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
}

async fn start_tls_collector(owned: &mut OwnedPodmanRun, name: &str, tls_dir: &Path) {
    let capture_dir = tls_dir.join("capture");
    fs::create_dir_all(&capture_dir).expect("run-isolated OTLP capture directory exists");
    fs::set_permissions(&capture_dir, fs::Permissions::from_mode(0o755))
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
    let script = "set -eu; test -s /tls/root-ca.pem; test -s /tls/untrusted-ca.pem; test -s /tls/server-chain.pem; for host in pg-tls.local ferl-postgres ferl-collector collector-wrong-name; do getent hosts \"$host\" >/dev/null; done";
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
            script,
        ],
        Duration::from_secs(30),
    )
    .await;
    assert!(
        output.status.success(),
        "untrusted-root file and both certificate hostnames exist before negative controls"
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

async fn wait_for_decoded_signals(path: &Path) -> (usize, usize, usize) {
    timeout(Duration::from_secs(20), async {
        loop {
            let counts = fs::read_to_string(path)
                .ok()
                .map(|contents| decode_signal_counts(&contents))
                .unwrap_or_default();
            if counts.0 > 0 && counts.1 > 0 && counts.2 > 0 {
                return counts;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Collector receives and decodes each OTLP signal before the deadline")
}

fn decode_signal_counts(capture: &str) -> (usize, usize, usize) {
    let mut traces = 0;
    let mut metrics = 0;
    let mut logs = 0;
    for line in capture.lines() {
        let Ok(document) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        traces += signal_resources(&document, "resourceSpans", "scopeSpans", "spans");
        metrics += signal_resources(&document, "resourceMetrics", "scopeMetrics", "metrics");
        logs += signal_resources(&document, "resourceLogs", "scopeLogs", "logRecords");
    }
    (traces, metrics, logs)
}

fn signal_resources(
    document: &Value,
    resource_key: &str,
    scope_key: &str,
    data_key: &str,
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
            if !service_is_ferl {
                return 0;
            }
            resource[scope_key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|scope| scope[data_key].as_array().map_or(0, Vec::len))
                .sum::<usize>()
        })
        .sum()
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

fn bounded_podman_cleanup(arguments: &[&str]) {
    let _ = bounded_podman_output(arguments);
}

fn bounded_podman_output(arguments: &[&str]) -> Option<Output> {
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
    let deadline = Instant::now() + Duration::from_secs(20);
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
