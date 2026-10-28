#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{call, json, migrated_database, production_router, request, seed_account};
use axum::http::{Method, StatusCode};

#[tokio::test]
async fn maximum_legal_destination_title_and_comparison_tags_are_insertable_and_listable() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "maximum_discovery", "user", true).await;
                let app = production_router(database).await;
                let destination = format!("https://x/{}", "a".repeat(8_182));
                assert_eq!(destination.len(), 8_192);
                let title = "ß".repeat(200);
                let maximal_tag = "é".repeat(64);
                let trimmed_tag = format!("{}x{}", "\u{2003}".repeat(127), "\u{a0}".repeat(128));
                assert_eq!(trimmed_tag.chars().count(), 256);
                let body = serde_json::to_string(&serde_json::json!({
                    "destination_url": destination,
                    "short_code": "maximum-values",
                    "title": title,
                    "tags": [maximal_tag, trimmed_tag],
                }))
                .unwrap();
                let created = call(
                    app.clone(),
                    request(Method::POST, "/api/v1/links", Some(&key), &body),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let created = json(created).await;
                assert_eq!(
                    created["link"]["destination_url"].as_str().unwrap().len(),
                    8_192
                );
                assert_eq!(created["link"]["title"], title);
                assert_eq!(created["link"]["tags"][0], "x");
                assert_eq!(created["link"]["tags"][1], maximal_tag);

                let listed = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links?sort=title&limit=1",
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(listed.status(), StatusCode::OK);
                let listed = json(listed).await;
                assert_eq!(listed["items"].as_array().unwrap().len(), 1);
                assert_eq!(listed["items"][0]["short_code"], "maximum-values");
                assert_eq!(listed["items"][0]["title"], title);
                assert_eq!(
                    listed["items"][0]["tags"],
                    serde_json::json!(["x", maximal_tag])
                );

                let tags = call(
                    app.clone(),
                    request(Method::GET, "/api/v1/tags", Some(&key), ""),
                )
                .await;
                assert_eq!(tags.status(), StatusCode::OK);
                let tags = json(tags).await;
                assert_eq!(tags["items"].as_array().unwrap().len(), 2);
                assert_eq!(tags["items"][0]["name"], "x");
                assert_eq!(tags["items"][1]["name"], maximal_tag);
            })
        })
        .await;
}
