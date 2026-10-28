#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use account_support::{call, json, migrated_database, request, seed_account};
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use ferl_server::app;
use http::{HeaderValue, header};
use http_body_util::BodyExt;
use serde_json::Value;

fn redirect_request(
    method: Method,
    code: &str,
    peer: SocketAddr,
    referer: Option<&str>,
    user_agent: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/{code}"))
        .body(Body::empty())
        .expect("public test request is valid");
    request.extensions_mut().insert(ConnectInfo(peer));
    if let Some(referer) = referer {
        request.headers_mut().insert(
            header::REFERER,
            HeaderValue::from_str(referer).expect("test referrer is a valid HTTP field value"),
        );
    }
    if let Some(user_agent) = user_agent {
        request.headers_mut().insert(
            header::USER_AGENT,
            HeaderValue::from_str(user_agent).expect("test user agent is a valid HTTP field value"),
        );
    }
    request
}

#[tokio::test]
async fn actual_redirect_snapshots_and_visit_pages_are_reported_after_delayed_worker_persistence() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) = seed_account(database.pool(), "report_records_owner", "user", true).await;
                let state = redirect_support::state(database.schema_database_url(), &[]);
                state.verify_schema().await.expect("production state verifies v2 schema");
                let router = app::router(state.clone());

                let created = call(
                    router.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://history.example.test/A?edition=old#part","short_code":"report-records"}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                assert_eq!(json(created).await["link"]["short_code"], "report-records");

                let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 41)), 43001);
                let first = redirect_support::call(
                    router.clone(),
                    redirect_request(
                        Method::GET,
                        "report-records",
                        ipv4,
                        Some("https://referrer.example.test/page?q=secret"),
                        Some("Example Browser/1.0"),
                    ),
                )
                .await;
                assert_eq!(first.status(), StatusCode::FOUND);
                assert_eq!(first.headers()[header::LOCATION], "https://history.example.test/A?edition=old#part");
                assert!(first.into_body().collect().await.unwrap().to_bytes().is_empty());

                let patched = call(
                    router.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/report-records",
                        Some(&key),
                        r#"{"destination_url":"https://history.example.test/B?edition=new#part"}"#,
                    ),
                )
                .await;
                assert_eq!(patched.status(), StatusCode::OK);
                let _ = json(patched).await;

                let mapped = SocketAddr::new(
                    IpAddr::V6("::ffff:192.0.2.42".parse::<Ipv6Addr>().unwrap()),
                    43002,
                );
                let second = redirect_support::call(
                    router.clone(),
                    redirect_request(
                        Method::HEAD,
                        "report-records",
                        mapped,
                        Some(""),
                        Some("Example Browser/2.0"),
                    ),
                )
                .await;
                assert_eq!(second.status(), StatusCode::FOUND);
                assert_eq!(second.headers()[header::LOCATION], "https://history.example.test/B?edition=new#part");
                assert!(second.into_body().collect().await.unwrap().to_bytes().is_empty());

                // A bot/repeat request for the same resolved destination is an
                // ordinary GET visit and becomes reportable after the worker drain.
                let bot_repeat = redirect_support::call(
                    router.clone(),
                    redirect_request(
                        Method::GET,
                        "report-records",
                        ipv4,
                        Some("https://bot-preview.example.test/"),
                        Some("Googlebot/2.1"),
                    ),
                )
                .await;
                assert_eq!(bot_repeat.status(), StatusCode::FOUND);
                assert_eq!(bot_repeat.headers()[header::LOCATION], "https://history.example.test/B?edition=new#part");
                assert!(bot_repeat.into_body().collect().await.unwrap().to_bytes().is_empty());

                // The worker is deliberately started only after A was edited to B
                // and the subsequent redirect captured B.
                let workers = state.start_analytics_workers().expect("workers start once");
                assert_eq!(workers.shutdown(Duration::from_secs(8)).await, ferl_server::analytics::WorkerStopOutcome::Drained);

                let page = call(
                    router.clone(),
                    request(Method::GET, "/api/v1/links/report-records/visits?limit=10", Some(&key), ""),
                )
                .await;
                assert_eq!(page.status(), StatusCode::OK);
                let page = json(page).await;
                assert_eq!(page["next_cursor"], Value::Null);
                let records = page["items"].as_array().unwrap();
                assert_eq!(records.len(), 3);
                let old = records
                    .iter()
                    .find(|row| row["redirect_url"] == "https://history.example.test/A?edition=old#part")
                    .unwrap();
                let new = records.iter().find(|row| row["method"] == "HEAD").unwrap();
                let bot = records
                    .iter()
                    .find(|row| row["user_agent"] == "Googlebot/2.1")
                    .unwrap();
                for visit in records {
                    for field in [
                        "id", "link_id", "short_code", "redirect_url", "occurred_at",
                        "method", "client_ip", "referrer", "user_agent", "device", "browser", "os",
                    ] {
                        assert!(visit.get(field).is_some(), "Visit contains {field}");
                    }
                    assert_eq!(visit["short_code"], "report-records");
                    assert_eq!(visit["link_id"], old["link_id"]);
                    assert!(visit["occurred_at"].as_str().unwrap().ends_with('Z'));
                }
                assert_eq!(old["redirect_url"], "https://history.example.test/A?edition=old#part");
                assert_eq!(old["client_ip"], "192.0.2.41");
                assert_eq!(old["referrer"], "https://referrer.example.test/page?q=secret");
                assert_eq!(old["user_agent"], "Example Browser/1.0");
                assert_eq!(new["redirect_url"], "https://history.example.test/B?edition=new#part");
                assert_eq!(new["client_ip"], "192.0.2.42", "mapped IPv6 is canonicalized to IPv4");
                assert_eq!(new["referrer"], "", "empty raw header remains distinct from null");
                assert_eq!(new["user_agent"], "Example Browser/2.0");
                assert_eq!(bot["method"], "GET");
                assert_eq!(bot["redirect_url"], "https://history.example.test/B?edition=new#part");
                assert_eq!(bot["referrer"], "https://bot-preview.example.test/");

                let stats = call(
                    router.clone(),
                    request(Method::GET, "/api/v1/links/report-records/stats", Some(&key), ""),
                )
                .await;
                assert_eq!(stats.status(), StatusCode::OK);
                assert_eq!(json(stats).await, serde_json::json!({"lifetime_visits":3,"matching_visits":3}));

                let before = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                let head = call(
                    router.clone(),
                    request(Method::HEAD, "/api/v1/links/report-records/visits?from=bad", None, ""),
                )
                .await;
                assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(head.headers()[header::ALLOW], "GET");
                assert!(!head.headers().contains_key(header::LOCATION));
                assert!(head.into_body().collect().await.unwrap().to_bytes().is_empty());
                let no_auth = call(
                    router.clone(),
                    request(Method::GET, "/api/v1/links/report-records/visits", None, ""),
                )
                .await;
                assert_eq!(no_auth.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(no_auth.headers()[header::WWW_AUTHENTICATE], "Bearer");
                let _ = json(no_auth).await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before,
                    "report reads, errors, and unsupported methods do not create visits"
                );
            })
        })
        .await;
}
