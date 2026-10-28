#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi;

use account_support::{
    bytes, call, json, migrated_database, production_router_with, request, seed_account,
};
use axum::{
    extract::ConnectInfo,
    http::{Method, StatusCode},
};
use serde_json::json as json_value;
use std::net::SocketAddr;

fn anonymous_request(body: &str) -> axum::http::Request<axum::body::Body> {
    let mut request = request(Method::POST, "/api/v1/links", None, body);
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 60], 4567))));
    request
        .headers_mut()
        .insert(http::header::HOST, "attacker-host.example".parse().unwrap());
    request.headers_mut().insert(
        "x-forwarded-host",
        "attacker-forwarded-host.example".parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn production_create_routes_return_complete_owned_and_anonymous_representations() {
    let database = migrated_database().await;
    database.verify(|database| Box::pin(async move {
        let (_, admin_key) = seed_account(database.pool(), "link_create_admin", "admin", true).await;
        let application = production_router_with(
            database,
            &[
                ("FERL_PUBLIC_BASE_URL", "https://configured.example"),
                ("FERL_ANONYMOUS_CREATION", "true"),
                ("FERL_DB_POOL_MAX", "1"),
            ],
        )
        .await;

        let anonymous = call(
            application.clone(),
            anonymous_request(
                r#"{"destination_url":"HTTPS://bücher.example/a/é?q=2&q=1#intro","title":"  Unicode docs  ","tags":["Work","work"," É ","E\u0301"]}"#,
            ),
        )
        .await;
        assert_eq!(anonymous.status(), StatusCode::CREATED);
        let anonymous_headers = anonymous.headers().clone();
        let anonymous_body = json(anonymous).await;
        let anonymous_link = &anonymous_body["link"];
        assert!(anonymous_link["short_url"]
            .as_str()
            .unwrap()
            .starts_with("https://configured.example/"));
        assert_eq!(anonymous_link["destination_url"], "https://xn--bcher-kva.example/a/%C3%A9?q=2&q=1#intro");
        assert_eq!(anonymous_link["title"], "Unicode docs");
        assert_eq!(anonymous_link["tags"], json_value!(["work", "é"]));
        assert_eq!(anonymous_link["owner_id"], serde_json::Value::Null);
        assert_eq!(anonymous_link["enabled"], true);
        assert_eq!(anonymous_link["active"], true);
        assert_eq!(anonymous_link["visit_count"], 0);
        let created_at = anonymous_link["created_at"].as_str().unwrap();
        let updated_at = anonymous_link["updated_at"].as_str().unwrap();
        assert!(created_at.ends_with('Z'));
        assert!(updated_at.ends_with('Z'));
        assert!(created_at <= updated_at);
        let token = anonymous_body["management_token"].as_str().unwrap();
        assert!(token.starts_with("ferl_mt_v1_"));
        assert!(openapi::validates("LinkCreation", &anonymous_body));
        assert!(openapi::validates_response("LinkCreated", &anonymous_headers, Some(&anonymous_body), false));
        assert_eq!(anonymous_link.as_object().unwrap().len(), 12);

        let owned = call(
            application.clone(),
            request(
                Method::POST,
                "/api/v1/links",
                Some(&admin_key),
                r#"{"destination_url":"https://same.example/path?q=1#part","short_code":"api-sale","title":null,"tags":[]}"#,
            ),
        )
        .await;
        assert_eq!(owned.status(), StatusCode::CREATED);
        let owned_headers = owned.headers().clone();
        let owned_body = json(owned).await;
        assert!(owned_body.get("management_token").is_none());
        let owned_link = &owned_body["link"];
        assert_eq!(owned_link["short_code"], "api-sale");
        assert_eq!(owned_link["short_url"], "https://configured.example/api-sale");
        assert_eq!(owned_link["destination_url"], "https://same.example/path?q=1#part");
        assert_eq!(owned_link["title"], serde_json::Value::Null);
        assert_eq!(owned_link["tags"], json_value!([]));
        assert_eq!(owned_link["owner_id"], admin_key_owner(database.pool(), &admin_key).await);
        assert_eq!(owned_link["enabled"], true);
        assert_eq!(owned_link["active"], true);
        assert_eq!(owned_link["visit_count"], 0);
        assert!(openapi::validates("LinkCreation", &owned_body));
        assert!(openapi::validates_response("LinkCreated", &owned_headers, Some(&owned_body), false));
        let mut owned_with_secret = owned_body.clone();
        owned_with_secret["management_token"] =
            serde_json::Value::String("invalid-owned-secret".to_owned());
        assert!(!openapi::validates("LinkCreation", &owned_with_secret));
        let mut anonymous_without_secret = anonymous_body.clone();
        anonymous_without_secret
            .as_object_mut()
            .unwrap()
            .remove("management_token");
        assert!(!openapi::validates("LinkCreation", &anonymous_without_secret));

        let repeated = call(
            application.clone(),
            request(
                Method::POST,
                "/api/v1/links",
                Some(&admin_key),
                r#"{"destination_url":"https://same.example/path?q=1#part"}"#,
            ),
        )
        .await;
        assert_eq!(repeated.status(), StatusCode::CREATED);
        let repeated_body = json(repeated).await;
        assert_ne!(owned_link["id"], repeated_body["link"]["id"]);
        assert_ne!(owned_link["short_code"], repeated_body["link"]["short_code"]);
        assert_eq!(repeated_body["link"]["destination_url"], owned_link["destination_url"]);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links").fetch_one(database.pool()).await.unwrap(), 3);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code <> 'api'").fetch_one(database.pool()).await.unwrap(), 3);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags").fetch_one(database.pool()).await.unwrap(), 2);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges").fetch_one(database.pool()).await.unwrap(), 3);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_buckets WHERE bucket_kind = 'account'").fetch_one(database.pool()).await.unwrap(), 1);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_buckets WHERE bucket_kind = 'anonymous_ip'").fetch_one(database.pool()).await.unwrap(), 1);
        drop(application);
    })).await;
}

