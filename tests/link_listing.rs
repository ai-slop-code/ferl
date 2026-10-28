#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{
    bytes, call, json, migrated_database, production_router_with, request, seed_account,
    seed_anonymous_link,
};
use axum::http::{Method, StatusCode};
use ferl_server::domain::text::ComparisonText;
use uuid::Uuid;

async fn create_link(app: &axum::Router, key: &str, code: &str, title: &str, tags: &[&str]) {
    let tags = serde_json::to_string(tags).unwrap();
    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &format!(
                r#"{{"destination_url":"https://shared.example.test/{code}","short_code":"{code}","title":"{title}","tags":{tags}}}"#
            ),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = json(response).await;
    assert_eq!(created["link"]["short_code"], code);
}

#[tokio::test]
async fn listing_uses_fresh_account_authority_and_complete_visible_link_snapshots() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (admin_id, admin_key) =
                    seed_account(database.pool(), "listing_admin", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "listing_owner", "user", true).await;
                let (other_id, suspended_key) =
                    seed_account(database.pool(), "listing_suspended", "user", false).await;
                let app = production_router_with(database, &[("FERL_DB_POOL_MAX", "1")]).await;
                create_link(&app, &owner_key, "owner-alpha", "Alpha", &["Work", "rust"]).await;
                create_link(&app, &owner_key, "owner-beta", "Beta", &["work"]).await;
                create_link(&app, &admin_key, "admin-alpha", "Admin", &["admin"]).await;
                let disabled = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/owner-beta",
                        Some(&owner_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;

                let (anonymous_id, anonymous_token) =
                    seed_anonymous_link(database.pool(), "anonymous-listing").await;
                let suspended_code = "suspended-listing";
                let suspended_destination = "https://suspended.example.test/";
                let suspended_search = ComparisonText::from_persisted(suspended_destination);
                sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
                    .bind(suspended_code)
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query(
                    "INSERT INTO links \
                     (id, short_code, owner_id, destination_url, destination_search, title, \
                      title_search, enabled, visit_count, management_token_digest) \
                     VALUES ($1, $2, $3, $4, $5, 'Suspended', 'suspended', true, 0, NULL)",
                )
                .bind(Uuid::new_v4())
                .bind(suspended_code)
                .bind(other_id.as_uuid())
                .bind(suspended_destination)
                .bind(suspended_search.as_str())
                .execute(database.pool())
                .await
                .unwrap();

                let before_charges: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                for path in ["/api/v1/links", "/api/v1/tags"] {
                    let disabled_credential = call(
                        app.clone(),
                        request(Method::GET, path, Some(&suspended_key), ""),
                    )
                    .await;
                    assert_eq!(disabled_credential.status(), StatusCode::UNAUTHORIZED);
                    assert_eq!(disabled_credential.headers()["www-authenticate"], "Bearer");
                    assert_eq!(
                        json(disabled_credential).await["error"]["code"],
                        "invalid_credentials"
                    );
                }
                let own = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links", Some(&owner_key), ""),
                )
                .await;
                assert_eq!(own.status(), StatusCode::OK);
                let own = json(own).await;
                assert_eq!(own["next_cursor"], serde_json::Value::Null);
                let own_codes = own["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|link| link["short_code"].as_str().unwrap())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(own_codes, ["owner-alpha", "owner-beta"].into());
                for item in own["items"].as_array().unwrap() {
                    for field in [
                        "id",
                        "short_code",
                        "short_url",
                        "destination_url",
                        "title",
                        "tags",
                        "owner_id",
                        "enabled",
                        "active",
                        "visit_count",
                        "created_at",
                        "updated_at",
                    ] {
                        assert!(item.get(field).is_some(), "Link DTO contains {field}");
                    }
                    assert_eq!(item["owner_id"], owner_id.as_uuid().to_string());
                }
                let disabled_item = own["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["short_code"] == "owner-beta")
                    .unwrap();
                assert_eq!(disabled_item["enabled"], false);
                assert_eq!(disabled_item["active"], false);

                let admin_own = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links?scope=own", Some(&admin_key), ""),
                )
                .await;
                assert_eq!(admin_own.status(), StatusCode::OK);
                assert_eq!(
                    json(admin_own).await["items"][0]["short_code"],
                    "admin-alpha"
                );

                let admin_all = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links?scope=all", Some(&admin_key), ""),
                )
                .await;
                assert_eq!(admin_all.status(), StatusCode::OK);
                let admin_all = json(admin_all).await;
                assert_eq!(admin_all["items"].as_array().unwrap().len(), 5);
                let suspended = admin_all["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["short_code"] == suspended_code)
                    .unwrap();
                assert_eq!(suspended["enabled"], true);
                assert_eq!(suspended["active"], false);

                let no_cross_account = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links?scope=all", Some(&owner_key), ""),
                )
                .await;
                assert_eq!(no_cross_account.status(), StatusCode::FORBIDDEN);
                assert_eq!(json(no_cross_account).await["error"]["code"], "forbidden");

                let anonymous = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links?scope=all&owner_id=anonymous",
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(anonymous.status(), StatusCode::OK);
                assert_eq!(
                    json(anonymous).await["items"][0]["id"],
                    anonymous_id.as_uuid().to_string()
                );

                let filtered_owner = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links?scope=all&owner_id={}", owner_id.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(filtered_owner.status(), StatusCode::OK);
                assert_eq!(
                    json(filtered_owner).await["items"]
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );

                let alpha_created: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
                    "SELECT created_at FROM links WHERE short_code = 'owner-alpha'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                let from = (alpha_created - chrono::Duration::seconds(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
                let to = (alpha_created + chrono::Duration::seconds(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
                let combined = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!(
                            "/api/v1/links?scope=all&owner_id={}&q=Alpha&enabled=true&created_from={from}&created_to={to}&tag=Work&tag=rust&tag_mode=all&sort=created_at&order=asc",
                            owner_id.as_uuid()
                        ),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(combined.status(), StatusCode::OK);
                assert_eq!(
                    json(combined).await["items"][0]["short_code"],
                    "owner-alpha"
                );

                let missing_owner = Uuid::from_u128(0x1234).to_string();
                let empty = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links?scope=all&owner_id={missing_owner}"),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(empty.status(), StatusCode::OK);
                assert_eq!(
                    json(empty).await,
                    serde_json::json!({"items": [], "next_cursor": null})
                );

                for (key, path, status, code) in [
                    (
                        Some(owner_key.as_str()),
                        "/api/v1/links?owner_id=%GG",
                        StatusCode::FORBIDDEN,
                        "forbidden",
                    ),
                    (
                        Some(owner_key.as_str()),
                        "/api/v1/links?scope=all&owner_id=%GG",
                        StatusCode::FORBIDDEN,
                        "forbidden",
                    ),
                    (
                        Some(admin_key.as_str()),
                        "/api/v1/links?owner_id=malformed",
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                    ),
                    (
                        Some(admin_key.as_str()),
                        "/api/v1/links?scope=all&owner_id=malformed",
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                    ),
                    (
                        Some(admin_key.as_str()),
                        "/api/v1/links?scope=unknown&owner_id=malformed",
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                    ),
                    (
                        Some(anonymous_token.as_str()),
                        "/api/v1/links?q=%GG",
                        StatusCode::FORBIDDEN,
                        "forbidden",
                    ),
                ] {
                    let response = call(app.clone(), request(Method::GET, path, key, "")).await;
                    assert_eq!(response.status(), status, "{path}");
                    assert_eq!(json(response).await["error"]["code"], code, "{path}");
                }

                let deleted_owner = call(
                    app.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", other_id.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_owner.status(), StatusCode::NO_CONTENT);
                assert!(bytes(deleted_owner).await.is_empty());
                let empty_deleted_owner = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links?scope=all&owner_id={}", other_id.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(empty_deleted_owner.status(), StatusCode::OK);
                assert_eq!(
                    json(empty_deleted_owner).await,
                    serde_json::json!({"items": [], "next_cursor": null})
                );

                let missing_credentials = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links?q=%GG", None, ""),
                )
                .await;
                assert_eq!(missing_credentials.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(missing_credentials.headers()["www-authenticate"], "Bearer");
                assert_eq!(
                    json(missing_credentials).await["error"]["code"],
                    "authentication_required"
                );
                let invalid_credentials = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/links", Some("not-a-valid-key"), ""),
                )
                .await;
                assert_eq!(invalid_credentials.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(invalid_credentials.headers()["www-authenticate"], "Bearer");
                assert_eq!(
                    json(invalid_credentials).await["error"]["code"],
                    "invalid_credentials"
                );

                let head = call(
                    app.clone(),
                    request(Method::HEAD, "/api/v1/links", None, ""),
                )
                .await;
                assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(head.headers()["allow"], "GET, POST");
                assert!(bytes(head).await.is_empty());
                let tag_head =
                    call(app.clone(), request(Method::HEAD, "/api/v1/tags", None, "")).await;
                assert_eq!(tag_head.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(tag_head.headers()["allow"], "GET");
                assert!(bytes(tag_head).await.is_empty());

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_charges,
                    "discovery does not charge link creation allowance"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "listing and discovery requests do not create visits"
                );
                assert_ne!(admin_id.as_uuid(), owner_id.as_uuid());
            })
        })
        .await;
}
