#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi;

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
    seed_anonymous_link,
};
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use ferl_server::domain::secret::AccountApiKey;
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};
use tokio::time::timeout;
use tower::ServiceExt;

fn anonymous_request(body: Body) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/links")
        .header("content-type", "application/json")
        .header("x-request-id", "client-value")
        .body(body)
        .expect("test request is valid");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 63], 4567))));
    request
}

fn unread_body() -> (Body, Arc<AtomicBool>) {
    let polled = Arc::new(AtomicBool::new(false));
    let body_polled = Arc::clone(&polled);
    let stream = futures_util::stream::poll_fn(move |_| {
        body_polled.store(true, Ordering::SeqCst);
        Poll::<Option<Result<Vec<u8>, Infallible>>>::Pending
    });
    (Body::from_stream(stream), polled)
}

#[tokio::test]
async fn route_and_eligibility_rejections_precede_body_reads_and_allowance_charges() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, token) = seed_anonymous_link(database.pool(), "order-token").await;
                let (_disabled_id, disabled_key) =
                    seed_account(database.pool(), "order_disabled_key", "user", false).await;
                let (revoked_id, revoked_key) =
                    seed_account(database.pool(), "order_revoked_key", "user", true).await;
                let revoked_replacement = AccountApiKey::generate().unwrap();
                sqlx::query("UPDATE accounts SET api_key_digest = $2 WHERE id = $1")
                    .bind(revoked_id.as_uuid())
                    .bind(revoked_replacement.digest().as_bytes().as_slice())
                    .execute(database.pool())
                    .await
                    .unwrap();
                let (deleted_id, deleted_key) =
                    seed_account(database.pool(), "order_deleted_key", "user", true).await;
                sqlx::query("DELETE FROM accounts WHERE id = $1")
                    .bind(deleted_id.as_uuid())
                    .execute(database.pool())
                    .await
                    .unwrap();
                let unmatched = AccountApiKey::generate().unwrap();
                let disabled = production_router_with(database, &[]).await;
                let enabled =
                    production_router_with(database, &[("FERL_ANONYMOUS_CREATION", "true")]).await;

                let (body, polled) = unread_body();
                let disabled_response = call(disabled, anonymous_request(body)).await;
                assert_eq!(disabled_response.status(), StatusCode::FORBIDDEN);
                assert_eq!(
                    json(disabled_response).await["error"]["code"],
                    "anonymous_creation_disabled"
                );
                assert!(!polled.load(Ordering::SeqCst));

                let invalid_credentials = [
                    "not-a-credential",
                    unmatched.as_str(),
                    disabled_key.as_str(),
                    revoked_key.as_str(),
                    deleted_key.as_str(),
                ];
                for credential in invalid_credentials {
                    let (body, polled) = unread_body();
                    let mut request = anonymous_request(body);
                    request.headers_mut().insert(
                        "authorization",
                        format!("Bearer {credential}").parse().unwrap(),
                    );
                    let response = call(enabled.clone(), request).await;
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                    assert_eq!(response.headers()["www-authenticate"], "Bearer");
                    assert!(!polled.load(Ordering::SeqCst));
                    assert_eq!(json(response).await["error"]["code"], "invalid_credentials");
                }

                let (body, polled) = unread_body();
                let mut wrong_scope = anonymous_request(body);
                wrong_scope
                    .headers_mut()
                    .insert("authorization", format!("Bearer {token}").parse().unwrap());
                let response = call(enabled.clone(), wrong_scope).await;
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert_eq!(json(response).await["error"]["code"], "forbidden");
                assert!(!polled.load(Ordering::SeqCst));

                let (body, polled) = unread_body();
                let mut wrong_method = anonymous_request(body);
                *wrong_method.method_mut() = Method::PUT;
                let response = ServiceExt::oneshot(enabled.clone(), wrong_method)
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(response.headers()["allow"], "POST");
                assert!(!polled.load(Ordering::SeqCst));
                let headers = response.headers().clone();
                let body = json(response).await;
                assert!(openapi::validates_response(
                    "MethodNotAllowed",
                    &headers,
                    Some(&body),
                    false
                ));

                let (body, polled) = unread_body();
                let unknown = Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/links/missing/visits")
                    .body(body)
                    .unwrap();
                let response = ServiceExt::oneshot(enabled.clone(), unknown).await.unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert!(!polled.load(Ordering::SeqCst));
                let _ = json(response).await;

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "eligibility, unknown route and wrong method do not charge"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "only the fixture link exists"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn every_eligible_invalid_attempt_is_charged_after_prepare() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let application = production_router_with(
                    database,
                    &[
                        ("FERL_ANONYMOUS_CREATION", "true"),
                        ("FERL_ANONYMOUS_CREATE_LIMIT", "32"),
                    ],
                )
                .await;
                let cases = [
                    (
                        Body::from(r#"{"destination_url":"https://valid.example/""#),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"https://first.example/","destination_url":"https://second.example/"}"#,
                        ),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(r#"{"destination_url":"https://valid.example/"}"#),
                        "text/plain",
                        StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    ),
                    (
                        Body::from(vec![b'x'; 65_537]),
                        "application/json",
                        StatusCode::PAYLOAD_TOO_LARGE,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"https://valid.example/","unknown":true}"#,
                        ),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(r#"{"destination_url":"javascript:alert(1)"}"#),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(r#"{"destination_url":"https://valid.example/","title":"  "}"#),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(r#"{"destination_url":"https://valid.example/","tags":[null]}"#),
                        "application/json",
                        StatusCode::BAD_REQUEST,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"https://valid.example/","short_code":null}"#,
                        ),
                        "application/json",
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"https://valid.example/","short_code":42}"#,
                        ),
                        "application/json",
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"https://valid.example/","short_code":"BAD!"}"#,
                        ),
                        "application/json",
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        Body::from(
                            r#"{"destination_url":"javascript:bad","short_code":"BAD!"}"#,
                        ),
                        "application/json",
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        Body::from(r#"{"destination_url":"https://valid.example/"}"#),
                        "application/json; charset=utf-16",
                        StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    ),
                ];
                for (index, (body, content_type, expected)) in cases.into_iter().enumerate() {
                    let mut request = anonymous_request(body);
                    request.headers_mut().insert(
                        "content-type",
                        content_type
                            .parse()
                            .expect("fixture media type is a header"),
                    );
                    let response = call(application.clone(), request).await;
                    assert_eq!(response.status(), expected, "invalid attempt {index}");
                    let headers = response.headers().clone();
                    let body = json(response).await;
                    assert_eq!(
                        body["error"]["code"],
                        match expected {
                            StatusCode::BAD_REQUEST => "invalid_request",
                            StatusCode::FORBIDDEN => "forbidden",
                            StatusCode::PAYLOAD_TOO_LARGE => "request_too_large",
                            StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
                            _ => unreachable!(),
                        },
                        "invalid attempt {index}"
                    );
                    let component = match expected {
                        StatusCode::BAD_REQUEST => "BadRequest",
                        StatusCode::FORBIDDEN => "Forbidden",
                        StatusCode::PAYLOAD_TOO_LARGE => "TooLarge",
                        StatusCode::UNSUPPORTED_MEDIA_TYPE => "UnsupportedMediaType",
                        _ => unreachable!(),
                    };
                    assert!(openapi::validates_response(
                        component,
                        &headers,
                        Some(&body),
                        false
                    ));
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                            .fetch_one(database.pool())
                            .await
                            .unwrap(),
                        i64::try_from(index + 1).unwrap(),
                        "invalid attempt {index} remains charged"
                    );
                }
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "failed validation never creates a reservation"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
            })
        })
        .await;
}

