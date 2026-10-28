use ferl_server::{
    app,
    client_ip::{CanonicalIp, ClientIpResolver},
};
use http::{HeaderMap, HeaderValue};
use std::{net::IpAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};

fn resolver(trust: &[&str]) -> ClientIpResolver {
    let config = ferl_server::config::Config::from_map(std::collections::BTreeMap::from([
        (
            "FERL_DATABASE_URL".into(),
            "postgres://ferl@127.0.0.1:1/ferl_test".into(),
        ),
        (
            "FERL_PUBLIC_BASE_URL".into(),
            "https://short.example.test".into(),
        ),
        (
            "FERL_CURSOR_KEY".into(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        ),
        ("FERL_TRUSTED_PROXIES".into(), trust.join(",")),
    ]))
    .unwrap();
    ClientIpResolver::new(&config.trusted_proxies)
}
fn resolve(trust: &[&str], peer: &str, chain: Option<&str>) -> IpAddr {
    let mut headers = HeaderMap::new();
    if let Some(chain) = chain {
        headers.insert("x-forwarded-for", HeaderValue::from_str(chain).unwrap());
    }
    resolver(trust)
        .resolve(peer.parse().unwrap(), &headers)
        .address()
}

#[test]
fn trust_walk_validates_the_complete_chain_and_ignores_other_headers() {
    for (trust, peer, chain, expected) in [
        (vec![], "192.0.2.1", "203.0.113.8", "192.0.2.1"),
        (vec!["10.0.0.0/8"], "192.0.2.1", "203.0.113.8", "192.0.2.1"),
        (
            vec!["10.0.0.0/8"],
            "10.0.0.1",
            "203.0.113.8, 192.0.2.1, 10.1.2.3",
            "192.0.2.1",
        ),
        (
            vec!["10.0.0.0/8"],
            "10.0.0.1",
            "10.9.8.7, 10.1.2.3",
            "10.9.8.7",
        ),
        (
            vec!["10.0.0.0/8"],
            "10.0.0.1",
            "\t2001:db8::2\t, 10.1.2.3 ",
            "2001:db8::2",
        ),
        (
            vec!["10.0.0.0/8"],
            "::ffff:10.0.0.1",
            "::ffff:203.0.113.8, ::ffff:10.1.2.3",
            "203.0.113.8",
        ),
        (
            vec!["10.0.0.0/8"],
            "10.0.0.1",
            "bad-prefix, 192.0.2.1",
            "10.0.0.1",
        ),
    ] {
        assert_eq!(
            resolve(&trust, peer, Some(chain)),
            expected.parse::<IpAddr>().unwrap()
        );
    }
    let headers = HeaderMap::from_iter([
        (
            "forwarded".parse::<http::HeaderName>().unwrap(),
            "for=203.0.113.8".parse().unwrap(),
        ),
        ("x-real-ip".parse().unwrap(), "203.0.113.8".parse().unwrap()),
    ]);
    assert_eq!(
        resolver(&["10.0.0.0/8"])
            .resolve("10.0.0.1".parse().unwrap(), &headers)
            .address(),
        "10.0.0.1".parse::<IpAddr>().unwrap()
    );
}

#[test]
fn malformed_duplicate_missing_and_oversized_chains_fall_back_atomically() {
    let peer: IpAddr = "10.0.0.1".parse().unwrap();
    for chain in [
        None,
        Some(""),
        Some(" "),
        Some(",192.0.2.1"),
        Some("192.0.2.1,"),
        Some("192.0.2.1,,10.1.1.1"),
        Some("192.0.2.1:80"),
        Some("[2001:db8::1]"),
        Some("[2001:db8::1]:80"),
        Some("fe80::1%en0"),
        Some("unknown"),
        Some("for=192.0.2.1"),
        Some("192.0.2.1/32"),
        Some("999.1.1.1"),
    ] {
        assert_eq!(resolve(&["10.0.0.0/8"], "10.0.0.1", chain), peer);
    }
    let mut headers = HeaderMap::new();
    headers.append("x-forwarded-for", "192.0.2.1".parse().unwrap());
    headers.append("x-forwarded-for", "203.0.113.1".parse().unwrap());
    assert_eq!(
        resolver(&["10.0.0.0/8"]).resolve(peer, &headers).address(),
        peer
    );
    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_bytes(b"192.0.2.1,\xff").unwrap(),
    );
    assert_eq!(
        resolver(&["10.0.0.0/8"]).resolve(peer, &headers).address(),
        peer
    );
    let chain = std::iter::repeat_n("192.0.2.1", 32)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        resolve(&["10.0.0.0/8"], "10.0.0.1", Some(&chain)),
        "192.0.2.1".parse::<IpAddr>().unwrap()
    );
    assert_eq!(
        resolve(
            &["10.0.0.0/8"],
            "10.0.0.1",
            Some(&format!("{chain},192.0.2.1"))
        ),
        peer
    );
}

