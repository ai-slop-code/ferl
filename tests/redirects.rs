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

use account_support::{
    migrated_database, request as api_request, seed_account, seed_anonymous_link,
};
use axum::http::{HeaderValue, Method, StatusCode, header};
use ferl_server::app;
use http_body_util::BodyExt;
use redirect_support::{call, request, state, wait_for_visits};
use tokio::time::timeout;

#[tokio::test]
async fn redirect_and_visit_use_one_immutable_resolution_snapshot() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (link_id, token) = seed_anonymous_link(database.pool(), "snapshot-code").await;
                let state = state(database.schema_database_url(), &[]);
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());

                let destination_a = "http://127.0.0.1:9/private/a?x=1&x=2#section-a";
                let patched = account_support::call(
                    app.clone(),
                    api_request(
                        Method::PATCH,
                        "/api/v1/links/snapshot-code",
                        Some(&token),
                        &format!(r#"{{"destination_url":"{destination_a}"}}"#),
                    ),
                )
                .await;
                assert_eq!(patched.status(), StatusCode::OK);
                let _ = account_support::json(patched).await;

                let before_a: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
                    "SELECT updated_at FROM links WHERE id = $1",
                )
                .bind(link_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();

                // Queue is deliberately not started yet: this request resolves
                // A and creates its event before the later edit to B.
                let mut first_request = request(
                    Method::GET,
                    "/snapshot-code?ferl_ak_v1_fake=ignored&campaign=discard-me",
                    SocketAddr::from(([198, 51, 100, 10], 45001)),
                );
                first_request
                    .headers_mut()
                    .append(header::AUTHORIZATION, HeaderValue::from_static("Bearer invalid-one"));
                first_request
                    .headers_mut()
                    .append(header::AUTHORIZATION, HeaderValue::from_static("Bearer invalid-two"));
                first_request.headers_mut().insert(
                    header::REFERER,
                    HeaderValue::from_static("https://referrer.example/path?q=private#frag"),
                );
                first_request.headers_mut().insert(
                    header::USER_AGENT,
                    HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36"),
                );
                let first = call(app.clone(), first_request).await;
                assert_eq!(first.status(), StatusCode::FOUND);
                assert_eq!(first.headers()[header::LOCATION], destination_a);
                assert_eq!(first.headers()[header::CACHE_CONTROL], "no-store");
                assert!(first.into_body().collect().await.unwrap().to_bytes().is_empty());

                let destination_b = "http://127.0.0.1:9/private/b?next=1#section-b";
                let patched = account_support::call(
                    app.clone(),
                    api_request(
                        Method::PATCH,
                        "/api/v1/links/snapshot-code",
                        Some(&token),
                        &format!(r#"{{"destination_url":"{destination_b}"}}"#),
                    ),
                )
                .await;
                assert_eq!(patched.status(), StatusCode::OK);
                let _ = account_support::json(patched).await;
                let after_b: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
                    "SELECT updated_at FROM links WHERE id = $1",
                )
                .bind(link_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert!(after_b >= before_a);

                let workers = state.start_analytics_workers().unwrap();
                wait_for_visits(database.pool(), "snapshot-code", 1).await;
                let first_visit = sqlx::query(
                    "SELECT redirect_url, method, host(client_ip) AS client_ip, referrer, user_agent, \
                            device, browser, os FROM visits WHERE link_id = $1",
                )
                .bind(link_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();
                use sqlx::Row;
                assert_eq!(first_visit.try_get::<String, _>("redirect_url").unwrap(), destination_a);
                assert_eq!(first_visit.try_get::<String, _>("method").unwrap(), "GET");
                assert_eq!(first_visit.try_get::<String, _>("client_ip").unwrap(), "198.51.100.10");
                assert_eq!(
                    first_visit.try_get::<Option<String>, _>("referrer").unwrap().as_deref(),
                    Some("https://referrer.example/path?q=private#frag")
                );
                assert_eq!(
                    first_visit.try_get::<Option<String>, _>("user_agent").unwrap().as_deref(),
                    Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36")
                );
                assert_eq!(first_visit.try_get::<String, _>("device").unwrap(), "desktop");
                assert_eq!(first_visit.try_get::<String, _>("browser").unwrap(), "Chrome");
                assert_eq!(first_visit.try_get::<String, _>("os").unwrap(), "Windows");

                let mut head_request = request(
                    Method::HEAD,
                    "/snapshot-code?tracking=must-not-be-appended",
                    SocketAddr::from(([198, 51, 100, 11], 45002)),
                );
                head_request.headers_mut().insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_static("Bearer malformed-but-ignored"),
                );
                let head = call(app.clone(), head_request).await;
                assert_eq!(head.status(), StatusCode::FOUND);
                assert_eq!(head.headers()[header::LOCATION], destination_b);
                assert_eq!(head.headers()[header::CACHE_CONTROL], "no-store");
                assert!(head.into_body().collect().await.unwrap().to_bytes().is_empty());
                wait_for_visits(database.pool(), "snapshot-code", 2).await;

                let methods = sqlx::query_scalar::<_, String>(
                    "SELECT method FROM visits WHERE link_id = $1 ORDER BY method",
                )
                .bind(link_id.as_uuid())
                .fetch_all(database.pool())
                .await
                .unwrap();
                assert_eq!(methods, ["GET", "HEAD"]);
                let destinations = sqlx::query_scalar::<_, String>(
                    "SELECT redirect_url FROM visits WHERE link_id = $1 ORDER BY occurred_at, id",
                )
                .bind(link_id.as_uuid())
                .fetch_all(database.pool())
                .await
                .unwrap();
                assert!(destinations.contains(&destination_a.to_owned()));
                assert!(destinations.contains(&destination_b.to_owned()));
                let count = sqlx::query_scalar::<_, i64>(
                    "SELECT visit_count FROM links WHERE id = $1",
                )
                .bind(link_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(count, 2);
                let after_visits: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
                    "SELECT updated_at FROM links WHERE id = $1",
                )
                .bind(link_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(after_visits, after_b, "analytics never touches updated_at");

                assert_eq!(
                    workers.shutdown(Duration::from_secs(5)).await,
                    ferl_server::analytics::WorkerStopOutcome::Drained
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
async fn enablement_suspension_and_deletion_apply_to_fresh_public_resolutions() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "redirect_admin", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "redirect_owner", "user", true).await;
                let (anonymous_id, token) =
                    seed_anonymous_link(database.pool(), "disabled-active").await;
                let state = state(database.schema_database_url(), &[]);
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());

                let created = account_support::call(
                    app.clone(),
                    api_request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"http://127.0.0.1:9/owned?fresh=1#active","short_code":"owned-active"}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let _ = account_support::json(created).await;
                let peer = SocketAddr::from(([198, 51, 100, 19], 45119));
                let initial = call(app.clone(), request(Method::GET, "/owned-active", peer)).await;
                assert_eq!(initial.status(), StatusCode::FOUND);
                assert_eq!(
                    initial.headers()[header::LOCATION],
                    "http://127.0.0.1:9/owned?fresh=1#active"
                );

                let suspended = account_support::call(
                    app.clone(),
                    api_request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(suspended.status(), StatusCode::OK);
                let _ = account_support::json(suspended).await;
                let suspended_miss =
                    call(app.clone(), request(Method::GET, "/owned-active", peer)).await;
                assert_eq!(suspended_miss.status(), StatusCode::NOT_FOUND);
                assert!(!suspended_miss.headers().contains_key(header::LOCATION));
                let suspended_body: serde_json::Value = serde_json::from_slice(
                    &suspended_miss
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes(),
                )
                .unwrap();

                let resumed = account_support::call(
                    app.clone(),
                    api_request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":true}"#,
                    ),
                )
                .await;
                assert_eq!(resumed.status(), StatusCode::OK);
                let _ = account_support::json(resumed).await;
                let active = call(app.clone(), request(Method::GET, "/owned-active", peer)).await;
                assert_eq!(active.status(), StatusCode::FOUND);
                assert_eq!(
                    active.headers()[header::LOCATION],
                    "http://127.0.0.1:9/owned?fresh=1#active"
                );

                let disabled = account_support::call(
                    app.clone(),
                    api_request(
                        Method::PATCH,
                        "/api/v1/links/disabled-active",
                        Some(&token),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = account_support::json(disabled).await;
                let disabled_miss =
                    call(app.clone(), request(Method::GET, "/disabled-active", peer)).await;
                assert_eq!(disabled_miss.status(), StatusCode::NOT_FOUND);
                assert!(!disabled_miss.headers().contains_key(header::LOCATION));
                let disabled_body: serde_json::Value = serde_json::from_slice(
                    &disabled_miss
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes(),
                )
                .unwrap();
                assert_eq!(
                    suspended_body["error"], disabled_body["error"],
                    "suspended and disabled links use identical safe error details"
                );

                let deleted = account_support::call(
                    app.clone(),
                    api_request(
                        Method::DELETE,
                        "/api/v1/links/disabled-active",
                        Some(&token),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                let _ = account_support::bytes(deleted).await;
                let deleted_miss =
                    call(app.clone(), request(Method::HEAD, "/disabled-active", peer)).await;
                assert_eq!(deleted_miss.status(), StatusCode::NOT_FOUND);
                assert!(!deleted_miss.headers().contains_key(header::LOCATION));
                assert!(
                    deleted_miss
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "failed redirects never enqueue/persist visits"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM code_reservations WHERE short_code = 'disabled-active'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE id = $1")
                        .bind(anonymous_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
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
