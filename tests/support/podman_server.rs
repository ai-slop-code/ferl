use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use tokio::{process::Command, time::timeout};
use url::Url;

use super::serving_process::send;

#[cfg(target_arch = "aarch64")]
pub const IMAGE: &str = "docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75";
#[cfg(target_arch = "x86_64")]
pub const IMAGE: &str = "docker.io/library/rust@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8";
pub const NGINX_IMAGE: &str = "docker.io/library/nginx:1.30.6-alpine@sha256:47928bf67eb62a1863cb08d9ecb52d3e704304ba614eaee17884204677702951";
pub const POSTGRES_CONTAINER: &str = "ferl-stage01-postgres";

pub struct ContainerServer {
    pub name: String,
    pub address: SocketAddr,
}

pub struct ExitedServer {
    pub logs: String,
    pub exit_code: i32,
    pub shutdown_elapsed: Duration,
    pub oom_killed: bool,
}

pub struct NetworkClient {
    pub name: String,
    pub id: String,
    pub ip: IpAddr,
}

impl NetworkClient {
    pub async fn start(name: &str, network: &str) -> Self {
        let output = command_output(
            "podman",
            &[
                "run",
                "--detach",
                "--pull=never",
                "--name",
                name,
                "--label",
                "io.ferl.harness=part11-client",
                "--network",
                network,
                "--entrypoint",
                "/bin/sh",
                NGINX_IMAGE,
                "-ec",
                "sleep 600",
            ],
            Duration::from_secs(30),
        )
        .await;
        assert!(output.status.success(), "run-owned network client starts");

        let id = command_output(
            "podman",
            &["inspect", "--format", "{{.Id}}", name],
            Duration::from_secs(10),
        )
        .await;
        assert!(
            id.status.success(),
            "Podman identifies the run-owned client container"
        );
        let networks = command_output(
            "podman",
            &[
                "inspect",
                "--format",
                "{{json .NetworkSettings.Networks}}",
                name,
            ],
            Duration::from_secs(10),
        )
        .await;
        assert!(
            networks.status.success(),
            "Podman reports the client network attachment"
        );
        let networks: serde_json::Value =
            serde_json::from_slice(&networks.stdout).expect("Podman network inspection is JSON");
        let ip = networks[network]["IPAddress"]
            .as_str()
            .expect("run-owned client has an IP on the isolated network")
            .parse()
            .expect("Podman client network address parses");
        Self {
            name: name.to_owned(),
            id: String::from_utf8_lossy(&id.stdout).trim().to_owned(),
            ip,
        }
    }

    /// Run curl as a process in this isolated-network client container. The
    /// request is a real TCP GET; curl does not follow the backend's redirect.
    pub async fn get(
        &self,
        address: IpAddr,
        path: &str,
        marker: &str,
        spoofed_ip: IpAddr,
    ) -> Output {
        let host = match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        };
        let url = format!("http://{host}:8080{path}");
        let user_agent = format!("User-Agent: {marker}");
        let forwarded = format!("X-Forwarded-For: {spoofed_ip}");
        command_output(
            "podman",
            &[
                "exec",
                &self.name,
                "curl",
                "--silent",
                "--show-error",
                "--max-time",
                "8",
                "--dump-header",
                "-",
                "--output",
                "/dev/null",
                "--write-out",
                "\\nFERL_CLIENT_RESULT %{http_code} %{remote_ip}\\n",
                "--header",
                &user_agent,
                "--header",
                &forwarded,
                &url,
            ],
            Duration::from_secs(12),
        )
        .await
    }
}

pub async fn container_database_url(host_schema_url: &str) -> String {
    let mut url = Url::parse(host_schema_url).expect("isolated PostgreSQL URL parses");
    url.set_host(Some("ferl-postgres"))
        .expect("custom network resolves the owned database alias");
    url.set_port(Some(5432))
        .expect("container PostgreSQL uses its internal port");
    url.to_string()
}