#[test]
fn mapped_configuration_preserves_exact_prefix_membership() {
    for trust in [
        "::ffff:10.0.0.1",
        "::ffff:10.0.0.1/128",
        "::ffff:10.0.0.0/120",
        "::ffff:0:0/96",
        "::/80",
    ] {
        assert_eq!(
            resolve(&[trust], "10.0.0.1", Some("203.0.113.8")),
            "203.0.113.8".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            resolve(&[trust], "::ffff:10.0.0.1", Some("203.0.113.8")),
            "203.0.113.8".parse::<IpAddr>().unwrap()
        );
    }
    for peer in ["10.0.1.1", "::ffff:10.0.1.1"] {
        assert_eq!(
            resolve(&["::ffff:10.0.0.0/120"], peer, Some("203.0.113.8")),
            "10.0.1.1".parse::<IpAddr>().unwrap()
        );
    }
    let ip = CanonicalIp::new("::ffff:192.0.2.1".parse().unwrap());
    assert_eq!(ip, CanonicalIp::new("192.0.2.1".parse().unwrap()));
    assert_eq!(ip.host_network().prefix_len(), 32);
    assert!(!format!("{ip:?}").contains("192.0.2.1"));
}

#[tokio::test]
async fn real_tcp_peer_reaches_the_production_resolver_and_missing_context_fails_closed() {
    let config = ferl_server::config::Config::from_map(std::collections::BTreeMap::from([
        (
            "FERL_DATABASE_URL".into(),
            "postgres://ferl@127.0.0.1:1/ferl_test".into(),
        ),
        (
            "FERL_PUBLIC_BASE_URL".into(),
            "https://short.example.test".into(),
        ),
        (
            "FERL_CURSOR_KEY".into(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        ),
    ]))
    .unwrap();
    let state = app::AppState::from_config(&config).unwrap();
    let mut forged = http::Request::builder()
        .header("x-forwarded-for", "203.0.113.8")
        .header("host", "192.0.2.1")
        .body(())
        .unwrap()
        .into_parts()
        .0;
    forged
        .extensions
        .insert(CanonicalIp::new("203.0.113.8".parse().unwrap()));
    assert!(state.client_ip.resolve_request(&forged).is_err());
    let (tx, mut rx) = mpsc::channel(1);
    let resolver = state.client_ip.clone();
    let router = axum::Router::new().route(
        "/peer",
        axum::routing::get(move |request: axum::extract::Request| {
            let tx = tx.clone();
            let resolver = resolver.clone();
            async move {
                tx.send(resolver.resolve_request(&request.into_parts().0).unwrap())
                    .await
                    .unwrap();
                http::StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let cancel = cancellation.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app::transport_service(router))
            .with_graceful_shutdown(cancel.cancelled_owned())
            .await
            .unwrap();
    });
    let result = timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let real_peer = stream.local_addr().unwrap().ip();
        stream.write_all(b"GET /peer HTTP/1.1\r\nHost: forged.invalid\r\nX-Forwarded-For: 203.0.113.8\r\nConnection: close\r\n\r\n").await.unwrap();
        assert_eq!(rx.recv().await.unwrap().address(), real_peer);
        let mut bytes = Vec::new(); stream.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.starts_with(b"HTTP/1.1 204"));
    }).await;
    cancellation.cancel();
    if timeout(Duration::from_secs(3), &mut server).await.is_err() {
        server.abort();
        let _ = server.await;
    }
    timeout(Duration::from_secs(3), state.pool.close())
        .await
        .unwrap();
    result.unwrap();
}
