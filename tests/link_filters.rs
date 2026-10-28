#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{call, json, migrated_database, production_router, request, seed_account};
use axum::http::{Method, StatusCode};
use serde_json::json as json_value;

async fn create(
    app: &axum::Router,
    key: &str,
    code: &str,
    destination: &str,
    title: Option<&str>,
    tags: &[&str],
) {
    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &serde_json::to_string(&json_value!({
                "destination_url": destination,
                "short_code": code,
                "title": title,
                "tags": tags,
            }))
            .unwrap(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let _ = json(response).await;
}

fn codes(page: &serde_json::Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["short_code"].as_str().unwrap().to_owned())
        .collect()
}

async fn get(app: &axum::Router, key: &str, query: &str) -> axum::response::Response {
    let separator = if query.is_empty() { "?" } else { "&" };
    call(
        app.clone(),
        request(
            Method::GET,
            &format!("/api/v1/links{query}{separator}sort=created_at&order=asc"),
            Some(key),
            "",
        ),
    )
    .await
}

#[tokio::test]
async fn filters_are_literal_canonical_and_composed_before_distinct_keyset_selection() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) = seed_account(database.pool(), "filter_owner", "user", true).await;
                let app = production_router(database).await;
                create(
                    &app,
                    &key,
                    "filter-alpha",
                    "https://literal.example.test/",
                    Some("Straße %_\\ a&b a,b"),
                    &["Work", "rust", "a,b", "a&b"],
                )
                .await;
                create(
                    &app,
                    &key,
                    "filter-beta",
                    "https://second.example.test/",
                    Some("STRASSE beta"),
                    &["work", "java"],
                )
                .await;
                create(
                    &app,
                    &key,
                    "filter-gamma",
                    "https://third.example.test/",
                    None,
                    &["rust"],
                )
                .await;
                create(
                    &app,
                    &key,
                    "filter-delta",
                    "https://fourth.example.test/",
                    Some("Cafe\u{301}"),
                    &["novel"],
                )
                .await;
                let disabled = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/filter-beta",
                        Some(&key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;
                for (code, created_at) in [
                    ("filter-alpha", "2026-04-01T00:00:00Z"),
                    ("filter-beta", "2026-04-02T00:00:00Z"),
                    ("filter-gamma", "2026-04-03T00:00:00Z"),
                    ("filter-delta", "2026-04-04T00:00:00Z"),
                ] {
                    sqlx::query("UPDATE links SET created_at = $2 WHERE short_code = $1")
                        .bind(code)
                        .bind(chrono::DateTime::parse_from_rfc3339(created_at).unwrap())
                        .execute(database.pool())
                        .await
                        .unwrap();
                }

                for (query, expected) in [
                    ("?q=STRASSE", ["filter-alpha", "filter-beta"].as_slice()),
                    ("?q=filter-beta", ["filter-beta"].as_slice()),
                    ("?q=literal.example.test", ["filter-alpha"].as_slice()),
                    ("?q=%25", ["filter-alpha"].as_slice()),
                    ("?q=%5F", ["filter-alpha"].as_slice()),
                    ("?q=%5C", ["filter-alpha"].as_slice()),
                    ("?q=%26", ["filter-alpha"].as_slice()),
                    ("?q=%2C", ["filter-alpha"].as_slice()),
                    ("?q=%C3%A9", ["filter-delta"].as_slice()),
                    ("?q=E%CC%81", ["filter-delta"].as_slice()),
                    ("?enabled=false", ["filter-beta"].as_slice()),
                    (
                        "?created_from=2026-04-02T00:00:00Z&created_to=2026-04-03T00:00:00Z",
                        ["filter-beta"].as_slice(),
                    ),
                    (
                        "?created_from=2026-04-01T00:00:00Z&created_to=2026-04-02T00:00:00Z",
                        ["filter-alpha"].as_slice(),
                    ),
                    ("?tag=Work&tag=rust", ["filter-alpha", "filter-beta", "filter-gamma"].as_slice()),
                    (
                        "?tag=Work&tag=rust&tag_mode=all",
                        ["filter-alpha"].as_slice(),
                    ),
                    (
                        "?tag=Work&tag=work&tag=rust&tag_mode=all",
                        ["filter-alpha"].as_slice(),
                    ),
                    ("?tag=a%2Cb", ["filter-alpha"].as_slice()),
                    ("?tag=a,b", ["filter-alpha"].as_slice()),
                    ("?tag=a%26b", ["filter-alpha"].as_slice()),
                    (
                        "?tag=Work&tag=unknown&tag_mode=any",
                        ["filter-alpha", "filter-beta"].as_slice(),
                    ),
                    ("?tag=Work&tag=unknown&tag_mode=all", [].as_slice()),
                    ("?tag=unknown", [].as_slice()),
                    (
                        "?q=STRASSE&enabled=true&created_from=2026-04-01T00:00:00Z&created_to=2026-04-02T00:00:00Z&tag=WORK&tag=rust&tag_mode=all",
                        ["filter-alpha"].as_slice(),
                    ),
                ] {
                    let response = get(&app, &key, query).await;
                    assert_eq!(response.status(), StatusCode::OK, "{query}");
                    let result = json(response).await;
                    assert_eq!(codes(&result), expected, "{query}");
                    assert_eq!(result["next_cursor"], serde_json::Value::Null, "{query}");
                }

                let twenty = std::iter::repeat_n("tag=work", 20)
                    .collect::<Vec<_>>()
                    .join("&");
                let accepted = get(&app, &key, &format!("?{twenty}")).await;
                assert_eq!(accepted.status(), StatusCode::OK);
                assert_eq!(codes(&json(accepted).await), ["filter-alpha", "filter-beta"]);
                let twenty_one = std::iter::repeat_n("tag=work", 21)
                    .collect::<Vec<_>>()
                    .join("&");
                let rejected = get(&app, &key, &format!("?{twenty_one}")).await;
                assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
                assert_eq!(json(rejected).await["error"]["code"], "invalid_request");

                for query in [
                    "?tag_mode=all",
                    "?enabled=true&enabled=false",
                    "?unknown=value",
                    "?created_from=2026-04-03T00:00:00Z&created_to=2026-04-02T00:00:00Z",
                    "?tag_mode=invalid&tag=work",
                    "?q=%GG",
                    "?q=%FF",
                    "?q=a&&tag=work",
                ] {
                    let response = get(&app, &key, query).await;
                    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
                    assert_eq!(json(response).await["error"]["code"], "invalid_request");
                }
            })
        })
        .await;
}
