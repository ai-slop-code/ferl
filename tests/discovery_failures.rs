#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use std::{collections::BTreeMap, time::Duration};

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
};
use axum::{
    Router,
    http::{Method, StatusCode},
};
use ferl_server::{
    app::{self, AppState},
    config::Config,
};
use tokio::time::{sleep, timeout};
use tower::ServiceExt;

fn router_for_unavailable_database(database_url: &str) -> Router {
    let values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://failure.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_POOL_MAX".to_owned(), "1".to_owned()),
        ("FERL_DB_ACQUIRE_TIMEOUT_SECS".to_owned(), "1".to_owned()),
    ]);
    let config = Config::from_map(values).unwrap();
    app::router(AppState::from_config(&config).unwrap())
}

async fn wait_for_discovery_lock_wait(pool: &sqlx::PgPool) {
    timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                   AND query LIKE '%FROM links l LEFT JOIN accounts a%')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("discovery statement enters the controlled lock wait before deadline");
}

#[tokio::test]
async fn storage_lock_and_connection_failures_are_503_not_empty_and_cancellation_recovers() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "discovery_failure", "user", true).await;
                let app = production_router_with(
                    database,
                    &[
                        ("FERL_DB_POOL_MAX", "1"),
                        ("FERL_DB_LOCK_TIMEOUT_SECS", "1"),
                        ("FERL_LINK_OPERATION_TIMEOUT_SECS", "5"),
                    ],
                )
                .await;

                let mut lock = database.pool().begin().await.unwrap();
                sqlx::query("LOCK TABLE links IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *lock)
                    .await
                    .unwrap();
                for path in [
                    "/api/v1/links?q=FILTER-PRIVATE-CANARY",
                    "/api/v1/tags?q=FILTER-PRIVATE-CANARY",
                ] {
                    let started = tokio::time::Instant::now();
                    let response =
                        call(app.clone(), request(Method::GET, path, Some(&key), "")).await;
                    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
                    let body = json(response).await;
                    assert_eq!(body["error"]["code"], "storage_unavailable");
                    assert!(
                        started.elapsed() < Duration::from_secs(5),
                        "lock timeout is bounded for {path}"
                    );
                    assert!(!body.to_string().contains("FILTER-PRIVATE-CANARY"));
                }
                lock.rollback().await.unwrap();

                let recovered = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links?limit=1", Some(&key), ""),
                )
                .await;
                assert_eq!(recovered.status(), StatusCode::OK);
                assert_eq!(
                    json(recovered).await,
                    serde_json::json!({"items": [], "next_cursor": null})
                );

                let mut cancellation_lock = database.pool().begin().await.unwrap();
                sqlx::query("LOCK TABLE links IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *cancellation_lock)
                    .await
                    .unwrap();
                let cancellation_app = app.clone();
                let cancellation_key = key.clone();
                let task = tokio::spawn(async move {
                    cancellation_app
                        .oneshot(request(
                            Method::GET,
                            "/api/v1/links?q=CANCEL-PRIVATE-CANARY",
                            Some(&cancellation_key),
                            "",
                        ))
                        .await
                });
                wait_for_discovery_lock_wait(database.pool()).await;
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                cancellation_lock.rollback().await.unwrap();
                let recovered =
                    call(app, request(Method::GET, "/api/v1/tags", Some(&key), "")).await;
                assert_eq!(recovered.status(), StatusCode::OK);
                assert_eq!(json(recovered).await["items"], serde_json::json!([]));
            })
        })
        .await;

    let unavailable = router_for_unavailable_database("postgres://ferl:ferl@127.0.0.1:1/ferl_test");
    let valid_but_unknown_key = ferl_server::domain::secret::AccountApiKey::generate().unwrap();
    for path in [
        "/api/v1/links?q=DATABASE-PRIVATE-CANARY",
        "/api/v1/tags?q=DATABASE-PRIVATE-CANARY",
    ] {
        let response = call(
            unavailable.clone(),
            request(Method::GET, path, Some(valid_but_unknown_key.as_str()), ""),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let body = json(response).await;
        assert_eq!(body["error"]["code"], "storage_unavailable");
        assert!(!body.to_string().contains("DATABASE-PRIVATE-CANARY"));
    }
}
