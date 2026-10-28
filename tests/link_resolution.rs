#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{call, json, migrated_database, request, seed_account};
use axum::{body::Body, extract::ConnectInfo, http::Method, http::StatusCode};
use ferl_server::{
    app::{self, AppState},
    config::Config,
    domain::url::StoredUrl,
    links::{ActiveLinkSnapshot, LinkFailure, resolve_active_link},
};
use std::{collections::BTreeMap, net::SocketAddr, time::Duration};
use tokio::time::timeout;

fn production_state(database_url: &str, extra: &[(&str, &str)]) -> AppState {
    let mut values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://resolver.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
    ]);
    for (name, value) in extra {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let config = Config::from_map(values).expect("resolver production config is valid");
    AppState::from_config(&config).expect("production state composes")
}

fn anonymous_create(body: &str) -> axum::http::Request<Body> {
    let mut request = request(Method::POST, "/api/v1/links", None, body);
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 94], 4567))));
    request
}

#[tokio::test]
async fn fresh_active_link_resolver_tracks_http_edits_disable_suspension_and_deletion() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "resolver_admin", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "resolver_owner", "user", true).await;
                let state = production_state(
                    database.schema_database_url(),
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                );
                state.verify_schema().await.unwrap();
                let application = app::router(state.clone());

                let anonymous = call(
                    application.clone(),
                    anonymous_create(
                        r#"{"destination_url":"HTTPS://Example.invalid/path?q=1&q=2#fragment","tags":["Resolver"]}"#,
                    ),
                )
                .await;
                assert_eq!(anonymous.status(), StatusCode::CREATED);
                let anonymous = json(anonymous).await;
                let anon_code = anonymous["link"]["short_code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let token = anonymous["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let resolved = resolve_active_link(&state.pool, &anon_code, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .expect("new anonymous enabled link resolves");
                assert_snapshot(
                    &resolved,
                    &anon_code,
                    "https://example.invalid/path?q=1&q=2#fragment",
                );

                let changed = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/links/{anon_code}"),
                        Some(&token),
                        r#"{"destination_url":"https://edited.example/a?b=2#next"}"#,
                    ),
                )
                .await;
                assert_eq!(changed.status(), StatusCode::OK);
                let _ = json(changed).await;
                let resolved = resolve_active_link(&state.pool, &anon_code, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .unwrap();
                assert_snapshot(
                    &resolved,
                    &anon_code,
                    "https://edited.example/a?b=2#next",
                );

                let disabled = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/links/{anon_code}"),
                        Some(&token),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;
                assert!(resolve_active_link(&state.pool, &anon_code, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .is_none());
                let enabled = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/links/{anon_code}"),
                        Some(&token),
                        r#"{"enabled":true}"#,
                    ),
                )
                .await;
                assert_eq!(enabled.status(), StatusCode::OK);
                let _ = json(enabled).await;
                assert!(resolve_active_link(&state.pool, &anon_code, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .is_some());

                let owned = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://owned-resolver.example/","short_code":"owned-resolver"}"#,
                    ),
                )
                .await;
                assert_eq!(owned.status(), StatusCode::CREATED);
                let _ = json(owned).await;
                assert!(resolve_active_link(
                    &state.pool,
                    "owned-resolver",
                    Duration::from_secs(2)
                )
                .await
                .unwrap()
                .is_some());
                let suspended = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(suspended.status(), StatusCode::OK);
                let _ = json(suspended).await;
                assert!(resolve_active_link(
                    &state.pool,
                    "owned-resolver",
                    Duration::from_secs(2)
                )
                .await
                .unwrap()
                .is_none());
                let resumed = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":true}"#,
                    ),
                )
                .await;
                assert_eq!(resumed.status(), StatusCode::OK);
                let _ = json(resumed).await;
                assert!(resolve_active_link(
                    &state.pool,
                    "owned-resolver",
                    Duration::from_secs(2)
                )
                .await
                .unwrap()
                .is_some());

                let deleted = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/links/{anon_code}"),
                        Some(&token),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                let _ = account_support::bytes(deleted).await;
                assert!(resolve_active_link(&state.pool, &anon_code, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .is_none());
                assert!(resolve_active_link(&state.pool, "missing-resolver", Duration::from_secs(2))
                    .await
                    .unwrap()
                    .is_none());

                // Persisted corruption is distinct from an unavailable code.
                sqlx::query(
                    "UPDATE links SET destination_url = 'https://', destination_search = 'https://' WHERE short_code = 'owned-resolver'",
                )
                .execute(database.pool())
                .await
                .unwrap();
                assert!(matches!(
                    resolve_active_link(
                        &state.pool,
                        "owned-resolver",
                        Duration::from_secs(2)
                    )
                    .await,
                    Err(LinkFailure::Internal)
                ));

                let unavailable = production_state(
                    "postgres://ferl:ferl@127.0.0.1:1/ferl_test",
                    &[("FERL_DB_ACQUIRE_TIMEOUT_SECS", "1")],
                );
                assert!(matches!(
                    resolve_active_link(
                        &unavailable.pool,
                        "missing-resolver",
                        Duration::from_secs(2)
                    )
                    .await,
                    Err(LinkFailure::StorageUnavailable)
                ));
                drop(unavailable);
                drop(application);
                let pool = state.pool.clone();
                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before schema cleanup");
            })
        })
        .await;
}

fn assert_snapshot(snapshot: &ActiveLinkSnapshot, code: &str, destination: &str) {
    assert_eq!(snapshot.short_code().as_str(), code);
    assert_eq!(snapshot.destination_url().as_str(), destination);
    let reparsed = StoredUrl::from_database(snapshot.destination_url().as_str().to_owned())
        .expect("resolver yields a checked serialized destination");
    assert_eq!(reparsed.as_str(), destination);
}