async fn admin_key_owner(pool: &sqlx::PgPool, key: &str) -> String {
    let digest = ferl_server::domain::secret::SecretDigest::from_presented_secret(key);
    sqlx::query_scalar("SELECT id::text FROM accounts WHERE api_key_digest = $1")
        .bind(digest.as_bytes().as_slice())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn one_connection_pool_composes_allowance_and_new_creation_transaction() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "one_pool_creator", "user", true).await;
                let application =
                    production_router_with(database, &[("FERL_DB_POOL_MAX", "1")]).await;
                let response = call(
                    application,
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://one-pool.example/"}"#,
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::CREATED);
                let body = json(response).await;
                assert!(body.get("management_token").is_none());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
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
async fn permanent_code_namespace_reserves_api_but_not_asset_or_health_root_names() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "code_namespace_owner", "user", true).await;
                let application = production_router_with(database, &[]).await;

                let reserved = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://api-reserved.example/","short_code":"api"}"#,
                    ),
                )
                .await;
                assert_eq!(reserved.status(), StatusCode::CONFLICT);
                assert_eq!(
                    json(reserved).await["error"]["code"],
                    "short_code_unavailable"
                );

                for code in ["assets", "health", "api-sale"] {
                    let response = call(
                application.clone(),
                request(
                    Method::POST,
                    "/api/v1/links",
                    Some(&key),
                    &format!(
                        r#"{{"destination_url":"https://{code}.example/","short_code":"{code}"}}"#
                    ),
                ),
            )
            .await;
                    assert_eq!(response.status(), StatusCode::CREATED, "custom code {code}");
                    assert_eq!(json(response).await["link"]["short_code"], code);
                }

                for (code, destination) in [
                    ("assets", "https://assets.example/"),
                    ("health", "https://health.example/"),
                    ("api-sale", "https://api-sale.example/"),
                ] {
                    let followed = call(
                        application.clone(),
                        request(Method::GET, &format!("/{code}"), None, ""),
                    )
                    .await;
                    assert_eq!(
                        followed.status(),
                        StatusCode::FOUND,
                        "{code} is a public code"
                    );
                    assert_eq!(followed.headers()[http::header::LOCATION], destination);
                    assert!(bytes(followed).await.is_empty());
                }

                let deleted = call(
                    application.clone(),
                    request(Method::DELETE, "/api/v1/links/assets", Some(&key), ""),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert!(bytes(deleted).await.is_empty());
                let reused = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://different.example/","short_code":"assets"}"#,
                    ),
                )
                .await;
                assert_eq!(reused.status(), StatusCode::CONFLICT);
                assert_eq!(
                    json(reused).await["error"]["code"],
                    "short_code_unavailable"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    4,
                    "deleted assets reservation remains alongside the other two codes and api"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    5,
                    "the reserved, three create, and reused-code attempts charge; DELETE does not"
                );
            })
        })
        .await;
}
