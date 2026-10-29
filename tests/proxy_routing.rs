#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
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
    net::{IpAddr, SocketAddr, TcpListener},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use account_support::{migrated_database, seed_account};
use futures_util::FutureExt;
use serving_process::{ServingProcess, assert_headers, json, send, send_with_headers};
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
use uuid::Uuid;

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[tokio::test]
async fn pinned_tcp_proxy_routes_raw_paths_trusts_only_observed_hops_and_carries_real_cursors() {
    let binary = PathBuf::from(
        std::env::var_os("FERL_RELEASE_BINARY")
            .expect("run scripts/check-release.sh; the packaged release executable is required"),
    )
    .canonicalize()
    .expect("packaged server executable exists");
    let database = migrated_database().await;
    let run_id = Uuid::new_v4().simple().to_string();
    let host_probe_id = format!("part11-hostprobe-{run_id}");
    let proxy_id = format!("part11-proxy-{run_id}");
    let client_name = format!("ferl-part11-client-{run_id}");
    let network = format!("ferl-part11-net-{run_id}");
    let host_proxy_port = reserve_port().await;
    let proxy_port = reserve_distinct_port(&[host_proxy_port]).await;
    assert_ne!(host_proxy_port, proxy_port);
    let host_runtime = std::env::current_dir()
        .unwrap()
        .join("target/proxy")
        .join(&host_probe_id);
    let proxy_runtime = std::env::current_dir()
        .unwrap()
        .join("target/proxy")
        .join(&proxy_id);
    let server_a = format!("ferl-part11-proxy-a-{run_id}");
    let server_b = format!("ferl-part11-proxy-b-{run_id}");
    let cleanup_server_a_name = server_a.clone();
    let cleanup_server_b_name = server_b.clone();
    let cleanup_client_name = client_name.clone();
    let cleanup_network = network.clone();
    let cleanup_host_id = host_probe_id.clone();
    let cleanup_proxy_id = proxy_id.clone();
    let cleanup_host_runtime = host_runtime.clone();
    let cleanup_proxy_runtime = proxy_runtime.clone();
    let result = AssertUnwindSafe(async {
        let network_for_test = podman_server::create_network(&run_id).await;
        assert_eq!(network_for_test, network);
        database
            .verify(|database| {
                Box::pin(async move {
                    // First prove the actual macOS Podman host-gateway behavior.
                    // This is a separate probe, not a trusted production proxy:
                    // gvproxy presents the host server with loopback, so the
                    // test deliberately refuses to trust that shared address.
                    let (_admin_id, key) =
                        seed_account(database.pool(), "proxy_release_owner", "user", true).await;
                    let (_id, _token) =
                        account_support::seed_anonymous_link(database.pool(), "proxy-observe").await;
                    let host_url = database.schema_database_url().to_owned();
                    let host_server_settings = [
                        ("FERL_ANONYMOUS_CREATION", "true"),
                        ("FERL_TRUSTED_PROXIES", ""),
                    ];
                    let host_server = ServingProcess::start_with_host_listener(
                        &host_url,
                        CURSOR_KEY,
                        &host_server_settings,
                    )
                    .await;
                    let host_backend_port = host_server.address.port().to_string();
                    proxy_action(
                        &host_probe_id,
                        host_proxy_port,
                        &host_backend_port,
                        "host.containers.internal",
                        None,
                        &host_runtime,
                        "start",
                    )
                    .await;
                    proxy_action(
                        &host_probe_id,
                        host_proxy_port,
                        &host_backend_port,
                        "host.containers.internal",
                        None,
                        &host_runtime,
                        "ready",
                    )
                    .await;
                    let host_proxy = SocketAddr::from(([127, 0, 0, 1], host_proxy_port));
                    let host_redirect = send_with_headers(
                        host_proxy,
                        "GET",
                        "/proxy-observe",
                        None,
                        "",
                        &[("User-Agent", "PROXY_HOST_GATEWAY_OBSERVATION")],
                    )
                    .await
                    .unwrap();
                    assert_headers(&host_redirect);
                    assert_eq!(host_redirect.status, 302);
                    wait_for_visit(
                        database.pool(),
                        "proxy-observe",
                        "PROXY_HOST_GATEWAY_OBSERVATION",
                    )
                    .await;
                    let observed_host_peer: IpAddr = sqlx::query_scalar::<_, String>(
                        "SELECT host(v.client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxy-observe' AND v.user_agent='PROXY_HOST_GATEWAY_OBSERVATION'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap()
                    .parse()
                    .unwrap();
                    assert!(!observed_host_peer.is_unspecified());
                    println!("PROXY_HOST_GATEWAY_OBSERVED backend_peer={observed_host_peer}");
                    let host_server_output = host_server.stop_and_capture().await;
                    assert!(host_server_output.exit_success);
                    proxy_action(
                        &host_probe_id,
                        host_proxy_port,
                        &host_backend_port,
                        "host.containers.internal",
                        None,
                        &host_runtime,
                        "cleanup",
                    )
                    .await;

                    // For a usable trusted-proxy boundary, run the packaged Linux
                    // executable and NGINX on one isolated Podman bridge. This
                    // makes their actual TCP peers distinct from host clients.
                    let package = binary.clone();
                    let container_database_url =
                        podman_server::container_database_url(database.schema_database_url()).await;
                    let app_port = podman_server::host_port();
                    let backend_a = podman_server::ContainerServer::start(
                        &package,
                        &server_a,
                        &network_for_test,
                        &container_database_url,
                        CURSOR_KEY,
                        "",
                        app_port,
                        &[],
                    )
                    .await;
                    proxy_action(
                        &proxy_id,
                        proxy_port,
                        "8080",
                        "ferl-backend",
                        Some(&network_for_test),
                        &proxy_runtime,
                        "start",
                    )
                    .await;
                    proxy_action(
                        &proxy_id,
                        proxy_port,
                        "8080",
                        "ferl-backend",
                        Some(&network_for_test),
                        &proxy_runtime,
                        "ready",
                    )
                    .await;
                    let proxy = SocketAddr::from(([127, 0, 0, 1], proxy_port));
                    let nginx_container = format!("ferl-part11-proxy-{proxy_id}");
                    let upstream_probe = podman_server::command_output(
                        "podman",
                        &[
                            "exec",
                            &nginx_container,
                            "wget",
                            "-qO-",
                            "http://ferl-backend:8080/api/health/ready",
                        ],
                        Duration::from_secs(10),
                    )
                    .await;
                    assert!(
                        upstream_probe.status.success(),
                        "NGINX container reaches the packaged server over the isolated network: {}",
                        String::from_utf8_lossy(&upstream_probe.stderr)
                    );
                    let direct_api = send(
                        backend_a.address,
                        "POST",
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://direct-network-probe.example/","short_code":"direct-probe"}"#,
                    )
                    .await
                    .unwrap();
                    assert_eq!(direct_api.status, 201, "packaged API works directly over its published port");

                    let root = send(proxy, "GET", "/", None, "").await.unwrap();
                    assert_eq!(root.status, 200);
                    assert!(String::from_utf8_lossy(&root.body).contains("Separate static fixture"));
                    let asset = send(proxy, "GET", "/assets/example.js", None, "")
                        .await
                        .unwrap();
                    assert_eq!(asset.status, 200);
                    assert!(String::from_utf8_lossy(&asset.body).contains("static asset served outside"));

                    let observed = send_with_headers(
                        proxy,
                        "GET",
                        "/proxy-observe",
                        None,
                        "",
                        &[("User-Agent", "PROXY_CONTAINER_PEER_OBSERVATION")],
                    )
                    .await
                    .unwrap();
                    assert_headers(&observed);
                    assert_eq!(observed.status, 302);
                    wait_for_visit(
                        database.pool(),
                        "proxy-observe",
                        "PROXY_CONTAINER_PEER_OBSERVATION",
                    )
                    .await;
                    let proxy_peer: IpAddr = sqlx::query_scalar::<_, String>(
                        "SELECT host(v.client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxy-observe' AND v.user_agent='PROXY_CONTAINER_PEER_OBSERVATION'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap()
                    .parse()
                    .unwrap();
                    assert_ne!(proxy_peer, IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
                    let nginx_ip = container_ip(&nginx_container).await;
                    assert_eq!(proxy_peer, nginx_ip, "DB proves NGINX's actual container peer");
                    let nginx_identity = container_id(&nginx_container).await;

                    // Observe the direct peer before granting proxy trust. It
                    // must differ from the NGINX bridge peer; otherwise this
                    // topology cannot safely be used for forwarded identity.
                    let direct = send_with_headers(
                        backend_a.address,
                        "GET",
                        "/proxy-observe",
                        None,
                        "",
                        &[
                            ("User-Agent", "PROXY_DIRECT_PEER_OBSERVATION"),
                            ("X-Forwarded-For", "198.51.100.230"),
                        ],
                    )
                    .await
                    .unwrap();
                    assert_eq!(direct.status, 302);
                    wait_for_visit(
                        database.pool(),
                        "proxy-observe",
                        "PROXY_DIRECT_PEER_OBSERVATION",
                    )
                    .await;
                    let direct_peer: IpAddr = sqlx::query_scalar::<_, String>(
                        "SELECT host(v.client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxy-observe' AND v.user_agent='PROXY_DIRECT_PEER_OBSERVATION'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap()
                    .parse()
                    .unwrap();
                    assert_ne!(direct_peer, proxy_peer, "direct caller is outside trusted proxy set");
                    println!(
                        "PROXY_PEERS podman_host_gateway_to_host_backend={observed_host_peer} \
                         packaged_backend_peer={proxy_peer} nginx_container={nginx_ip} \
                         direct_untrusted_peer={direct_peer}"
                    );
                    let server_a_exit = backend_a.stop_naturally().await;
                    assert_eq!(server_a_exit.exit_code, 0);
                    assert!(!server_a_exit.oom_killed);

                    let trusted = proxy_peer.to_string();
                    let backend = podman_server::ContainerServer::start(
                        &package,
                        &server_b,
                        &network_for_test,
                        &container_database_url,
                        CURSOR_KEY,
                        &trusted,
                        app_port,
                        &[],
                    )
                    .await;
                    let backend_id = container_id(&server_b).await;
                    let backend_ip = container_ip(&server_b).await;
                    let backend_ip_text = backend_ip.to_string();
                    let configured_trusted_proxies =
                        podman_server::trusted_proxy_entries(&server_b).await;
                    assert_eq!(
                        configured_trusted_proxies.as_slice(),
                        std::slice::from_ref(&trusted),
                        "the backend trusts exactly the NGINX peer observed on this network"
                    );
                    let backend_health = podman_server::command_output(
                        "podman",
                        &[
                            "exec",
                            &nginx_container,
                            "wget",
                            "-qO-",
                            &format!("http://{backend_ip_text}:8080/api/health/ready"),
                        ],
                        Duration::from_secs(10),
                    )
                    .await;
                    assert!(backend_health.status.success(),
                        "the retained NGINX container reaches replacement backend IP {backend_ip}: {}",
                        String::from_utf8_lossy(&backend_health.stderr));
                    proxy_action(
                        &proxy_id,
                        proxy_port,
                        "8080",
                        &backend_ip_text,
                        Some(&network_for_test),
                        &proxy_runtime,
                        "reload",
                    )
                    .await;
                    let effective_config = podman_server::command_output(
                        "podman",
                        &["exec", &nginx_container, "nginx", "-T"],
                        Duration::from_secs(10),
                    ).await;
                    assert!(effective_config.status.success());
                    let active_config = String::from_utf8_lossy(&effective_config.stdout);
                    assert!(active_config.contains(&backend_ip_text),
                        "running NGINX config uses the replacement backend's observed address");
                    assert!(active_config.contains("proxy_set_header X-Forwarded-For $remote_addr;"),
                        "the active edge config replaces supplied XFF with its observed TCP client");
                    assert_eq!(container_id(&nginx_container).await, nginx_identity,
                        "the actual proxy instance observed before trust remains alive for assertions");
                    assert_eq!(
                        container_ip(&nginx_container).await,
                        nginx_ip,
                        "the forwarding proxy's peer address cannot pass by replacement-address reuse"
                    );
                    let proxy_ready = wait_for_reloaded_backend(proxy).await;
                    assert_headers(&proxy_ready);
                    let nginx_health = podman_server::command_output(
                        "podman",
                        &["exec", &nginx_container, "wget", "-qO-", "http://127.0.0.1:8080/api/health/ready"],
                        Duration::from_secs(10),
                    ).await;
                    assert!(nginx_health.status.success(),
                        "reloaded NGINX forwards health to its replacement backend");

                    for (code, destination) in [
                        ("assets", "https://proxy-assets.example/path"),
                        ("health", "https://proxy-health.example/path"),
                        ("api-sale", "https://proxy-api-sale.example/path"),
                        ("proxyraw", "https://proxy-raw.example/path?edition=2#section"),
                        ("proxyquote", "https://proxy-quote.example/path"),
                        ("proxyutf8", "https://proxy-utf8.example/path"),
                    ] {
                        let created = send(
                            proxy,
                            "POST",
                            "/api/v1/links",
                            Some(&key),
                            &format!(
                                r#"{{"destination_url":"{destination}","short_code":"{code}","title":"Proxy fixture","tags":["Proxy Test"]}}"#
                            ),
                        )
                        .await
                        .unwrap();
                        assert_eq!(
                            created.status,
                            201,
                            "proxy permits API create {code}; headers={:?}; body={}; app logs={}; proxy logs={}",
                            created.headers,
                            String::from_utf8_lossy(&created.body),
                            String::from_utf8_lossy(&podman_server::command_output(
                                "podman", &["logs", &server_b], Duration::from_secs(10)
                            ).await.stdout),
                            String::from_utf8_lossy(&podman_server::command_output(
                                "podman", &["logs", &nginx_container], Duration::from_secs(10)
                            ).await.stdout)
                        );
                        assert_headers(&created);
                    }
                    let reserved = send(
                        proxy,
                        "POST",
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://reserved.example/","short_code":"api"}"#,
                    )
                    .await
                    .unwrap();
                    assert_eq!(reserved.status, 409);

                    for code in ["assets", "health", "api-sale"] {
                        let response = send(proxy, "GET", &format!("/{code}"), None, "")
                            .await
                            .unwrap();
                        assert_headers(&response);
                        assert_eq!(response.status, 302, "/{code} belongs to short-link routing");
                        assert!(response.body.is_empty(), "redirect responses have zero body bytes");
                    }
                    for method in ["GET", "HEAD"] {
                        let response = send(proxy, method, "/proxyraw?incoming=ignored", None, "")
                            .await
                            .unwrap();
                        assert_headers(&response);
                        assert_eq!(response.status, 302);
                        assert_eq!(response.headers["location"], "https://proxy-raw.example/path?edition=2#section");
                        assert!(response.body.is_empty(), "NGINX-forwarded {method} redirect is bodyless");
                    }
                    let static_missing = send(proxy, "GET", "/assets/missing.js", None, "")
                        .await
                        .unwrap();
                    assert_eq!(static_missing.status, 404);
                    assert!(!String::from_utf8_lossy(&static_missing.body)
                        .contains("Separate static fixture"));
                    assert_eq!(
                        send(proxy, "GET", "/assets", None, "").await.unwrap().status,
                        302,
                        "bare /assets is a short code and is not rewritten to /assets/"
                    );

                    for path in ["/api", "/api/", "/api/unknown", "/api/v1/no-such-operation"] {
                        let response = send(proxy, "GET", path, None, "").await.unwrap();
                        assert_headers(&response);
                        assert_eq!(response.status, 404, "{path} stays in the API namespace");
                        assert!(!response.headers.contains_key("location"));
                        assert_eq!(json(&response)["error"]["code"], "not_found");
                    }

                    for path in [
                        "/proxyraw/",
                        "/proxyraw/extra",
                        "/%70roxyraw",
                        "/proxyraw%2fextra",
                        "/definitely-missing",
                        "/definitely-missing/extra",
                    ] {
                        let response = send(proxy, "GET", path, None, "").await.unwrap();
                        assert_headers(&response);
                        assert_eq!(response.status, 404, "raw path {path} is never normalized to a redirect");
                        assert!(!response.headers.contains_key("location"));
                        assert!(!String::from_utf8_lossy(&response.body)
                            .contains("Separate static fixture"));
                    }
                    let unsupported = send(proxy, "POST", "/proxyraw", None, "")
                        .await
                        .unwrap();
                    assert_headers(&unsupported);
                    assert_eq!(unsupported.status, 405);
                    assert_eq!(unsupported.headers["allow"], "GET, HEAD");
                    assert_eq!(json(&unsupported)["error"]["code"], "method_not_allowed");
                    assert_eq!(unsupported.headers["content-type"], "application/json");

                    let too_large_body = "x".repeat(65_537);
                    let too_large = send(
                        proxy,
                        "POST",
                        "/api/v1/links",
                        Some(&key),
                        &too_large_body,
                    )
                    .await
                    .unwrap();
                    assert_headers(&too_large);
                    assert_eq!(too_large.status, 413);
                    assert_eq!(json(&too_large)["error"]["code"], "request_too_large");

                    // This client is a distinct, run-owned container on the
                    // same isolated network. It reaches both NGINX and the
                    // backend by real TCP, so the proxy's observed client and
                    // the backend's NGINX peer cannot collapse to one address.
                    let client =
                        podman_server::NetworkClient::start(&client_name, &network_for_test).await;
                    let client_id = container_id(&client.name).await;
                    let client_ip = container_ip(&client.name).await;
                    assert_eq!(client.id, client_id, "client container identity is retained");
                    assert_eq!(client.ip, client_ip, "client address is read from its network attachment");
                    let backend_id_after_reload = container_id(&server_b).await;
                    let backend_ip_after_reload = container_ip(&server_b).await;
                    assert_eq!(backend_id_after_reload, backend_id);
                    assert_eq!(backend_ip_after_reload, backend_ip);
                    assert_ne!(client_id, nginx_identity);
                    assert_ne!(client_id, backend_id);
                    assert_ne!(nginx_identity, backend_id);

                    let spoofed_ip: IpAddr = "198.51.100.240".parse().unwrap();
                    assert_ne!(client_ip, nginx_ip, "actual proxy client differs from NGINX peer");
                    assert_ne!(client_ip, spoofed_ip, "attacker-supplied IP is a third identity");
                    assert_ne!(nginx_ip, spoofed_ip);
                    println!(
                        "PROXY_NETWORK_IDENTITIES client_id={client_id} client_ip={client_ip} \
                         nginx_id={nginx_identity} nginx_ip={nginx_ip} \
                         backend_id={backend_id} backend_ip={backend_ip} \
                         attacker_spoofed_ip={spoofed_ip}"
                    );

                    let forwarded_marker = format!("PROXY_EFFECTIVE_IP_{run_id}");
                    let forwarded = client
                        .get(nginx_ip, "/proxyraw", &forwarded_marker, spoofed_ip)
                        .await;
                    assert_network_redirect(
                        &forwarded,
                        nginx_ip,
                        "https://proxy-raw.example/path?edition=2#section",
                    );
                    let nginx_client = proxy_client(&proxy_id).await;
                    assert_eq!(
                        nginx_client, client_ip,
                        "NGINX access log records the separate client container's TCP peer"
                    );
                    wait_for_visit(database.pool(), "proxyraw", &forwarded_marker).await;
                    let stored_forwarded: String = sqlx::query_scalar(
                        "SELECT host(client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxyraw' AND user_agent=$1",
                    )
                    .bind(&forwarded_marker)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                    assert_eq!(stored_forwarded, client_ip.to_string());
                    assert_ne!(stored_forwarded, proxy_peer.to_string());
                    assert_ne!(stored_forwarded, spoofed_ip.to_string());

                    let direct_marker = format!("UNTRUSTED_DIRECT_IP_{run_id}");
                    let direct = client
                        .get(backend_ip, "/proxyraw", &direct_marker, spoofed_ip)
                        .await;
                    assert_network_redirect(
                        &direct,
                        backend_ip,
                        "https://proxy-raw.example/path?edition=2#section",
                    );
                    wait_for_visit(database.pool(), "proxyraw", &direct_marker).await;
                    let stored_direct: String = sqlx::query_scalar(
                        "SELECT host(client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxyraw' AND user_agent=$1",
                    )
                    .bind(&direct_marker)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                    assert_eq!(stored_direct, client_ip.to_string());
                    assert_ne!(stored_direct, proxy_peer.to_string());
                    assert_ne!(stored_direct, spoofed_ip.to_string());

                    // Confirm the retained NGINX network peer still reaches the
                    // replacement backend as that same peer. An absent XFF
                    // exercises the production resolver's trusted-peer fallback.
                    let nginx_peer_marker = format!("NGINX_PEER_AFTER_RELOAD_{run_id}");
                    let nginx_peer_url = format!("http://{backend_ip}:8080/proxyraw");
                    let nginx_peer_agent = format!("User-Agent: {nginx_peer_marker}");
                    let nginx_peer_probe = podman_server::command_output(
                        "podman",
                        &[
                            "exec",
                            &nginx_container,
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
                            &nginx_peer_agent,
                            "--header",
                            "X-Forwarded-For:",
                            &nginx_peer_url,
                        ],
                        Duration::from_secs(12),
                    )
                    .await;
                    assert_network_redirect(
                        &nginx_peer_probe,
                        backend_ip,
                        "https://proxy-raw.example/path?edition=2#section",
                    );
                    wait_for_visit(database.pool(), "proxyraw", &nginx_peer_marker).await;
                    let persisted_replacement_peer: String = sqlx::query_scalar(
                        "SELECT host(client_ip) FROM visits v JOIN links l ON l.id=v.link_id \
                         WHERE l.short_code='proxyraw' AND user_agent=$1",
                    )
                    .bind(&nginx_peer_marker)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                    assert_eq!(persisted_replacement_peer, nginx_ip.to_string());
                    assert_eq!(container_id(&nginx_container).await, nginx_identity);
                    assert_eq!(container_ip(&nginx_container).await, nginx_ip);
                    assert_eq!(container_id(&server_b).await, backend_id);

                    println!(
                        "PROXY_FORWARDED_ASSERTION actual_client={client_ip} nginx_peer={proxy_peer} \
                         attacker_spoof={spoofed_ip} persisted_proxied={stored_forwarded} \
                         persisted_direct={stored_direct} persisted_replacement_peer={persisted_replacement_peer} \
                         observed_by_nginx={nginx_client}"
                    );

                    let quote_value = format!("M{}", "\"".repeat(8_191));
                    let utf8_value = format!("M{}€", "é".repeat(4_094));
                    assert_eq!(quote_value.len(), 8_192);
                    assert_eq!(utf8_value.len(), 8_192);
                    for (code, maximum) in [
                        ("proxyquote", quote_value.as_str()),
                        ("proxyutf8", utf8_value.as_str()),
                    ] {
                        for user_agent in ["A", maximum, "N", "Z"] {
                            let redirect = send_with_headers(
                                proxy,
                                "GET",
                                &format!("/{code}"),
                                None,
                                "",
                                &[("User-Agent", user_agent)],
                            )
                            .await
                            .unwrap();
                            assert_eq!(redirect.status, 302);
                            assert!(redirect.body.is_empty(), "redirect through NGINX has no body bytes");
                        }
                    }
                    wait_for_user_agent(database.pool(), "proxyquote", &quote_value).await;
                    wait_for_user_agent(database.pool(), "proxyutf8", &utf8_value).await;
                    for (code, maximum, minimum_cursor) in [
                        ("proxyquote", quote_value.as_str(), 16_000),
                        ("proxyutf8", utf8_value.as_str(), 8_192),
                    ] {
                        let first_page = send(
                            proxy,
                            "GET",
                            &format!("/api/v1/links/{code}/stats/breakdown?group_by=user_agent&limit=2"),
                            Some(&key),
                            "",
                        )
                        .await
                        .unwrap();
                        assert_headers(&first_page);
                        assert_eq!(first_page.status, 200);
                        let page = json(&first_page);
                        assert_eq!(page["items"].as_array().unwrap().len(), 2);
                        assert_eq!(page["items"][0]["value"], "A");
                        assert_eq!(page["items"][1]["value"], maximum);
                        let cursor = page["next_cursor"].as_str().unwrap();
                        assert!(cursor.len() > minimum_cursor);
                        let next_page = send(
                            proxy,
                            "GET",
                            &format!("/api/v1/links/{code}/stats/breakdown?group_by=user_agent&limit=1&cursor={cursor}"),
                            Some(&key),
                            "",
                        )
                        .await
                        .unwrap();
                        assert_headers(&next_page);
                        assert_eq!(next_page.status, 200, "actual issued cursor traverses NGINX");
                        let next_body = json(&next_page);
                        assert_eq!(next_body["items"], serde_json::json!([{"value":"N","visits":1}]));
                        let next_cursor = next_body["next_cursor"].as_str().unwrap();
                        let final_page = send(
                            proxy,
                            "GET",
                            &format!("/api/v1/links/{code}/stats/breakdown?group_by=user_agent&limit=1&cursor={next_cursor}"),
                            Some(&key),
                            "",
                        ).await.unwrap();
                        assert_headers(&final_page);
                        assert_eq!(final_page.status, 200);
                        let final_body = json(&final_page);
                        assert_eq!(final_body["items"], serde_json::json!([{"value":"Z","visits":1}]));
                        assert!(final_body["next_cursor"].is_null(), "server-issued pages terminate explicitly after every bucket");
                    }

                    let backend_root = send(backend.address, "GET", "/", None, "")
                        .await
                        .unwrap();
                    assert_eq!(backend_root.status, 404);
                    let backend_asset = send(backend.address, "GET", "/assets/example.js", None, "")
                        .await
                        .unwrap();
                    assert_eq!(backend_asset.status, 404);

                    let exit = backend.stop_naturally().await;
                    assert_eq!(exit.exit_code, 0);
                    assert!(!exit.oom_killed);
                    assert!(exit.shutdown_elapsed < Duration::from_secs(20));
                    for secret in [
                        key.as_str(),
                        forwarded_marker.as_str(),
                        direct_marker.as_str(),
                        nginx_peer_marker.as_str(),
                    ] {
                        assert!(!exit.logs.contains(secret));
                    }
                    let proxy_config =
                        fs::read_to_string(proxy_runtime.join("conf.d/default.conf")).unwrap();
                    let forwarding_config =
                        fs::read_to_string(proxy_runtime.join("ferl-proxy-headers.conf")).unwrap();
                    assert!(proxy_config.contains(&backend_ip_text));
                    assert!(forwarding_config
                        .contains("proxy_set_header X-Forwarded-For $remote_addr;"));
                    assert!(proxy_config.contains("large_client_header_buffers 4 128k"));
                })
            })
            .await;
    })
    .catch_unwind()
    .await;

    let cleanup_server_a = podman_server::cleanup_server(&cleanup_server_a_name).await;
    let cleanup_server_b = podman_server::cleanup_server(&cleanup_server_b_name).await;
    let cleanup_client = podman_server::cleanup_client(&cleanup_client_name).await;
    let cleanup_proxy = proxy_action(
        &cleanup_proxy_id,
        proxy_port,
        "8080",
        "ferl-backend",
        Some(&network),
        &cleanup_proxy_runtime,
        "cleanup",
    )
    .await;
    let cleanup_host_proxy = proxy_action(
        &cleanup_host_id,
        host_proxy_port,
        "8080",
        "host.containers.internal",
        None,
        &cleanup_host_runtime,
        "cleanup",
    )
    .await;
    let cleanup_network = podman_server::remove_network(&cleanup_network).await;
    assert!(cleanup_server_a.status.success() || !container_exists(&cleanup_server_a_name).await);
    assert!(cleanup_server_b.status.success() || !container_exists(&cleanup_server_b_name).await);
    assert!(cleanup_client.status.success() || !container_exists(&cleanup_client_name).await);
    assert!(cleanup_proxy.status.success());
    assert!(cleanup_host_proxy.status.success());
    assert!(cleanup_network.status.success());
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn proxy_action(
    run_id: &str,
    port: u16,
    backend_port: &str,
    backend_host: &str,
    network: Option<&str>,
    runtime_dir: &Path,
    action: &str,
) -> Output {
    let mut command = Command::new("sh");
    command
        .args(["scripts/proxy.sh", action])
        .env("FERL_PROXY_RUN_ID", run_id)
        .env("FERL_PROXY_NAME", format!("ferl-part11-proxy-{run_id}"))
        .env("FERL_PROXY_PORT", port.to_string())
        .env("FERL_PROXY_BACKEND_PORT", backend_port)
        .env("FERL_PROXY_BACKEND_HOST", backend_host)
        .env("FERL_PROXY_RUNTIME_DIR", runtime_dir)
        .kill_on_drop(true);
    if let Some(network) = network {
        command.env("FERL_PROXY_NETWORK", network);
    }
    timeout(Duration::from_secs(30), command.output())
        .await
        .unwrap_or_else(|_| panic!("proxy harness {action} exceeds its 30-second bound"))
        .expect("proxy harness command starts")
}

fn assert_network_redirect(output: &Output, peer: IpAddr, location: &str) {
    assert!(
        output.status.success(),
        "isolated-network curl request completes: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = String::from_utf8_lossy(&output.stdout);
    assert!(
        response.contains("HTTP/1.1 302"),
        "real TCP response is a 302: {response}"
    );
    assert!(
        response.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with(&format!("location: {location}").to_ascii_lowercase())
        }),
        "real TCP response carries the exact stored Location"
    );
    assert!(
        response.contains(&format!("FERL_CLIENT_RESULT 302 {peer}")),
        "curl reports the expected real network peer and HTTP status"
    );
}

async fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn reserve_distinct_port(excluded: &[u16]) -> u16 {
    loop {
        let candidate = reserve_port().await;
        if !excluded.contains(&candidate) {
            return candidate;
        }
    }
}

async fn wait_for_reloaded_backend(proxy: SocketAddr) -> serving_process::HttpResult {
    timeout(Duration::from_secs(15), async {
        loop {
            let response = send(proxy, "GET", "/api/health/ready", None, "")
                .await
                .expect("reloaded proxy readiness request reaches TCP");
            if response.status == 200 {
                return response;
            }
            assert!(
                matches!(response.status, 502 | 504),
                "proxy backend reconfiguration returned an unexpected status {}",
                response.status
            );
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the retained NGINX instance forwards to its replacement backend before deadline")
}

async fn container_ip(name: &str) -> IpAddr {
    let output = podman_server::command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            name,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        output.status.success(),
        "Podman inspects test-owned container {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("NGINX receives an address on the isolated user-defined network")
}

async fn container_id(name: &str) -> String {
    let output = podman_server::command_output(
        "podman",
        &["inspect", "--format", "{{.Id}}", name],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        output.status.success(),
        "Podman inspects owned container {name}"
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn proxy_client(run_id: &str) -> IpAddr {
    let output = podman_server::command_output(
        "podman",
        &["logs", &format!("ferl-part11-proxy-{run_id}")],
        Duration::from_secs(10),
    )
    .await;
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .rev()
        .find_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() == 3 && fields[1] == "GET" && fields[2] == "302")
                .then(|| fields[0].parse().ok())
                .flatten()
        })
        .expect("NGINX safe access log records peer/method/status without request targets")
}

async fn wait_for_visit(pool: &sqlx::PgPool, code: &str, user_agent: &str) {
    timeout(Duration::from_secs(12), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id \
                 WHERE l.short_code=$1 AND v.user_agent=$2",
            )
            .bind(code)
            .bind(user_agent)
            .fetch_one(pool)
            .await
            .unwrap();
            if count > 0 {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("proxy-generated visit persists before its deadline");
}

async fn wait_for_user_agent(pool: &sqlx::PgPool, code: &str, value: &str) {
    timeout(Duration::from_secs(12), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id \
                 WHERE l.short_code=$1 AND v.user_agent=$2",
            )
            .bind(code)
            .bind(value)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            assert!(count == 0, "the maximum grouping value is recorded once");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maximum legal analytics header persists over the proxy");
}

async fn container_exists(name: &str) -> bool {
    podman_server::command_output(
        "podman",
        &["container", "exists", name],
        Duration::from_secs(10),
    )
    .await
    .status
    .success()
}
