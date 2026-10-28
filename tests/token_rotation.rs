#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi;

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
};
use axum::{body::Body, extract::ConnectInfo, http::Method, http::StatusCode};
use std::net::SocketAddr;

fn anonymous_create(body: &str) -> axum::http::Request<Body> {
    let mut request = request(Method::POST, "/api/v1/links", None, body);
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 78], 4567))));
    request
}

#[tokio::test]
async fn token_replacement_is_scoped_bodyless_one_time_and_survives_creation_disable() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, admin_key) =
                    seed_account(database.pool(), "rotation_admin", "admin", true).await;
                let (_, user_key) =
                    seed_account(database.pool(), "rotation_user", "user", true).await;
                let application = production_router_with(
                    database,
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                )
                .await;
                let created = call(
                    application.clone(),
                    anonymous_create(
                        r#"{"destination_url":"https://rotate.example/","title":"Secret test","tags":["Visible"]}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let created_headers = created.headers().clone();
                let created = json(created).await;
                assert!(openapi::validates("LinkCreation", &created));
                assert!(openapi::validates_response(
                    "LinkCreated",
                    &created_headers,
                    Some(&created),
                    false
                ));
                let code = created["link"]["short_code"].as_str().unwrap().to_owned();
                let uri = format!("/api/v1/links/{code}");
                let old_token = created["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();

                let owned = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&user_key),
                        r#"{"destination_url":"https://owned-rotation.example/","short_code":"owned-rotation"}"#,
                    ),
                )
                .await;
                assert_eq!(owned.status(), StatusCode::CREATED);
                let owned = json(owned).await;
                assert!(owned.get("management_token").is_none());

                let invalid_body = call(
                    application.clone(),
                    request(Method::POST, &format!("{uri}/management-token"), Some(&old_token), "{}"),
                )
                .await;
                assert_eq!(invalid_body.status(), StatusCode::BAD_REQUEST);
                assert_eq!(json(invalid_body).await["error"]["code"], "invalid_request");
                let still_valid = call(
                    application.clone(),
                    request(Method::GET, &uri, Some(&old_token), ""),
                )
                .await;
                assert_eq!(still_valid.status(), StatusCode::OK);

                let ordinary_account = call(
                    application.clone(),
                    request(
                        Method::POST,
                        &format!("{uri}/management-token"),
                        Some(&user_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(ordinary_account.status(), StatusCode::FORBIDDEN);
                assert_eq!(json(ordinary_account).await["error"]["code"], "forbidden");
                let ordinary_account_with_body = call(
                    application.clone(),
                    request(
                        Method::POST,
                        &format!("{uri}/management-token"),
                        Some(&user_key),
                        "{}",
                    ),
                )
                .await;
                assert_eq!(ordinary_account_with_body.status(), StatusCode::FORBIDDEN);
                assert_eq!(
                    json(ordinary_account_with_body).await["error"]["code"],
                    "forbidden",
                    "wrong operation scope precedes bodyless-request validation"
                );

                let wrong_object = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links/owned-rotation/management-token",
                        Some(&old_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(wrong_object.status(), StatusCode::NOT_FOUND);
                assert_eq!(json(wrong_object).await["error"]["code"], "not_found");
                let wrong_object_with_body = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links/owned-rotation/management-token",
                        Some(&old_token),
                        "{}",
                    ),
                )
                .await;
                assert_eq!(wrong_object_with_body.status(), StatusCode::NOT_FOUND);
                assert_eq!(
                    json(wrong_object_with_body).await["error"]["code"],
                    "not_found",
                    "concealed object authority precedes bodyless-request validation"
                );

                let owned_reset = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links/owned-rotation/management-token",
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(owned_reset.status(), StatusCode::CONFLICT);
                assert_eq!(json(owned_reset).await["error"]["code"], "not_anonymous_link");

                let old_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT management_token_digest FROM links WHERE short_code = $1",
                )
                .bind(&code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                let rotated = call(
                    application.clone(),
                    request(
                        Method::POST,
                        &format!("{uri}/management-token"),
                        Some(&old_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(rotated.status(), StatusCode::OK);
                let headers = rotated.headers().clone();
                let rotated = json(rotated).await;
                assert!(openapi::validates("ManagementTokenReplacement", &rotated));
                assert!(openapi::validates_response(
                    "ManagementTokenReplaced",
                    &headers,
                    Some(&rotated),
                    false
                ));
                let new_token = rotated["management_token"].as_str().unwrap().to_owned();
                assert!(new_token.starts_with("ferl_mt_v1_"));
                let new_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT management_token_digest FROM links WHERE short_code = $1",
                )
                .bind(&code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_ne!(old_digest, new_digest);

                let revoked = call(
                    application.clone(),
                    request(Method::GET, &uri, Some(&old_token), ""),
                )
                .await;
                assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(revoked).await["error"]["code"], "invalid_credentials");
                let detail = call(
                    application.clone(),
                    request(Method::GET, &uri, Some(&new_token), ""),
                )
                .await;
                assert_eq!(detail.status(), StatusCode::OK);
                let detail = json(detail).await;
                assert!(detail.get("management_token").is_none());
                assert!(detail.get("management_token_digest").is_none());
                assert!(!serde_json::to_string(&detail).unwrap().contains(&new_token));

                let creation_disabled =
                    production_router_with(database, &[("FERL_ANONYMOUS_CREATION", "false")])
                        .await;
                let existing_token = call(
                    creation_disabled.clone(),
                    request(Method::GET, &uri, Some(&new_token), ""),
                )
                .await;
                assert_eq!(existing_token.status(), StatusCode::OK);
                let rotated_again = call(
                    creation_disabled.clone(),
                    request(
                        Method::POST,
                        &format!("{uri}/management-token"),
                        Some(&new_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(rotated_again.status(), StatusCode::OK);
                let rotated_again = json(rotated_again).await;
                let final_token = rotated_again["management_token"].as_str().unwrap();
                let final_read = call(
                    creation_disabled,
                    request(Method::GET, &uri, Some(final_token), ""),
                )
                .await;
                assert_eq!(final_read.status(), StatusCode::OK);

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2,
                    "the two create requests are charged; token management is not"
                );
            })
        })
        .await;
}