pub async fn create_network(run_id: &str) -> String {
    let network = format!("ferl-part11-net-{run_id}");
    let created = command_output(
        "podman",
        &[
            "network",
            "create",
            "--label",
            "io.ferl.harness=part11-network",
            &network,
        ],
        Duration::from_secs(30),
    )
    .await;
    assert!(created.status.success(), "owned Podman network is created");
    let connected = command_output(
        "podman",
        &[
            "network",
            "connect",
            "--alias",
            "ferl-postgres",
            &network,
            POSTGRES_CONTAINER,
        ],
        Duration::from_secs(30),
    )
    .await;
    assert!(
        connected.status.success(),
        "only the verified PostgreSQL harness joins the isolated test network: {}",
        String::from_utf8_lossy(&connected.stderr).trim()
    );
    network
}

pub async fn remove_network(network: &str) -> Output {
    let inspect = command_output(
        "podman",
        &[
            "network",
            "inspect",
            "--format",
            "{{ index .Labels \"io.ferl.harness\" }}",
            network,
        ],
        Duration::from_secs(20),
    )
    .await;
    assert!(inspect.status.success());
    assert_eq!(
        String::from_utf8_lossy(&inspect.stdout).trim(),
        "part11-network"
    );
    let disconnected = command_output(
        "podman",
        &["network", "disconnect", network, POSTGRES_CONTAINER],
        Duration::from_secs(20),
    )
    .await;
    assert!(disconnected.status.success());
    command_output(
        "podman",
        &["network", "rm", network],
        Duration::from_secs(20),
    )
    .await
}

pub async fn migrate(binary: &Path, network: &str, database_url: &str) -> Output {
    one_shot(
        binary,
        network,
        database_url,
        &[],
        "migrate",
        Duration::from_secs(120),
    )
    .await
}

pub async fn bootstrap(binary: &Path, network: &str, database_url: &str, username: &str) -> Output {
    one_shot(
        binary,
        network,
        database_url,
        &[],
        &format!("bootstrap-admin --username {username}"),
        Duration::from_secs(60),
    )
    .await
}

async fn one_shot(
    binary: &Path,
    network: &str,
    database_url: &str,
    environment: &[(&str, &str)],
    command: &str,
    limit: Duration,
) -> Output {
    let binary = binary
        .canonicalize()
        .expect("packaged ferl-server executable resolves");
    let mut child = Command::new("podman");
    child
        .args([
            "run",
            "--rm",
            "--pull=never",
            "--network",
            network,
            "--volume",
        ])
        .arg(format!("{}:/opt/ferl-server:ro", binary.display()))
        .args([
            "--user",
            "65532:65532",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=16m",
            "--env",
            &format!("FERL_DATABASE_URL={database_url}"),
        ]);
    for (name, value) in environment {
        child.arg("--env").arg(format!("{name}={value}"));
    }
    child
        .args(["--entrypoint", "/opt/ferl-server", IMAGE])
        .args(command.split_whitespace())
        .kill_on_drop(true);
    timeout(limit, child.output())
        .await
        .unwrap_or_else(|_| panic!("packaged ferl-server {command} exceeded its deadline"))
        .expect("Podman executes packaged ferl-server command")
}