#[tokio::test]
async fn account_rotation_after_charge_and_before_body_completion_blocks_creation() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, old_key) =
                    seed_account(database.pool(), "charge_then_rotate", "user", true).await;
                let application = production_router_with(database, &[]).await;
                let body_polled = Arc::new(AtomicBool::new(false));
                let observed = Arc::clone(&body_polled);
                let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
                let stream = futures_util::stream::once(async move {
                    observed.store(true, Ordering::SeqCst);
                    let _ = release_rx.await;
                    Ok::<_, Infallible>(br#"{"destination_url":"https://stale.example/","short_code":"stale-create"}"#.to_vec())
                });
                let create_request = Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/links")
                    .header("authorization", format!("Bearer {old_key}"))
                    .header("content-type", "application/json")
                    .body(Body::from_stream(stream))
                    .unwrap();
                let mut requests = tokio::task::JoinSet::new();
                let create_app = application.clone();
                requests.spawn(async move {
                    ServiceExt::oneshot(create_app, create_request)
                        .await
                        .unwrap()
                });
                timeout(std::time::Duration::from_secs(3), async {
                    while !body_polled.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("body starts only after the allowance COMMIT is acknowledged");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );

                let rotated = call(
                    application.clone(),
                    request(Method::POST, "/api/v1/me/api-key", Some(&old_key), ""),
                )
                .await;
                assert_eq!(rotated.status(), StatusCode::OK);
                let rotated = json(rotated).await;
                let new_key = rotated["api_key"].as_str().unwrap().to_owned();
                assert_ne!(old_key, new_key);
                release_tx.send(()).expect("body waiter is still active");
                let rejected = timeout(std::time::Duration::from_secs(5), requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(rejected).await["error"]["code"], "invalid_credentials");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "key rotation cannot undo the already committed allowance"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "stale authority cannot create a link after rotation"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
            })
        })
        .await;
}

