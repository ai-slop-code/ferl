#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
};
use axum::{
    extract::ConnectInfo,
    http::{Method, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::net::SocketAddr;

async fn create_owned(app: &axum::Router, key: &str, code: &str) {
    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &format!(
                r#"{{"destination_url":"https://authorization.example.test/{code}","short_code":"{code}"}}"#
            ),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(json(response).await["link"]["short_code"], code);
}

async fn create_anonymous(app: &axum::Router) -> (String, String) {
    let mut anonymous_request = request(
        Method::POST,
        "/api/v1/links",
        None,
        r#"{"destination_url":"https://authorization.example.test/anonymous"}"#,
    );
    anonymous_request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 70], 47001))));
    let response = call(app.clone(), anonymous_request).await;
    let status = response.status();
    let response = json(response).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "anonymous create failed: {response}"
    );
    (
        response["link"]["short_code"].as_str().unwrap().to_owned(),
        response["management_token"].as_str().unwrap().to_owned(),
    )
}

async fn assert_report(
    app: &axum::Router,
    method: Method,
    path: &str,
    credential: Option<&str>,
    expected: StatusCode,
) -> Value {
    let response = call(app.clone(), request(method, path, credential, "")).await;
    assert_eq!(response.status(), expected, "{path}");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    if expected == StatusCode::UNAUTHORIZED {
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }
    if expected.is_success() {
        json(response).await
    } else {
        let body = json(response).await;
        body["error"]["code"].clone()
    }
}

