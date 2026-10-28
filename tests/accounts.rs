#[path = "support/accounts.rs"]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use axum::http::{Method, StatusCode};
use ferl_server::domain::ids::AccountId;
use serde_json::Value;
use uuid::Uuid;

use account_support::{
    assert_status, bytes, call, json as response_json, migrated_database, production_router,
    request, seed_account, seed_anonymous_link,
};

fn fields(value: &Value) -> Vec<&str> {
    let mut keys = value
        .as_object()
        .expect("response is a JSON object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

#[tokio::test]
async fn fresh_authentication_role_matrix_and_secret_rotation_are_exact() {
    let database = migrated_database().await;
    let (admin_id, admin_key) = seed_account(database.pool(), "root_admin", "admin", true).await;
    let (user_id, user_key) = seed_account(database.pool(), "user_one", "user", true).await;
    let (_, management_token) = seed_anonymous_link(database.pool(), "auth-fixture").await;
    let app = production_router(&database).await;

    let response = call(app.clone(), request(Method::GET, "/api/v1/me", None, "")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    let value = response_json(response).await;
    assert_eq!(value["error"]["code"], "authentication_required");

    let response = call(
        app.clone(),
        request(
            Method::GET,
            &format!("/api/v1/me?api_key={user_key}"),
            None,
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "authentication_required"
    );
    let response = call(
        app.clone(),
        request(
            Method::GET,
            "/api/v1/me",
            None,
            &format!("{{\"api_key\":\"{user_key}\"}}"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "authentication_required"
    );

    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some("ferl_ak_v1_bad"), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    let value = response_json(response).await;
    assert_eq!(value["error"]["code"], "invalid_credentials");

    let mut duplicate = request(Method::GET, "/api/v1/me", Some(&user_key), "");
    duplicate.headers_mut().append(
        "authorization",
        format!("Bearer {admin_key}").parse().unwrap(),
    );
    let response = call(app.clone(), duplicate).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    let value = response_json(response).await;
    assert_eq!(value["error"]["code"], "invalid_credentials");

    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&management_token), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response_json(response).await["error"]["code"], "forbidden");

    for (method, path, body) in [
        (Method::GET, "/api/v1/admin/accounts", ""),
        (Method::POST, "/api/v1/admin/accounts", "not-json"),
        (Method::GET, "/api/v1/admin/accounts/not-a-uuid", ""),
        (
            Method::PATCH,
            "/api/v1/admin/accounts/not-a-uuid",
            "not-json",
        ),
        (Method::DELETE, "/api/v1/admin/accounts/not-a-uuid", ""),
        (
            Method::POST,
            "/api/v1/admin/accounts/not-a-uuid/api-key",
            "{}",
        ),
    ] {
        let label = format!("{method} {path}");
        let response = call(
            app.clone(),
            request(method.clone(), path, Some(&user_key), body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
        assert_eq!(response_json(response).await["error"]["code"], "forbidden");
    }

    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&user_key), ""),
    )
    .await;
    assert_status(&response, StatusCode::OK);
    let account = response_json(response).await;
    assert_eq!(
        fields(&account),
        [
            "created_at",
            "enabled",
            "id",
            "role",
            "updated_at",
            "username"
        ]
    );
    assert_eq!(account["id"], user_id.as_uuid().to_string());
    assert_eq!(account["role"], "user");
    assert!(account["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(!account.to_string().contains(&user_key));

    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"created_user"}"#,
        ),
    )
    .await;
    assert_status(&response, StatusCode::CREATED);
    let created = response_json(response).await;
    assert_eq!(fields(&created), ["account", "api_key"]);
    assert_eq!(fields(&created["account"]), fields(&account));
    assert_eq!(created["account"]["role"], "user");
    assert_eq!(created["account"]["enabled"], true);
    let created_key = created["api_key"].as_str().unwrap().to_owned();
    assert!(created_key.starts_with("ferl_ak_v1_"));
    let created_id = created["account"]["id"].as_str().unwrap();
    let account_path = format!("/api/v1/admin/accounts/{created_id}");
    let key_path = format!("{account_path}/api-key");
    let route_matrix = [
        (Method::GET, "/api/v1/me".to_owned(), ""),
        (Method::POST, "/api/v1/me/api-key".to_owned(), ""),
        (Method::GET, "/api/v1/admin/accounts".to_owned(), ""),
        (
            Method::POST,
            "/api/v1/admin/accounts".to_owned(),
            r#"{"username":"matrix_user"}"#,
        ),
        (Method::GET, account_path.clone(), ""),
        (Method::PATCH, account_path.clone(), r#"{"enabled":true}"#),
        (Method::DELETE, account_path, ""),
        (Method::POST, key_path, ""),
    ];
    for (method, path, body) in route_matrix {
        for (credential, expected) in [
            (None, "authentication_required"),
            (Some("ferl_ak_v1_invalid"), "invalid_credentials"),
            (Some(management_token.as_str()), "forbidden"),
        ] {
            let response = call(
                app.clone(),
                request(method.clone(), &path, credential, body),
            )
            .await;
            let expected_status =
                if expected.starts_with("authentication_") || expected == "invalid_credentials" {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                };
            assert_eq!(response.status(), expected_status, "{method} {path}");
            if expected_status == StatusCode::UNAUTHORIZED {
                assert_eq!(response.headers()["www-authenticate"], "Bearer");
            }
            assert_eq!(response_json(response).await["error"]["code"], expected);
        }
    }

    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"created_user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "username_unavailable"
    );

    let response = call(
        app.clone(),
        request(Method::POST, "/api/v1/me/api-key", Some(&user_key), ""),
    )
    .await;
    assert_status(&response, StatusCode::OK);
    let replacement = response_json(response).await;
    assert_eq!(fields(&replacement), ["api_key"]);
    let new_key = replacement["api_key"].as_str().unwrap().to_owned();
    assert!(new_key.starts_with("ferl_ak_v1_"));

    for (key, expected) in [(&user_key, "invalid_credentials"), (&new_key, "")] {
        let response = call(
            app.clone(),
            request(Method::GET, "/api/v1/me", Some(key), ""),
        )
        .await;
        if expected.is_empty() {
            assert_eq!(response.status(), StatusCode::OK);
            let body = response_json(response).await;
            assert_eq!(body["id"], user_id.as_uuid().to_string());
        } else {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
            assert_eq!(response_json(response).await["error"]["code"], expected);
        }
    }

    let response = call(
        app.clone(),
        request(Method::POST, "/api/v1/me/api-key", Some(&new_key), "{}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request"
    );
    let still_valid = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&new_key), ""),
    )
    .await;
    assert_eq!(still_valid.status(), StatusCode::OK);

    let response = call(
        app.clone(),
        request(Method::HEAD, "/api/v1/me", Some(&new_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    assert!(bytes(response).await.is_empty());
    let response = call(app.clone(), request(Method::POST, "/api/v1/me", None, "")).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");

    let response = call(app, request(Method::PATCH, "/api/v1/unknown", None, "")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "not_found");

    let _ = admin_id;
    database.close().await;
}

#[tokio::test]
async fn account_admin_changes_preserve_noops_disabled_resets_and_last_admin() {
    let database = migrated_database().await;
    let (admin_id, admin_key) = seed_account(database.pool(), "admin_one", "admin", true).await;
    let (second_admin, second_key) =
        seed_account(database.pool(), "admin_two", "admin", true).await;
    let (third_admin, third_key) =
        seed_account(database.pool(), "admin_three", "admin", true).await;
    let app = production_router(&database).await;

    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"disabled_user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = response_json(response).await;
    let target_id = created["account"]["id"].as_str().unwrap().to_owned();
    let target_key = created["api_key"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/admin/accounts/{target_id}");

    let before = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        "SELECT updated_at FROM accounts WHERE id = $1",
    )
    .bind(Uuid::parse_str(&target_id).unwrap())
    .fetch_one(database.pool())
    .await
    .unwrap();
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":true}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let no_op = response_json(response).await;
    let after = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        "SELECT updated_at FROM accounts WHERE id = $1",
    )
    .bind(Uuid::parse_str(&target_id).unwrap())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(before, after, "no-op PATCH preserves updated_at");
    assert_eq!(no_op["enabled"], true);

    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":false}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let disabled = response_json(response).await;
    assert_eq!(disabled["enabled"], false);
    let old_target = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&target_key), ""),
    )
    .await;
    assert_eq!(old_target.status(), StatusCode::UNAUTHORIZED);
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":true}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let restored = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&target_key), ""),
    )
    .await;
    assert_eq!(
        restored.status(),
        StatusCode::OK,
        "reenabling restores the preserved key"
    );
    let _ = response_json(restored).await;
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":false}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = call(
        app.clone(),
        request(
            Method::POST,
            &format!("{path}/api-key"),
            Some(&admin_key),
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reset = response_json(response).await;
    assert_eq!(fields(&reset), ["api_key"]);
    let reset_key = reset["api_key"].as_str().unwrap().to_owned();
    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&reset_key), ""),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "disabled reset stays disabled"
    );
    let response = call(
        app.clone(),
        request(Method::GET, &path, Some(&admin_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !response_json(response)
            .await
            .to_string()
            .contains(&reset_key)
    );
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":true}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reset_key_auth = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&reset_key), ""),
    )
    .await;
    assert_eq!(reset_key_auth.status(), StatusCode::OK);
    let old_key_auth = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&target_key), ""),
    )
    .await;
    assert_eq!(old_key_auth.status(), StatusCode::UNAUTHORIZED);

    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &path,
            Some(&admin_key),
            r#"{"enabled":false,"role":"user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let admin_path = format!("/api/v1/admin/accounts/{}", admin_id.as_uuid());
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &admin_path,
            Some(&second_key),
            r#"{"enabled":false}"#,
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "self-disable is allowed with another admin"
    );
    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/admin/accounts", Some(&admin_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let second_path = format!("/api/v1/admin/accounts/{}", second_admin.as_uuid());
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &second_path,
            Some(&second_key),
            r#"{"role":"user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&second_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/admin/accounts", Some(&second_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let third_path = format!("/api/v1/admin/accounts/{}", third_admin.as_uuid());
    let response = call(
        app.clone(),
        request(
            Method::PATCH,
            &third_path,
            Some(&third_key),
            r#"{"role":"user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "last_admin_required"
    );
    let response = call(
        app,
        request(Method::DELETE, &third_path, Some(&third_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "last_admin_required"
    );

    database.close().await;
}

#[tokio::test]
async fn account_deletion_cascades_children_keeps_reservation_and_allows_clean_username_reuse() {
    let database = migrated_database().await;
    let (admin_id, admin_key) = seed_account(database.pool(), "delete_admin", "admin", true).await;
    let (_other_admin, other_admin_key) =
        seed_account(database.pool(), "delete_admin_two", "admin", true).await;
    let (owner_id, owner_key) = seed_account(database.pool(), "reusable_name", "user", true).await;
    let link_id = Uuid::new_v4();
    let code = "owned-delete-code";
    let destination = "https://destination.example.test/delete";
    let search = ferl_server::domain::text::ComparisonText::from_persisted(destination);
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
        .bind(code)
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO links (id, short_code, owner_id, destination_url, destination_search, enabled, visit_count) \
         VALUES ($1, $2, $3, $4, $5, true, 1)",
    )
    .bind(link_id)
    .bind(code)
    .bind(owner_id.as_uuid())
    .bind(destination)
    .bind(search.as_str())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'private-tag')")
        .bind(link_id)
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, device, browser, os) \
         VALUES ($1, $2, $3, clock_timestamp(), 'GET', '192.0.2.9'::inet, 'unknown', 'unknown', 'unknown')",
    )
    .bind(Uuid::new_v4())
    .bind(link_id)
    .bind(destination)
    .execute(database.pool())
    .await
    .unwrap();
    let app = production_router(&database).await;
    let path = format!("/api/v1/admin/accounts/{}", owner_id.as_uuid());
    let response = call(
        app.clone(),
        request(Method::DELETE, &path, Some(&admin_key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(bytes(response).await.is_empty(), "204 has zero body bytes");

    let account_count: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE id = $1")
        .bind(owner_id.as_uuid())
        .fetch_one(database.pool())
        .await
        .unwrap();
    let link_count: i64 = sqlx::query_scalar("SELECT count(*) FROM links WHERE id = $1")
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
    let tag_count: i64 = sqlx::query_scalar("SELECT count(*) FROM link_tags WHERE link_id = $1")
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
    let visit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM visits WHERE link_id = $1")
        .bind(link_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
    let reservation: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM code_reservations WHERE short_code = $1)")
            .bind(code)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        (
            account_count,
            link_count,
            tag_count,
            visit_count,
            reservation
        ),
        (0, 0, 0, 0, true)
    );

    let repeated = call(
        app.clone(),
        request(Method::DELETE, &path, Some(&admin_key), ""),
    )
    .await;
    assert_eq!(repeated.status(), StatusCode::NOT_FOUND);
    let former = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&owner_key), ""),
    )
    .await;
    assert_eq!(former.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(former).await["error"]["code"],
        "invalid_credentials"
    );

    let reused = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"reusable_name"}"#,
        ),
    )
    .await;
    assert_eq!(reused.status(), StatusCode::CREATED);
    let reused = response_json(reused).await;
    assert_ne!(reused["account"]["id"], owner_id.as_uuid().to_string());
    let inherited: i64 = sqlx::query_scalar("SELECT count(*) FROM links WHERE owner_id = $1")
        .bind(
            reused["account"]["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(inherited, 0);

    let self_path = format!("/api/v1/admin/accounts/{}", admin_id.as_uuid());
    let self_delete = call(
        app,
        request(Method::DELETE, &self_path, Some(&admin_key), ""),
    )
    .await;
    assert_eq!(self_delete.status(), StatusCode::NO_CONTENT);
    assert!(bytes(self_delete).await.is_empty());
    let enabled_admins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND enabled")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(enabled_admins, 1);
    assert!(!other_admin_key.is_empty());
    database.close().await;
}

#[tokio::test]
async fn account_pagination_is_keyset_bound_and_continues_after_boundary_deletion() {
    let database = migrated_database().await;
    let actor_id = AccountId::new(Uuid::from_u128(100));
    let actor_key = "ferl_ak_v1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let actor_digest = ferl_server::domain::secret::SecretDigest::from_presented_secret(actor_key);
    sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest, created_at, updated_at) \
         VALUES ($1, 'admin_actor', 'admin', true, $2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .bind(actor_id.as_uuid())
    .bind(actor_digest.as_bytes().as_slice())
    .execute(database.pool())
    .await
    .unwrap();
    for id in 1..=5_u128 {
        let uuid = Uuid::from_u128(id);
        let key = format!("fixture_key_{id}");
        let digest = ferl_server::domain::secret::SecretDigest::from_presented_secret(&key);
        sqlx::query(
            "INSERT INTO accounts (id, username, role, enabled, api_key_digest, created_at, updated_at) \
             VALUES ($1, $2, 'user', true, $3, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(uuid)
        .bind(format!("fixture_{id}"))
        .bind(digest.as_bytes().as_slice())
        .execute(database.pool())
        .await
        .unwrap();
    }
    let app_a = production_router(&database).await;
    let app_b = production_router(&database).await;

    let response = call(
        app_a.clone(),
        request(
            Method::GET,
            "/api/v1/admin/accounts?limit=2",
            Some(actor_key),
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = response_json(response).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let first_page_cursor = cursor.clone();
    let boundary_id = first["items"][1]["id"].as_str().unwrap();
    assert_eq!(boundary_id, Uuid::from_u128(5).to_string());
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(Uuid::from_u128(5))
        .execute(database.pool())
        .await
        .unwrap();

    let response = call(
        app_b.clone(),
        request(
            Method::GET,
            &format!("/api/v1/admin/accounts?limit=1&cursor={cursor}"),
            Some(actor_key),
            "",
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "page continues on another instance"
    );
    let second = response_json(response).await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["items"][0]["id"], Uuid::from_u128(4).to_string());
    let next = second["next_cursor"].as_str().unwrap().to_owned();

    let changed = format!("{}x", next);
    let response = call(
        app_a.clone(),
        request(
            Method::GET,
            &format!("/api/v1/admin/accounts?cursor={changed}"),
            Some(actor_key),
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request"
    );

    let (other_admin, other_key) =
        seed_account(database.pool(), "other_admin", "admin", true).await;
    let response = call(
        app_b.clone(),
        request(
            Method::GET,
            &format!("/api/v1/admin/accounts?cursor={next}"),
            Some(&other_key),
            "",
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "cursor is bound to actor account"
    );
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_request"
    );

    let mut cursor = second["next_cursor"].as_str().unwrap().to_owned();
    for expected in [3_u128, 2, 1] {
        let response = call(
            app_b.clone(),
            request(
                Method::GET,
                &format!("/api/v1/admin/accounts?limit=1&cursor={cursor}"),
                Some(actor_key),
                "",
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = response_json(response).await;
        assert_eq!(
            page["items"][0]["id"],
            Uuid::from_u128(expected).to_string()
        );
        cursor = page["next_cursor"].as_str().unwrap_or_default().to_owned();
        if expected == 1 {
            assert!(page["next_cursor"].is_null());
        } else {
            assert!(!cursor.is_empty());
        }
    }
    sqlx::query("DELETE FROM accounts WHERE id = ANY($1)")
        .bind((1..=4_u128).map(Uuid::from_u128).collect::<Vec<_>>())
        .execute(database.pool())
        .await
        .unwrap();
    let empty = call(
        app_b,
        request(
            Method::GET,
            &format!("/api/v1/admin/accounts?limit=1&cursor={first_page_cursor}"),
            Some(actor_key),
            "",
        ),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::OK);
    let empty = response_json(empty).await;
    assert_eq!(empty["items"], serde_json::json!([]));
    assert!(
        empty["next_cursor"].is_null(),
        "an empty live page is exhausted"
    );
    let _ = app_a;
    let _ = cursor;
    let _ = other_admin;
    database.close().await;
}
