#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{
    bytes, call, json, migrated_database, production_router, request, seed_account,
};
use axum::http::{Method, StatusCode};
use ferl_server::domain::text::ComparisonText;
use serde_json::Value;
use uuid::Uuid;

const FIXTURES: &[(u128, &str, Option<&str>, i64, &str)] = &[
    (10, "page-a", Some("Beta"), 0, "2026-04-02T00:00:00Z"),
    (11, "page-b", Some("Alpha"), 4, "2026-04-02T00:00:00Z"),
    (12, "page-c", None, 0, "2026-04-02T00:00:00Z"),
    (13, "page-d", Some("ALPHA"), 4, "2026-04-02T00:00:00Z"),
    (14, "page-e", Some("Éclair"), 1, "2026-04-01T00:00:00Z"),
    (15, "page-f", None, 4, "2026-04-01T00:00:00Z"),
];

async fn page(app: &axum::Router, key: &str, query: &str) -> axum::response::Response {
    call(
        app.clone(),
        request(
            Method::GET,
            &format!("/api/v1/links?{query}"),
            Some(key),
            "",
        ),
    )
    .await
}

fn codes(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["short_code"].as_str().unwrap().to_owned())
        .collect()
}

async fn seed_ordered_links(pool: &sqlx::PgPool, owner: Uuid) {
    for (id, code, title, count, created_at) in FIXTURES {
        let destination = format!("https://page.example.test/{code}");
        let destination_search = ComparisonText::from_persisted(&destination);
        let title_search = title.map(ComparisonText::from_persisted);
        let created_at = chrono::DateTime::parse_from_rfc3339(created_at).unwrap();
        sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
            .bind(code)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO links \
             (id, short_code, owner_id, destination_url, destination_search, title, \
              title_search, enabled, visit_count, management_token_digest, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, true, $8, NULL, $9)",
        )
        .bind(Uuid::from_u128(*id))
        .bind(code)
        .bind(owner)
        .bind(&destination)
        .bind(destination_search.as_str())
        .bind(title)
        .bind(title_search.as_ref().map(ComparisonText::as_str))
        .bind(count)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'shared')")
            .bind(Uuid::from_u128(*id))
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn create(app: &axum::Router, key: &str, code: &str) {
    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &format!(
                r#"{{"destination_url":"https://{code}.authority.example/","short_code":"{code}"}}"#
            ),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let _ = json(response).await;
}

#[tokio::test]
async fn all_sorts_directions_have_golden_keyset_order_and_stable_stateless_continuations() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (owner, key) = seed_account(database.pool(), "page_owner", "user", true).await;
                seed_ordered_links(database.pool(), owner.as_uuid()).await;
                let app = production_router(database).await;
                let expected = [
                    (
                        "created_at",
                        "asc",
                        ["page-e", "page-f", "page-a", "page-b", "page-c", "page-d"],
                    ),
                    (
                        "created_at",
                        "desc",
                        ["page-d", "page-c", "page-b", "page-a", "page-f", "page-e"],
                    ),
                    (
                        "title",
                        "asc",
                        ["page-b", "page-d", "page-a", "page-e", "page-c", "page-f"],
                    ),
                    (
                        "title",
                        "desc",
                        ["page-e", "page-a", "page-d", "page-b", "page-f", "page-c"],
                    ),
                    (
                        "visit_count",
                        "asc",
                        ["page-a", "page-c", "page-e", "page-b", "page-d", "page-f"],
                    ),
                    (
                        "visit_count",
                        "desc",
                        ["page-f", "page-d", "page-b", "page-e", "page-c", "page-a"],
                    ),
                ];
                for (sort, order, golden) in expected {
                    let mut query = format!("sort={sort}&order={order}&limit=2");
                    let mut actual = Vec::new();
                    loop {
                        let response = page(&app, &key, &query).await;
                        if response.status() != StatusCode::OK {
                            let status = response.status();
                            let body = json(response).await;
                            panic!("{sort} {order} {query} returned {status}: {body}");
                        }
                        let page_body = json(response).await;
                        actual.extend(codes(&page_body));
                        let Some(cursor) = page_body["next_cursor"].as_str() else {
                            break;
                        };
                        query = format!("sort={sort}&order={order}&limit=1&cursor={cursor}");
                    }
                    assert_eq!(
                        actual,
                        golden.map(str::to_owned),
                        "independent golden ordering for {sort} {order}"
                    );
                }

                let first = page(&app, &key, "sort=created_at&order=desc&limit=2").await;
                let first = json(first).await;
                assert_eq!(codes(&first), ["page-d", "page-c"]);
                let cursor = first["next_cursor"].as_str().unwrap().to_owned();
                let deleted = call(
                    app.clone(),
                    request(Method::DELETE, "/api/v1/links/page-c", Some(&key), ""),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert!(bytes(deleted).await.is_empty());
                let continued = page(
                    &app,
                    &key,
                    &format!("sort=created_at&order=desc&limit=2&cursor={cursor}"),
                )
                .await;
                assert_eq!(continued.status(), StatusCode::OK);
                let continued = json(continued).await;
                assert_eq!(codes(&continued), ["page-b", "page-a"]);
                let next = continued["next_cursor"].as_str().unwrap();
                let final_page = page(
                    &app,
                    &key,
                    &format!("sort=created_at&order=desc&limit=2&cursor={next}"),
                )
                .await;
                let final_page = json(final_page).await;
                assert_eq!(codes(&final_page), ["page-f", "page-e"]);
                assert_eq!(final_page["next_cursor"], Value::Null);

                let boundary = page(&app, &key, "sort=title&limit=1").await;
                let boundary = json(boundary).await;
                let cursor = boundary["next_cursor"].as_str().unwrap().to_owned();
                let changed_context = page(
                    &app,
                    &key,
                    &format!("sort=title&order=asc&enabled=true&cursor={cursor}"),
                )
                .await;
                assert_eq!(changed_context.status(), StatusCode::BAD_REQUEST);
                assert_eq!(
                    json(changed_context).await["error"]["code"],
                    "invalid_request"
                );
                let mut tampered = cursor.clone().into_bytes();
                let last = tampered.len() - 1;
                tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
                let tampered = String::from_utf8(tampered).unwrap();
                let tampered_response =
                    page(&app, &key, &format!("sort=title&limit=1&cursor={tampered}")).await;
                assert_eq!(tampered_response.status(), StatusCode::BAD_REQUEST);
                let cross_resource = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/tags?cursor={cursor}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(cross_resource.status(), StatusCode::BAD_REQUEST);

                let rotated = call(
                    app.clone(),
                    request(Method::POST, "/api/v1/me/api-key", Some(&key), ""),
                )
                .await;
                assert_eq!(rotated.status(), StatusCode::OK);
                let new_key = json(rotated).await["api_key"].as_str().unwrap().to_owned();
                let old_key =
                    page(&app, &key, &format!("sort=title&limit=1&cursor={cursor}")).await;
                assert_eq!(old_key.status(), StatusCode::UNAUTHORIZED);
                let rotated_continuation = page(
                    &app,
                    &new_key,
                    &format!("sort=title&limit=1&cursor={cursor}"),
                )
                .await;
                assert_eq!(rotated_continuation.status(), StatusCode::OK);

                let empty = page(&app, &key, "q=does-not-exist&limit=1").await;
                assert_eq!(empty.status(), StatusCode::UNAUTHORIZED);
                let empty = page(&app, &new_key, "q=does-not-exist&limit=1").await;
                assert_eq!(empty.status(), StatusCode::OK);
                assert_eq!(
                    json(empty).await,
                    serde_json::json!({"items": [], "next_cursor": null})
                );
            })
        })
        .await;
}