#[tokio::test]
async fn reports_reauthorize_every_actor_conceal_objects_and_check_authority_before_query_work() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_a_key) =
                    seed_account(database.pool(), "report_auth_owner_a", "user", true).await;
                let (owner_b_id, owner_b_key) =
                    seed_account(database.pool(), "report_auth_owner_b", "user", true).await;
                let (admin_a_id, admin_a_key) =
                    seed_account(database.pool(), "report_auth_admin_a", "admin", true).await;
                let (_, admin_b_key) =
                    seed_account(database.pool(), "report_auth_admin_b", "admin", true).await;
                let app = production_router_with(
                    database,
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                )
                .await;
                create_owned(&app, &owner_a_key, "report-own-a").await;
                create_owned(&app, &owner_b_key, "report-own-b").await;
                create_owned(&app, &owner_a_key, "report-disabled").await;
                let (anonymous_a_code, token_a) = create_anonymous(&app).await;
                let (_anonymous_b_code, _token_b) = create_anonymous(&app).await;

                let disabled = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/report-disabled",
                        Some(&owner_a_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(disabled.status(), StatusCode::OK);
                let _ = json(disabled).await;

                // Every malformed operation value is hidden by the object-authority
                // result for an otherwise valid but unrelated account/token.
                for (path, credential) in [
                    (
                        "/api/v1/links/report-own-b/visits?from=not-a-date",
                        owner_a_key.as_str(),
                    ),
                    (
                        "/api/v1/links/report-own-b/stats?to=not-a-date",
                        owner_a_key.as_str(),
                    ),
                    (
                        "/api/v1/links/report-own-b/stats/breakdown?group_by=invalid&cursor=tampered",
                        owner_a_key.as_str(),
                    ),
                    (
                        "/api/v1/links/report-own-b/stats/breakdown?group_by=invalid",
                        token_a.as_str(),
                    ),
                    (
                        "/api/v1/links/no-such-report/stats?from=not-a-date",
                        owner_a_key.as_str(),
                    ),
                    (
                        "/api/v1/links/report-own-b/visits?scope=all&tag=private",
                        token_a.as_str(),
                    ),
                ] {
                    assert_eq!(
                        assert_report(&app, Method::GET, path, Some(credential), StatusCode::NOT_FOUND)
                            .await,
                        "not_found",
                        "object concealment precedes operation query work: {path}"
                    );
                }

                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-a/visits?from=not-a-date",
                        Some(&owner_a_key),
                        StatusCode::BAD_REQUEST,
                    )
                    .await,
                    "invalid_request"
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-a/stats/breakdown",
                        Some(&owner_a_key),
                        StatusCode::BAD_REQUEST,
                    )
                    .await,
                    "invalid_request",
                    "group_by is required"
                );
                for path in [
                    "/api/v1/links/report-own-a/visits?sort=created_at",
                    "/api/v1/links/report-own-a/visits?limit=0",
                    "/api/v1/links/report-own-a/visits?limit=201",
                    "/api/v1/links/report-own-a/visits?from=2026-01-01T00%3A00%3A00Z&from=2026-01-02T00%3A00%3A00Z",
                    "/api/v1/links/report-own-a/stats?limit=1",
                    "/api/v1/links/report-own-a/stats/breakdown?group_by=invalid",
                    "/api/v1/links/report-own-a/stats/breakdown?group_by=day&group_by=os",
                    "/api/v1/links/report-own-a/stats/breakdown?group_by=day&owner_id=ignored",
                    "/api/v1/links/report-own-a/stats/breakdown?group_by=day&q=ignored",
                    "/api/v1/links/report-own-a/visits?cursor=",
                    "/api/v1/links/report-own-a/stats?from=2026-01-01T00%3A00%3A00Z&to=2026-01-01T00%3A00%3A00Z",
                    "/api/v1/links/report-own-a/stats?from=2026-01-02T00%3A00%3A00Z&to=2026-01-01T00%3A00%3A00Z",
                    "/api/v1/links/report-own-a/stats?from=%GG",
                ] {
                    assert_eq!(
                        assert_report(&app, Method::GET, path, Some(&owner_a_key), StatusCode::BAD_REQUEST)
                            .await,
                        "invalid_request",
                        "strict operation allowlist: {path}"
                    );
                }

                for path in [
                    "/api/v1/links/report-own-a/visits",
                    "/api/v1/links/report-own-a/stats",
                    "/api/v1/links/report-own-a/stats/breakdown?group_by=day",
                ] {
                    let body = assert_report(
                        &app,
                        Method::GET,
                        path,
                        Some(&owner_a_key),
                        StatusCode::OK,
                    )
                    .await;
                    if path.ends_with("/stats") {
                        assert_eq!(body, serde_json::json!({"lifetime_visits":0,"matching_visits":0}));
                    } else {
                        assert_eq!(body, serde_json::json!({"items": [], "next_cursor": null}));
                    }
                }
                // Disabled links remain reportable to their enabled owner.
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-disabled/stats",
                        Some(&owner_a_key),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0
                );

                for path in [
                    "/api/v1/links/report-own-a/stats",
                    "/api/v1/links/report-own-b/visits?limit=1",
                    "/api/v1/links/report-disabled/stats/breakdown?group_by=os",
                ] {
                    let body = assert_report(
                        &app,
                        Method::GET,
                        path,
                        Some(&admin_a_key),
                        StatusCode::OK,
                    )
                    .await;
                    if path.ends_with("/stats") {
                        assert_eq!(body["lifetime_visits"], 0);
                    } else {
                        assert_eq!(body["items"], serde_json::json!([]));
                    }
                }

                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&token_a),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&admin_a_key),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0,
                    "enabled administrators can report anonymous links"
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{_anonymous_b_code}/stats"),
                        Some(&token_a),
                        StatusCode::NOT_FOUND,
                    )
                    .await,
                    "not_found"
                );
                for credential in [None, Some("malformed"), Some("ferl_ak_v1_invalid")] {
                    let response = call(
                        app.clone(),
                        request(
                            Method::GET,
                            "/api/v1/links/report-own-a/stats",
                            credential,
                            "",
                        ),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
                    let _ = json(response).await;
                }
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/%72eport-own-a/stats",
                        Some(&owner_a_key),
                        StatusCode::BAD_REQUEST,
                    )
                    .await,
                    "invalid_request",
                    "percent-encoded code characters never alias the stored code"
                );

                let before_visits: i64 = sqlx::query_scalar("SELECT count(*) FROM visits")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                let before_charges: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();

                // Link-token rotation is fresh across requests. Disabling anonymous
                // creation in a replacement production router does not revoke it.
                let rotated_token = call(
                    app.clone(),
                    request(
                        Method::POST,
                        &format!("/api/v1/links/{anonymous_a_code}/management-token"),
                        Some(&token_a),
                        "",
                    ),
                )
                .await;
                assert_eq!(rotated_token.status(), StatusCode::OK);
                let replacement_token = json(rotated_token).await["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&token_a),
                        StatusCode::UNAUTHORIZED,
                    )
                    .await,
                    "invalid_credentials"
                );
                let creation_disabled =
                    production_router_with(database, &[("FERL_ANONYMOUS_CREATION", "false")])
                        .await;
                assert_eq!(
                    assert_report(
                        &creation_disabled,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&replacement_token),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0
                );

                // Account suspension removes owner-token authority but preserves an
                // administrator's ability to inspect the live link and old totals.
                let suspended = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_b_id.as_uuid()),
                        Some(&admin_a_key),
                        r#"{"enabled":false}"#,
                    ),
                )
                .await;
                assert_eq!(suspended.status(), StatusCode::OK);
                let _ = json(suspended).await;
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-b/stats",
                        Some(&owner_b_key),
                        StatusCode::UNAUTHORIZED,
                    )
                    .await,
                    "invalid_credentials"
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-b/stats",
                        Some(&admin_b_key),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0
                );

                // An account key rotation preserves ownership but revokes the old
                // credential on the next request.
                let rotated_key = call(
                    app.clone(),
                    request(Method::POST, "/api/v1/me/api-key", Some(&owner_a_key), ""),
                )
                .await;
                assert_eq!(rotated_key.status(), StatusCode::OK);
                let replacement_key = json(rotated_key).await["api_key"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-a/stats",
                        Some(&owner_a_key),
                        StatusCode::UNAUTHORIZED,
                    )
                    .await,
                    "invalid_credentials"
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-a/stats",
                        Some(&replacement_key),
                        StatusCode::OK,
                    )
                    .await["lifetime_visits"],
                    0
                );

                // Demotion removes cross-account admin reporting authority; the
                // remaining enabled admin protects the global last-admin invariant.
                let demoted = call(
                    app.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", admin_a_id.as_uuid()),
                        Some(&admin_b_key),
                        r#"{"role":"user"}"#,
                    ),
                )
                .await;
                assert_eq!(demoted.status(), StatusCode::OK);
                let _ = json(demoted).await;
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-b/stats",
                        Some(&admin_a_key),
                        StatusCode::NOT_FOUND,
                    )
                    .await,
                    "not_found"
                );

                let deleted = call(
                    app.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/links/{anonymous_a_code}"),
                        Some(&admin_b_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert!(deleted.into_body().collect().await.unwrap().to_bytes().is_empty());
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&replacement_token),
                        StatusCode::UNAUTHORIZED,
                    )
                    .await,
                    "invalid_credentials"
                );
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        &format!("/api/v1/links/{anonymous_a_code}/stats"),
                        Some(&admin_b_key),
                        StatusCode::NOT_FOUND,
                    )
                    .await,
                    "not_found"
                );

                let deleted_account = call(
                    app.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", owner_b_id.as_uuid()),
                        Some(&admin_b_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_account.status(), StatusCode::NO_CONTENT);
                assert_eq!(
                    assert_report(
                        &app,
                        Method::GET,
                        "/api/v1/links/report-own-b/stats",
                        Some(&admin_b_key),
                        StatusCode::NOT_FOUND,
                    )
                    .await,
                    "not_found"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_visits
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_charges,
                    "reports and report failures never charge creation allowance"
                );
            })
        })
        .await;
}