impl ContainerServer {
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        binary: &Path,
        name: &str,
        network: &str,
        database_url: &str,
        cursor_key: &str,
        trusted_proxies: &str,
        host_port: u16,
        extra: &[(&str, &str)],
    ) -> Self {
        Self::start_with_mounts(
            binary,
            name,
            network,
            database_url,
            cursor_key,
            trusted_proxies,
            host_port,
            &[],
            extra,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_mounts(
        binary: &Path,
        name: &str,
        network: &str,
        database_url: &str,
        cursor_key: &str,
        trusted_proxies: &str,
        host_port: u16,
        read_only_mounts: &[(&Path, &str)],
        extra: &[(&str, &str)],
    ) -> Self {
        let binary = binary
            .canonicalize()
            .expect("packaged ferl-server executable resolves");
        let name = name.to_owned();
        let host_port_text = host_port.to_string();
        let mut child = Command::new("podman");
        child
            .args([
                "run",
                "--detach",
                "--pull=never",
                "--name",
                &name,
                "--label",
                "io.ferl.harness=part11-server",
                "--network",
                network,
                "--network-alias",
                "ferl-backend",
                "--publish",
            ])
            .arg(format!("127.0.0.1:{host_port_text}:8080"))
            .args(["--volume"])
            .arg(format!("{}:/opt/ferl-server:ro", binary.display()))
            .args([
                "--user",
                "65532:65532",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--tmpfs",
                "/tmp:rw,noexec,nosuid,nodev,size=16m",
                "--env",
                &format!("FERL_DATABASE_URL={database_url}"),
                "--env",
                "FERL_PUBLIC_BASE_URL=https://short.example.test",
                "--env",
                &format!("FERL_CURSOR_KEY={cursor_key}"),
                "--env",
                "FERL_LISTEN_ADDR=0.0.0.0:8080",
                "--env",
                &format!("FERL_TRUSTED_PROXIES={trusted_proxies}"),
                "--env",
                "FERL_DB_POOL_MAX=4",
                "--env",
                "FERL_DB_ACQUIRE_TIMEOUT_SECS=2",
                "--env",
                "FERL_DB_LOCK_TIMEOUT_SECS=5",
                "--env",
                "FERL_HTTP_MAX_IN_FLIGHT=64",
                "--env",
                "FERL_HTTP_MAX_WAITING=32",
                "--env",
                "FERL_ANONYMOUS_CREATION=true",
                "--env",
                "FERL_ANONYMOUS_CREATE_LIMIT=100",
                "--env",
                "FERL_ACCOUNT_CREATE_LIMIT=100",
                "--env",
                "FERL_READINESS_WITHDRAWAL_SECS=1",
                "--env",
                "FERL_HTTP_DRAIN_TIMEOUT_SECS=5",
                "--env",
                "FERL_SHUTDOWN_TIMEOUT_SECS=20",
            ]);
        for (host_path, container_path) in read_only_mounts {
            child
                .args(["--volume"])
                .arg(format!("{}:{container_path}:ro", host_path.display()));
        }
        for (key, value) in extra {
            child.arg("--env").arg(format!("{key}={value}"));
        }
        child
            .args(["--entrypoint", "/opt/ferl-server", IMAGE, "serve"])
            .kill_on_drop(true);
        let output = timeout(Duration::from_secs(30), child.output())
            .await
            .expect("Podman server container starts before deadline")
            .expect("Podman run starts");
        assert!(
            output.status.success(),
            "packaged server container starts: {}",
            safe_output(&output)
        );
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), host_port);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = container_exit_code(&name).await {
                panic!("packaged server exited before readiness with status {status}");
            }
            if matches!(
                timeout(
                    Duration::from_secs(1),
                    send(address, "GET", "/api/health/ready", None, ""),
                )
                .await,
                Ok(Ok(response)) if response.status == 200
            ) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "packaged server container does not become ready before deadline"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let uid = command_output(
            "podman",
            &["exec", &name, "id", "-u"],
            Duration::from_secs(10),
        )
        .await;
        assert!(uid.status.success());
        assert_eq!(String::from_utf8_lossy(&uid.stdout).trim(), "65532");
        Self { name, address }
    }

    pub async fn stop_naturally(self) -> ExitedServer {
        let signal_started = tokio::time::Instant::now();
        let signal = command_output(
            "podman",
            &["kill", "--signal", "TERM", &self.name],
            Duration::from_secs(10),
        )
        .await;
        assert!(
            signal.status.success(),
            "SIGTERM reaches the packaged server process"
        );
        let waited = command_output("podman", &["wait", &self.name], Duration::from_secs(25)).await;
        assert!(
            waited.status.success(),
            "packaged server exits before forced cleanup"
        );
        // Stop timing at observed process exit, before inspect, log collection,
        // or container removal. The measurement conservatively includes the
        // bounded Podman signal-delivery command.
        let shutdown_elapsed = signal_started.elapsed();
        let inspect = command_output(
            "podman",
            &[
                "inspect",
                "--format",
                "{{.State.ExitCode}}|{{.State.OOMKilled}}",
                &self.name,
            ],
            Duration::from_secs(10),
        )
        .await;
        assert!(inspect.status.success());
        let result = String::from_utf8_lossy(&inspect.stdout);
        let (exit, oom) = result
            .trim()
            .split_once('|')
            .expect("inspect returns exit and OOM state");
        let logs = command_output("podman", &["logs", &self.name], Duration::from_secs(10)).await;
        assert!(logs.status.success());
        let cleanup = command_output("podman", &["rm", &self.name], Duration::from_secs(10)).await;
        assert!(
            cleanup.status.success(),
            "naturally exited harness server is removed"
        );
        ExitedServer {
            logs: safe_output(&logs),
            exit_code: exit.parse().expect("container exit code is numeric"),
            shutdown_elapsed,
            oom_killed: oom == "true",
        }
    }
}

