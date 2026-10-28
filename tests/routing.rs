#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::{net::SocketAddr, time::Duration};

use account_support::{migrated_database, seed_anonymous_link};
use axum::http::{Method, StatusCode};
use ferl_server::{app, error::ErrorCode};
use http_body_util::BodyExt;
use redirect_support::{call, request, state};
use tokio::time::timeout;
use uuid::Uuid;

const PEER: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 41000);

#[tokio::test]
async fn raw_path_ownership_methods_and_fallbacks_are_exact() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                for code in ["valid-code", "assets", "health", "api-sale"] {
                    seed_anonymous_link(database.pool(), code).await;
                }
                let state = state(database.schema_database_url(), &[]);
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());

                for code in ["valid-code", "assets", "health", "api-sale"] {
                    let response =
                        call(app.clone(), request(Method::GET, &format!("/{code}"), PEER)).await;
                    assert_eq!(response.status(), StatusCode::FOUND, "{code}");
                    assert!(response.headers().contains_key(http::header::LOCATION));
                    assert_eq!(response.headers()[http::header::CACHE_CONTROL], "no-store");
                    let request_id = response.headers()["x-request-id"].to_str().unwrap();
                    assert_ne!(request_id, "client-value");
                    Uuid::parse_str(request_id).unwrap();
                    assert!(
                        timeout(Duration::from_secs(2), response.into_body().collect())
                            .await
                            .unwrap()
                            .unwrap()
                            .to_bytes()
                            .is_empty(),
                        "redirect bodies are empty"
                    );
                }

                for (path, method, status) in [
                    ("/", Method::GET, StatusCode::NOT_FOUND),
                    ("/missing", Method::GET, StatusCode::NOT_FOUND),
                    ("/valid-code/", Method::GET, StatusCode::NOT_FOUND),
                    ("/valid-code/extra", Method::GET, StatusCode::NOT_FOUND),
                    ("//valid-code", Method::GET, StatusCode::NOT_FOUND),
                    ("/valid%2dcode", Method::GET, StatusCode::NOT_FOUND),
                    ("/%76alid-code", Method::GET, StatusCode::NOT_FOUND),
                    ("/VALID-CODE", Method::GET, StatusCode::NOT_FOUND),
                    ("/assets/missing.js", Method::GET, StatusCode::NOT_FOUND),
                    ("/api", Method::GET, StatusCode::NOT_FOUND),
                    ("/api/", Method::GET, StatusCode::NOT_FOUND),
                    ("/api/unknown", Method::GET, StatusCode::NOT_FOUND),
                ] {
                    let response = call(app.clone(), request(method, path, PEER)).await;
                    assert_eq!(response.status(), status, "{path}");
                    assert!(
                        !response.headers().contains_key(http::header::LOCATION),
                        "{path}"
                    );
                    assert_eq!(response.headers()[http::header::CACHE_CONTROL], "no-store");
                    let request_id = response.headers()["x-request-id"]
                        .to_str()
                        .unwrap()
                        .to_owned();
                    Uuid::parse_str(&request_id).unwrap();
                    let body = timeout(Duration::from_secs(2), response.into_body().collect())
                        .await
                        .unwrap()
                        .unwrap()
                        .to_bytes();
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(json["error"]["code"], "not_found");
                    assert_eq!(json["request_id"], request_id);
                }

                for path in ["/missing", "/api/unknown", "/valid-code/extra"] {
                    let response = call(app.clone(), request(Method::HEAD, path, PEER)).await;
                    assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
                    assert!(!response.headers().contains_key(http::header::LOCATION));
                    assert!(
                        response
                            .into_body()
                            .collect()
                            .await
                            .unwrap()
                            .to_bytes()
                            .is_empty()
                    );
                }

                for method in [Method::POST, Method::OPTIONS, Method::PUT] {
                    let response = call(app.clone(), request(method, "/valid-code", PEER)).await;
                    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                    assert_eq!(response.headers()[http::header::ALLOW], "GET, HEAD");
                    assert!(!response.headers().contains_key(http::header::LOCATION));
                    let body: serde_json::Value = serde_json::from_slice(
                        &response.into_body().collect().await.unwrap().to_bytes(),
                    )
                    .unwrap();
                    assert_eq!(body["error"]["code"], ErrorCode::MethodNotAllowed.as_str());
                }

                let health =
                    call(app.clone(), request(Method::GET, "/api/health/live", PEER)).await;
                assert_eq!(health.status(), StatusCode::OK);
                assert_eq!(health.headers()[http::header::CACHE_CONTROL], "no-store");

                let missing_peer_request = axum::http::Request::builder()
                    .method(Method::GET)
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap();
                let missing_peer = call(app.clone(), missing_peer_request).await;
                assert_eq!(missing_peer.status(), StatusCode::FOUND);
                assert_eq!(
                    missing_peer.headers()[http::header::LOCATION],
                    "https://destination.example.test/path"
                );
                assert!(missing_peer.into_body().collect().await.unwrap().to_bytes().is_empty());

                sqlx::query(
                    "UPDATE links SET destination_url = 'https://', destination_search = 'https://' \
                     WHERE short_code = 'valid-code'",
                )
                .execute(database.pool())
                .await
                .unwrap();
                let corrupt = call(app.clone(), request(Method::GET, "/valid-code", PEER)).await;
                assert_eq!(corrupt.status(), StatusCode::INTERNAL_SERVER_ERROR);
                assert!(!corrupt.headers().contains_key(http::header::LOCATION));
                let corrupt_body: serde_json::Value = serde_json::from_slice(
                    &corrupt.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(corrupt_body["error"]["code"], "internal_error");

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "API, health, path aliases, 404 and 405 never persist visits"
                );

                let pool = state.pool.clone();
                drop(app);
                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before schema cleanup");
            })
        })
        .await;
}

#[tokio::test]
async fn resolution_storage_failure_is_not_fabricated_as_missing() {
    let unavailable = state(
        "postgres://ferl:ferl@127.0.0.1:1/ferl_test",
        &[("FERL_DB_ACQUIRE_TIMEOUT_SECS", "1")],
    );
    let app = app::router(unavailable.clone());
    for (method, bodyless) in [(Method::GET, false), (Method::HEAD, true)] {
        let response = call(app.clone(), request(method, "/storage-outage", PEER)).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key(http::header::LOCATION));
        assert_eq!(response.headers()[http::header::CACHE_CONTROL], "no-store");
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        Uuid::parse_str(&request_id).unwrap();
        let bytes = timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        if bodyless {
            assert!(bytes.is_empty(), "HEAD storage errors have no body");
        } else {
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "storage_unavailable");
            assert_eq!(body["request_id"], request_id);
        }
    }
    let pool = unavailable.pool.clone();
    drop(app);
    drop(unavailable);
    timeout(Duration::from_secs(5), pool.close())
        .await
        .expect("failed-resolution pool closes before test exit");
}
