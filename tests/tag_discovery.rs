#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
    seed_anonymous_link,
};
use axum::http::{Method, StatusCode};
use ferl_server::domain::text::ComparisonText;
use serde_json::Value;
use uuid::Uuid;

async fn create(app: &axum::Router, key: &str, code: &str, destination: &str, tags: &[&str]) {
    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &serde_json::to_string(&serde_json::json!({
                "destination_url": destination,
                "short_code": code,
                "tags": tags,
            }))
            .unwrap(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let _ = json(response).await;
}

async fn discover(app: &axum::Router, key: &str, query: &str) -> Value {
    let response = call(
        app.clone(),
        request(Method::GET, &format!("/api/v1/tags{query}"), Some(key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{query}");
    json(response).await
}

#[tokio::test]
async fn tag_counts_cover_visible_live_links_and_cursor_pages_groups_not_links() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (admin_id, admin_key) =
                    seed_account(database.pool(), "tag_admin", "admin", true).await;
                let (owner_a, key_a) =
                    seed_account(database.pool(), "tag_owner_a", "user", true).await;
                let (owner_b, key_b) =
                    seed_account(database.pool(), "tag_owner_b", "user", true).await;
                let (suspended_owner, _) =
                    seed_account(database.pool(), "tag_suspended", "user", false).await;
                let app = production_router_with(database, &[("FERL_DB_POOL_MAX", "1")]).await;

                create(
                    &app,
                    &key_a,
                    "tag-owner-a1",
                    "https://shared.example.test/?dest-only=searchcanary",
                    &["Shared", "AlphaOnly", " shared "],
                )
                .await;
                create(
                    &app,
                    &key_a,
                    "tag-owner-a2",
                    "https://disabled.example.test/",
                    &["shared", "Disabled"],
                )
                .await;
                create(
                    &app,
                    &key_b,
                    "tag-owner-b1",
                    "https://beta.example.test/",
                    &["Shared", "BetaOnly"],
                )
                .await;
                create(
                    &app,
                    &admin_key,
                    "tag-admin-own",
                    "https://admin.example.test/",
                    &["AdminOwn"],
                )
                .await;
                let disabled = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/tag-owner-a2",
                        Some(&key_a),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;

                let (anonymous_id, anonymous_token) =
                    seed_anonymous_link(database.pool(), "tag-anonymous").await;
                let suspended_id = Uuid::new_v4();
                let suspended_destination = "https://suspended.example.test/";
                let suspended_search = ComparisonText::from_persisted(suspended_destination);
                sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('tag-suspended')")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query(
                    "INSERT INTO links \
                     (id, short_code, owner_id, destination_url, destination_search, enabled, \
                      visit_count, management_token_digest) \
                     VALUES ($1, 'tag-suspended', $2, $3, $4, true, 0, NULL)",
                )
                .bind(suspended_id)
                .bind(suspended_owner.as_uuid())
                .bind(suspended_destination)
                .bind(suspended_search.as_str())
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "INSERT INTO link_tags (link_id, name) VALUES \
                     ($1, 'shared'), ($2, 'shared')",
                )
                .bind(anonymous_id.as_uuid())
                .bind(suspended_id)
                .execute(database.pool())
                .await
                .unwrap();

                let own_a = discover(&app, &key_a, "").await;
                assert_eq!(
                    own_a["items"],
                    serde_json::json!([
                        {"name":"alphaonly", "link_count":1},
                        {"name":"disabled", "link_count":1},
                        {"name":"shared", "link_count":2}
                    ])
                );
                let admin_own = discover(&app, &admin_key, "?scope=own").await;
                assert_eq!(
                    admin_own["items"],
                    serde_json::json!([{"name":"adminown", "link_count":1}])
                );

                let first = discover(&app, &key_a, "?limit=1").await;
                assert_eq!(
                    first["items"],
                    serde_json::json!([{"name":"alphaonly", "link_count":1}])
                );
                let first_cursor = first["next_cursor"].as_str().unwrap();
                let second =
                    discover(&app, &key_a, &format!("?limit=1&cursor={first_cursor}")).await;
                assert_eq!(
                    second["items"],
                    serde_json::json!([{"name":"disabled", "link_count":1}])
                );
                let second_cursor = second["next_cursor"].as_str().unwrap();
                let third =
                    discover(&app, &key_a, &format!("?limit=1&cursor={second_cursor}")).await;
                assert_eq!(
                    third["items"],
                    serde_json::json!([{"name":"shared", "link_count":2}])
                );
                assert_eq!(third["next_cursor"], Value::Null);

                let all = discover(&app, &admin_key, "?scope=all&q=shared").await;
                assert_eq!(
                    all["items"],
                    serde_json::json!([{"name":"shared", "link_count":5}])
                );
                let b_only = discover(
                    &app,
                    &admin_key,
                    &format!("?scope=all&owner_id={}", owner_b.as_uuid()),
                )
                .await;
                assert_eq!(
                    b_only["items"],
                    serde_json::json!([
                        {"name":"betaonly", "link_count":1},
                        {"name":"shared", "link_count":1}
                    ])
                );
                let anonymous =
                    discover(&app, &admin_key, "?scope=all&owner_id=anonymous&q=shared").await;
                assert_eq!(
                    anonymous["items"],
                    serde_json::json!([{"name":"shared", "link_count":1}])
                );
                assert_eq!(
                    discover(&app, &key_a, "?q=searchcanary").await["items"],
                    serde_json::json!([]),
                    "tag q does not search destination URLs"
                );
                assert_eq!(
                    discover(&app, &key_a, "?q=SHARED").await["items"],
                    serde_json::json!([{"name":"shared", "link_count":2}])
                );

                let response = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/tags?q=%GG",
                        Some(&anonymous_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert_eq!(json(response).await["error"]["code"], "forbidden");
                let invalid_auth = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/tags?q=%GG",
                        Some("not-a-valid-credential"),
                        "",
                    ),
                )
                .await;
                assert_eq!(invalid_auth.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(invalid_auth.headers()["www-authenticate"], "Bearer");
                assert_eq!(
                    json(invalid_auth).await["error"]["code"],
                    "invalid_credentials"
                );

                let before_clear = discover(&app, &key_a, "?limit=1").await;
                assert_eq!(before_clear["items"][0]["name"], "alphaonly");
                let before_clear_cursor = before_clear["next_cursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let owner_tags = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/tag-owner-a2",
                        Some(&key_a),
                        r#"{"tags":[]}"#,
                    ),
                )
                .await;
                assert_eq!(owner_tags.status(), StatusCode::OK);
                let _ = json(owner_tags).await;
                let after_clear = discover(
                    &app,
                    &key_a,
                    &format!("?limit=1&cursor={before_clear_cursor}"),
                )
                .await;
                assert_eq!(
                    after_clear["items"],
                    serde_json::json!([{"name":"shared", "link_count":1}]),
                    "a committed clear before a continuation removes its group and recomputes other counts"
                );
                assert_eq!(after_clear["next_cursor"], Value::Null);
                assert_eq!(
                    discover(&app, &key_a, "?q=disabled").await["items"],
                    serde_json::json!([]),
                    "clearing the final own-scope association removes the name"
                );
                assert_eq!(
                    discover(&app, &admin_key, "?scope=all&q=shared").await["items"],
                    serde_json::json!([{"name":"shared", "link_count":4}]),
                    "the same name and other accounts' counts remain in all-scope"
                );

                let clear_shared = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/tag-owner-a1",
                        Some(&key_a),
                        r#"{"tags":["AlphaOnly"]}"#,
                    ),
                )
                .await;
                assert_eq!(clear_shared.status(), StatusCode::OK);
                let _ = json(clear_shared).await;
                assert_eq!(
                    discover(&app, &key_a, "?q=shared").await["items"],
                    serde_json::json!([])
                );
                assert_eq!(
                    discover(&app, &admin_key, "?scope=all&q=shared").await["items"],
                    serde_json::json!([{"name":"shared", "link_count":3}])
                );

                let deleted = call(
                    app.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", owner_b.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert_eq!(
                    discover(&app, &admin_key, "?scope=all&q=shared").await["items"],
                    serde_json::json!([{"name":"shared", "link_count":2}])
                );
                assert_eq!(
                    sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE id = $1")
                        .bind(owner_a.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    owner_a.as_uuid()
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "discovery and metadata edits do not create visits"
                );
                assert!(admin_id.as_uuid() != owner_a.as_uuid());
            })
        })
        .await;
}