#[tokio::test]
async fn default_public_create_limits_charge_five_anonymous_and_sixty_account_attempts() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, account_key) =
                    seed_account(database.pool(), "default_link_creator", "user", true).await;
                let application =
                    production_router_with(database, &[("FERL_ANONYMOUS_CREATION", "true")]).await;
                for attempt in 1..=5 {
                    let response = call(
                        application.clone(),
                        anonymous_request(Body::from(
                            r#"{"destination_url":"https://anonymous-default.example/"}"#,
                        )),
                    )
                    .await;
                    assert_eq!(
                        response.status(),
                        StatusCode::CREATED,
                        "anonymous {attempt}"
                    );
                    let _ = json(response).await;
                }
                let rejected = call(
                    application.clone(),
                    anonymous_request(Body::from(
                        r#"{"destination_url":"https://sixth-anonymous.example/"}"#,
                    )),
                )
                .await;
                assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
                let retry_after = rejected.headers()["retry-after"]
                    .to_str()
                    .unwrap()
                    .parse::<u32>()
                    .unwrap();
                assert!(retry_after > 0);
                assert_eq!(json(rejected).await["error"]["code"], "rate_limited");

                for attempt in 1..=60 {
                    let response = call(
                        application.clone(),
                        request(
                            Method::POST,
                            "/api/v1/links",
                            Some(&account_key),
                            r#"{"destination_url":"https://account-default.example/"}"#,
                        ),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::CREATED, "account {attempt}");
                    let _ = json(response).await;
                }
                let rejected = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&account_key),
                        r#"{"destination_url":"https://sixty-first-account.example/"}"#,
                    ),
                )
                .await;
                assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
                assert!(
                    rejected.headers()["retry-after"]
                        .to_str()
                        .unwrap()
                        .parse::<u32>()
                        .unwrap()
                        > 0
                );
                assert_eq!(json(rejected).await["error"]["code"], "rate_limited");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    65
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    65
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_buckets")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2
                );
            })
        })
        .await;
}