pub async fn cleanup_server(name: &str) -> Output {
    let inspect = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{ index .Config.Labels \"io.ferl.harness\" }}",
            name,
        ],
        Duration::from_secs(10),
    )
    .await;
    if !inspect.status.success() {
        return inspect;
    }
    assert_eq!(
        String::from_utf8_lossy(&inspect.stdout).trim(),
        "part11-server"
    );
    command_output("podman", &["rm", "--force", name], Duration::from_secs(15)).await
}

pub async fn cleanup_client(name: &str) -> Output {
    let inspect = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{ index .Config.Labels \"io.ferl.harness\" }}",
            name,
        ],
        Duration::from_secs(10),
    )
    .await;
    if !inspect.status.success() {
        return inspect;
    }
    assert_eq!(
        String::from_utf8_lossy(&inspect.stdout).trim(),
        "part11-client",
        "refuse cleanup of a container not owned by the proxy-client fixture"
    );
    command_output("podman", &["rm", "--force", name], Duration::from_secs(15)).await
}

/// Return only the configured proxy-trust entries; the raw inspection output
/// also contains credentials and is deliberately never returned or printed.
pub async fn trusted_proxy_entries(name: &str) -> Vec<String> {
    let output = command_output(
        "podman",
        &["inspect", "--format", "{{json .Config.Env}}", name],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        output.status.success(),
        "Podman inspects the backend environment"
    );
    let environment: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("container environment inspection is JSON");
    environment
        .as_array()
        .expect("container environment is an array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|entry| entry.strip_prefix("FERL_TRUSTED_PROXIES="))
        .map(str::to_owned)
        .collect()
}

pub async fn command_output(program: &str, arguments: &[&str], limit: Duration) -> Output {
    let mut command = Command::new(program);
    command.args(arguments).kill_on_drop(true);
    timeout(limit, command.output())
        .await
        .unwrap_or_else(|_| panic!("{program} command exceeds its bound: {arguments:?}"))
        .unwrap_or_else(|error| panic!("{program} command cannot start: {error}"))
}

async fn container_exit_code(name: &str) -> Option<String> {
    let inspect = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{.State.Running}}|{{.State.ExitCode}}",
            name,
        ],
        Duration::from_secs(10),
    )
    .await;
    if !inspect.status.success() {
        return None;
    }
    let result = String::from_utf8_lossy(&inspect.stdout);
    let (running, exit_code) = result.trim().split_once('|')?;
    (running == "false").then(|| exit_code.to_owned())
}

fn safe_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn host_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a local published port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

pub fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().expect("packaged release path resolves")
}
