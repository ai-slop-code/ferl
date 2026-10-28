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
use axum::{body::Body, extract::ConnectInfo, http::Method, http::StatusCode};
use serde_json::Value;
use std::net::SocketAddr;

fn anonymous_create(body: &str) -> axum::http::Request<Body> {
    let mut request = request(Method::POST, "/api/v1/links", None, body);
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 73], 4567))));
    request
}

async fn error_object(response: axum::response::Response) -> Value {
    let value = json(response).await;
    value["error"].clone()
}

#[tokio::test]
async fn public_link_operations_apply_fresh_object_authority_and_conceal_other_objects() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "object_admin", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "object_owner", "user", true).await;
                let (_, unrelated_key) =
                    seed_account(database.pool(), "object_unrelated", "user", true).await;
                let application = production_router_with(
                    database,
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                )
                .await;

                for (key, code) in [(&owner_key, "owned-auth"), (&owner_key, "owned-delete")] {
                    let created = call(
                        application.clone(),
                        request(
                            Method::POST,
                            "/api/v1/links",
                            Some(key),
                            &format!(
                                r#"{{"destination_url":"https://{code}.example/","short_code":"{code}"}}"#
                            ),
                        ),
                    )
                    .await;
                    assert_eq!(created.status(), StatusCode::CREATED);
                    let _ = json(created).await;
                }
                let anonymous = call(
                    application.clone(),
                    anonymous_create(r#"{"destination_url":"https://anonymous-auth.example/"}"#),
                )
                .await;
                assert_eq!(anonymous.status(), StatusCode::CREATED);
                let anonymous_body = json(anonymous).await;
                let anonymous_code = anonymous_body["link"]["short_code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let anonymous_uri = format!("/api/v1/links/{anonymous_code}");
                let token = anonymous_body["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let second_anonymous = call(
                    application.clone(),
                    anonymous_create(r#"{"destination_url":"https://anonymous-other.example/"}"#),
                )
                .await;
                assert_eq!(second_anonymous.status(), StatusCode::CREATED);
                let second_anonymous_body = json(second_anonymous).await;
                let second_anonymous_code = second_anonymous_body["link"]["short_code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let second_anonymous_uri = format!("/api/v1/links/{second_anonymous_code}");
                let second_token = second_anonymous_body["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();

                let owner_read = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/links/owned-auth", Some(&owner_key), ""),
                )
                .await;
                assert_eq!(owner_read.status(), StatusCode::OK);
                assert!(json(owner_read).await.get("management_token").is_none());
                let admin_read = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/links/owned-auth", Some(&admin_key), ""),
                )
                .await;
                assert_eq!(admin_read.status(), StatusCode::OK);
                assert_eq!(
                    json(admin_read).await["owner_id"],
                    owner_id.as_uuid().to_string()
                );

                let wrong_account = call(
                    application.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/owned-auth",
                        Some(&unrelated_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(wrong_account.status(), StatusCode::NOT_FOUND);
                let unknown = call(
                    application.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/nonexistent-code",
                        Some(&unrelated_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
                assert_eq!(
                    error_object(wrong_account).await,
                    error_object(unknown).await,
                    "valid unrelated credentials receive the same concealed not_found"
                );
                for (credential, expected) in [
                    (None, "authentication_required"),
                    (Some("malformed-credential"), "invalid_credentials"),
                ] {
                    let response = call(
                        application.clone(),
                        request(Method::GET, "/api/v1/links/owned-auth", credential, ""),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                    assert_eq!(response.headers()["www-authenticate"], "Bearer");
                    assert_eq!(json(response).await["error"]["code"], expected);
                }

                for (credential, expected) in [
                    (&token, StatusCode::OK),
                    (&admin_key, StatusCode::OK),
                    (&owner_key, StatusCode::NOT_FOUND),
                    (&unrelated_key, StatusCode::NOT_FOUND),
                    (&second_token, StatusCode::NOT_FOUND),
                ] {
                    let response = call(
                        application.clone(),
                        request(
                            Method::GET,
                            &anonymous_uri,
                            Some(credential),
                            "",
                        ),
                    )
                    .await;
                    assert_eq!(response.status(), expected);
                    if expected == StatusCode::OK {
                        let body = json(response).await;
                        assert_eq!(body["owner_id"], Value::Null);
                        assert!(body.get("management_token").is_none());
                        assert!(body.get("management_token_digest").is_none());
                    } else {
                        assert_eq!(json(response).await["error"]["code"], "not_found");
                    }
                }

                for (credential, uri, expected) in [
                    (&owner_key, "/api/v1/links/owned-auth", StatusCode::OK),
                    (&admin_key, "/api/v1/links/owned-auth", StatusCode::OK),
                    (&unrelated_key, "/api/v1/links/owned-auth", StatusCode::NOT_FOUND),
                    (&token, "/api/v1/links/owned-auth", StatusCode::NOT_FOUND),
                    (&token, anonymous_uri.as_str(), StatusCode::OK),
                    (&second_token, anonymous_uri.as_str(), StatusCode::NOT_FOUND),
                    (
                        &second_token,
                        second_anonymous_uri.as_str(),
                        StatusCode::OK,
                    ),
                ] {
                    let response = call(
                        application.clone(),
                        request(Method::PATCH, uri, Some(credential), r#"{"title":"edited"}"#),
                    )
                    .await;
                    assert_eq!(response.status(), expected, "PATCH {uri}");
                    let body = json(response).await;
                    if expected == StatusCode::OK {
                        assert_eq!(body["title"], "edited");
                    } else {
                        assert_eq!(body["error"]["code"], "not_found");
                    }
                }
                let concealed_before_body = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/owned-auth",
                        Some(&unrelated_key),
                        "{ malformed body",
                    ),
                )
                .await;
                assert_eq!(concealed_before_body.status(), StatusCode::NOT_FOUND);
                assert_eq!(
                    json(concealed_before_body).await["error"]["code"],
                    "not_found",
                    "object authority is resolved before strict body validation"
                );

                let disabled = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;
                let former_owner = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/links/owned-auth", Some(&owner_key), ""),
                )
                .await;
                assert_eq!(former_owner.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(former_owner).await["error"]["code"], "invalid_credentials");
                let suspended_admin_read = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/links/owned-auth", Some(&admin_key), ""),
                )
                .await;
                assert_eq!(suspended_admin_read.status(), StatusCode::OK);
                let suspended_body = json(suspended_admin_read).await;
                assert_eq!(suspended_body["active"], false);
                assert_eq!(suspended_body["enabled"], true);
                let admin_patch = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/owned-auth",
                        Some(&admin_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(admin_patch.status(), StatusCode::OK);
                assert_eq!(json(admin_patch).await["active"], false);
                let reenabled = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":true}"#,
                    ),
                )
                .await;
                assert_eq!(reenabled.status(), StatusCode::OK);
                let _ = json(reenabled).await;

                let unauthorized_delete = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        "/api/v1/links/owned-delete",
                        Some(&unrelated_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(unauthorized_delete.status(), StatusCode::NOT_FOUND);
                assert_eq!(json(unauthorized_delete).await["error"]["code"], "not_found");
                let deleted_owner = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        "/api/v1/links/owned-delete",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_owner.status(), StatusCode::NO_CONTENT);
                assert!(openapi::validates_headers("Deleted", deleted_owner.headers()));
                assert!(bytes(deleted_owner).await.is_empty());
                let repeated_owner_delete = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        "/api/v1/links/owned-delete",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(repeated_owner_delete.status(), StatusCode::NOT_FOUND);

                let deleted_anonymous = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        &anonymous_uri,
                        Some(&token),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_anonymous.status(), StatusCode::NO_CONTENT);
                assert!(openapi::validates_headers("Deleted", deleted_anonymous.headers()));
                assert!(bytes(deleted_anonymous).await.is_empty());
                let former_token = call(
                    application.clone(),
                    request(Method::GET, &anonymous_uri, Some(&token), ""),
                )
                .await;
                assert_eq!(former_token.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(former_token).await["error"]["code"], "invalid_credentials");
                let admin_unknown = call(
                    application.clone(),
                    request(Method::GET, &anonymous_uri, Some(&admin_key), ""),
                )
                .await;
                assert_eq!(admin_unknown.status(), StatusCode::NOT_FOUND);
                let reservation: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM code_reservations WHERE short_code = $1)",
                )
                .bind(&anonymous_code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert!(reservation);

                let cascade = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://account-cascade.example/","short_code":"public-cascade","tags":["cascade"]}"#,
                    ),
                )
                .await;
                assert_eq!(cascade.status(), StatusCode::CREATED);
                let cascade = json(cascade).await;
                let cascade_id =
                    uuid::Uuid::parse_str(cascade["link"]["id"].as_str().unwrap()).unwrap();
                sqlx::query(
                    "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, referrer, user_agent, device, browser, os) \
                     VALUES ($1, $2, 'https://account-cascade.example/', clock_timestamp(), 'GET', '192.0.2.1', NULL, NULL, 'unknown', 'unknown', 'unknown')",
                )
                .bind(uuid::Uuid::new_v4())
                .bind(cascade_id)
                .execute(database.pool())
                .await
                .unwrap();
                let account_deleted = call(
                    application.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(account_deleted.status(), StatusCode::NO_CONTENT);
                assert!(openapi::validates_headers("Deleted", account_deleted.headers()));
                assert!(bytes(account_deleted).await.is_empty());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE owner_id = $1")
                        .bind(owner_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "account deletion cascades links created through POST /api/v1/links"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits WHERE link_id = $1")
                        .bind(cascade_id)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags WHERE link_id = $1")
                        .bind(cascade_id)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'public-cascade'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                let former_owner_key = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/me", Some(&owner_key), ""),
                )
                .await;
                assert_eq!(former_owner_key.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(former_owner_key).await["error"]["code"], "invalid_credentials");

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    5,
                    "only five create attempts charged; detail, edit and delete never charge"
                );
            })
        })
        .await;
}