#[tokio::test]
async fn each_page_renews_role_and_account_identity_before_decoding_its_cursor() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (admin_id, admin_key) =
                    seed_account(database.pool(), "cursor_admin", "admin", true).await;
                let (_, remaining_admin_key) =
                    seed_account(database.pool(), "cursor_admin_two", "admin", true).await;
                let (recreated_id, old_user_key) =
                    seed_account(database.pool(), "cursor_recreated", "user", true).await;
                let app = production_router(database).await;
                create(&app, &admin_key, "admin-cursor-link").await;
                create(&app, &old_user_key, "recreated-cursor-a").await;
                create(&app, &old_user_key, "recreated-cursor-b").await;

                let admin_page = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links?scope=all&limit=1",
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(admin_page.status(), StatusCode::OK);
                let admin_cursor = json(admin_page).await["next_cursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let demoted = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", admin_id.as_uuid()),
                        Some(&remaining_admin_key),
                        r#"{"role":"user"}"#,
                    ),
                )
                .await;
                assert_eq!(demoted.status(), StatusCode::OK);
                let _ = json(demoted).await;
                let forbidden = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links?scope=all&limit=1&cursor={admin_cursor}"),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
                assert_eq!(json(forbidden).await["error"]["code"], "forbidden");

                let user_page = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links?limit=1",
                        Some(&old_user_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(user_page.status(), StatusCode::OK);
                let user_cursor = json(user_page).await["next_cursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let deleted = call(
                    app.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", recreated_id.as_uuid()),
                        Some(&remaining_admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert!(bytes(deleted).await.is_empty());
                let replacement = call(
                    app.clone(),
                    request(
                        Method::POST,
                        "/api/v1/admin/accounts",
                        Some(&remaining_admin_key),
                        r#"{"username":"cursor_recreated","role":"user"}"#,
                    ),
                )
                .await;
                assert_eq!(replacement.status(), StatusCode::CREATED);
                let replacement = json(replacement).await;
                let new_id =
                    Uuid::parse_str(replacement["account"]["id"].as_str().unwrap()).unwrap();
                let new_key = replacement["api_key"].as_str().unwrap().to_owned();
                assert_ne!(new_id, recreated_id.as_uuid());
                let old_credential = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links", Some(&old_user_key), ""),
                )
                .await;
                assert_eq!(old_credential.status(), StatusCode::UNAUTHORIZED);
                let inherited_cursor = call(
                    app,
                    request(
                        Method::GET,
                        &format!("/api/v1/links?limit=1&cursor={user_cursor}"),
                        Some(&new_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(inherited_cursor.status(), StatusCode::BAD_REQUEST);
                assert_eq!(
                    json(inherited_cursor).await["error"]["code"],
                    "invalid_request"
                );
            })
        })
        .await;
}
