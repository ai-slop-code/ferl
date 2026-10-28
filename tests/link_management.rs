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
use axum::http::{Method, StatusCode};
use serde_json::json as json_value;
use std::time::Duration;
use tokio::{sync::Barrier, time::timeout};
use uuid::Uuid;

#[tokio::test]
async fn patch_detail_noops_and_delete_are_atomic_and_keep_permanent_reservations() {
    let database = migrated_database().await;
    database.verify(|database| Box::pin(async move {
        let (_, owner_key) = seed_account(database.pool(), "management_owner", "user", true).await;
        let application = production_router_with(database, &[]).await;
        let created = call(
            application.clone(),
            request(
                Method::POST,
                "/api/v1/links",
                Some(&owner_key),
                r#"{"destination_url":"https://before.example/a?q=1#frag","short_code":"patch-link","title":"Initial title","tags":["Work"]}"#,
            ),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = json(created).await;
        let link_id = Uuid::parse_str(created["link"]["id"].as_str().unwrap()).unwrap();

        sqlx::query(
            "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, \
             referrer, user_agent, device, browser, os) \
             VALUES ($1, $2, 'https://before.example/a?q=1#frag', clock_timestamp(), 'GET', \
             '192.0.2.1', NULL, NULL, 'unknown', 'unknown', 'unknown')",
        )
        .bind(Uuid::new_v4())
        .bind(link_id)
        .execute(database.pool())
        .await
        .unwrap();

        let detail = call(
            application.clone(),
            request(Method::GET, "/api/v1/links/patch-link", Some(&owner_key), ""),
        )
        .await;
        assert_eq!(detail.status(), StatusCode::OK);
        let headers = detail.headers().clone();
        let detail = json(detail).await;
        assert!(openapi::validates("Link", &detail));
        assert!(openapi::validates_response("Link", &headers, Some(&detail), false));
        assert_eq!(detail["title"], "Initial title");
        assert_eq!(detail["tags"], json_value!(["work"]));
        assert_eq!(detail["visit_count"], 0);
        assert!(detail.get("management_token").is_none());

        let original_updated: String = sqlx::query_scalar(
            "SELECT updated_at::text FROM links WHERE id = $1",
        )
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        let noop = call(
            application.clone(),
            request(
                Method::PATCH,
                "/api/v1/links/patch-link",
                Some(&owner_key),
                r#"{"title":"  Initial title ","tags":["WORK","work"],"enabled":true}"#,
            ),
        )
        .await;
        assert_eq!(noop.status(), StatusCode::OK);
        let noop = json(noop).await;
        assert_eq!(noop["title"], "Initial title");
        assert_eq!(noop["tags"], json_value!(["work"]));
        let unchanged_updated: String = sqlx::query_scalar(
            "SELECT updated_at::text FROM links WHERE id = $1",
        )
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(original_updated, unchanged_updated, "canonical no-op preserves updated_at exactly");

        for invalid in [
            "{}",
            r#"{"owner_id":null}"#,
            r#"{"short_code":"new-code"}"#,
            r#"{"destination_url":null}"#,
            r#"{"enabled":null}"#,
            r#"{"tags":null}"#,
            r#"{"title":"valid","destination_url":null}"#,
        ] {
            let response = call(
                application.clone(),
                request(Method::PATCH, "/api/v1/links/patch-link", Some(&owner_key), invalid),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{invalid}");
            assert_eq!(json(response).await["error"]["code"], "invalid_request");
        }
        let after_invalid: (String, Option<String>, i64) = sqlx::query_as(
            "SELECT destination_url, title, visit_count FROM links WHERE id = $1",
        )
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(after_invalid.0, "https://before.example/a?q=1#frag");
        assert_eq!(after_invalid.1.as_deref(), Some("Initial title"));

        let changed = call(
            application.clone(),
            request(
                Method::PATCH,
                "/api/v1/links/patch-link",
                Some(&owner_key),
                r#"{"destination_url":"HTTPS://after.example/path?q=2#new","title":null,"tags":["Rust"," rust ","Zed"],"enabled":false}"#,
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let changed = json(changed).await;
        assert_eq!(changed["destination_url"], "https://after.example/path?q=2#new");
        assert_eq!(changed["title"], serde_json::Value::Null);
        assert_eq!(changed["tags"], json_value!(["rust", "zed"]));
        assert_eq!(changed["enabled"], false);
        assert_eq!(changed["active"], false);
        assert_eq!(changed["visit_count"], 0);

        let preserve = call(
            application.clone(),
            request(
                Method::PATCH,
                "/api/v1/links/patch-link",
                Some(&owner_key),
                r#"{"destination_url":"https://third.example/"}"#,
            ),
        )
        .await;
        assert_eq!(preserve.status(), StatusCode::OK);
        let preserve = json(preserve).await;
        assert_eq!(preserve["title"], serde_json::Value::Null);
        assert_eq!(preserve["tags"], json_value!(["rust", "zed"]));
        assert_eq!(preserve["enabled"], false);

        let deleted = call(
            application.clone(),
            request(Method::DELETE, "/api/v1/links/patch-link", Some(&owner_key), ""),
        )
        .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert!(bytes(deleted).await.is_empty());
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE id = $1").bind(link_id).fetch_one(database.pool()).await.unwrap(), 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags WHERE link_id = $1").bind(link_id).fetch_one(database.pool()).await.unwrap(), 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits WHERE link_id = $1").bind(link_id).fetch_one(database.pool()).await.unwrap(), 0);
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'patch-link'").fetch_one(database.pool()).await.unwrap(), 1);

        for method in [Method::GET, Method::DELETE] {
            let response = call(
                application.clone(),
                request(method, "/api/v1/links/patch-link", Some(&owner_key), ""),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(json(response).await["error"]["code"], "not_found");
        }
    })).await;
}

#[tokio::test]
async fn concurrent_tag_replacements_commit_complete_sets_without_union() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "tag_replace_owner", "user", true).await;
                let application = production_router_with(database, &[]).await;
                let response = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://tags.example/","short_code":"tag-race"}"#,
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::CREATED);
                let _ = json(response).await;

                let barrier = std::sync::Arc::new(Barrier::new(3));
                let mut tasks = tokio::task::JoinSet::new();
                for body in [
                    r#"{"tags":["alpha","beta"]}"#,
                    r#"{"tags":["gamma","delta"]}"#,
                ] {
                    let app = application.clone();
                    let key = key.clone();
                    let barrier = barrier.clone();
                    tasks.spawn(async move {
                        barrier.wait().await;
                        call(
                            app,
                            request(Method::PATCH, "/api/v1/links/tag-race", Some(&key), body),
                        )
                        .await
                    });
                }
                barrier.wait().await;
                for _ in 0..2 {
                    let response = timeout(Duration::from_secs(8), tasks.join_next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    let _ = json(response).await;
                }
                let names: Vec<String> =
                    sqlx::query_scalar("SELECT name FROM link_tags ORDER BY name COLLATE \"C\"")
                        .fetch_all(database.pool())
                        .await
                        .unwrap();
                assert!(
                    names == ["alpha", "beta"] || names == ["delta", "gamma"],
                    "committed replacement was one whole set: {names:?}"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2
                );
            })
        })
        .await;
}
