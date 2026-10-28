#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi_validation;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::collections::BTreeSet;

use openapi_validation::{document, validates, validates_ref_with_siblings};
use serde_json::{Value, json};

#[test]
fn openapi_31_document_and_all_local_references_are_present() {
    let doc = document();
    assert_eq!(doc["openapi"], "3.1.0");
    assert!(
        doc["paths"]
            .as_object()
            .is_some_and(|paths| paths.len() >= 10)
    );
    assert!(doc["components"]["schemas"].as_object().is_some());

    fn visit(value: &Value, root: &Value, references: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    assert!(reference.starts_with("#/"), "all references are local");
                    assert!(
                        root.pointer(reference.trim_start_matches('#')).is_some(),
                        "resolved {reference}"
                    );
                    references.insert(reference.to_owned());
                }
                for value in object.values() {
                    visit(value, root, references);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, root, references);
                }
            }
            _ => {}
        }
    }

    let mut references = BTreeSet::new();
    visit(&doc, &doc, &mut references);
    assert!(
        references.len() > 20,
        "operation, parameter, response, and schema refs are exercised"
    );

    let paths = doc["paths"].as_object().unwrap();
    let methods = ["get", "post", "patch", "delete", "head"];
    let mut operation_ids = BTreeSet::new();
    let mut operations = 0;
    for (path, item) in paths {
        let item = item.as_object().unwrap();
        for method in methods {
            let Some(operation) = item.get(method).and_then(Value::as_object) else {
                continue;
            };
            operations += 1;
            let operation_id = operation["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} declares an operation ID"));
            assert!(
                operation_ids.insert(operation_id.to_owned()),
                "operation IDs are unique"
            );
            assert!(
                operation["responses"]
                    .as_object()
                    .is_some_and(|responses| !responses.is_empty())
            );
        }
    }
    assert!(
        operations >= 20,
        "business, health, and redirect methods are included"
    );
}

#[test]
fn json_schema_ref_sibling_assertions_are_conjunctive_and_formats_are_asserted() {
    let restricted_role = json!({
        "$ref": "#/components/schemas/Role",
        "enum": ["admin"]
    });
    assert!(validates_ref_with_siblings(
        &restricted_role,
        &json!("admin")
    ));
    assert!(!validates_ref_with_siblings(
        &restricted_role,
        &json!("user")
    ));
    assert!(!validates_ref_with_siblings(
        &restricted_role,
        &json!("root")
    ));

    let valid_account = json!({
        "id": "4a8ffcd5-3066-4519-a68e-060f956ab500",
        "username": "alice_1",
        "role": "user",
        "enabled": true,
        "created_at": "2026-10-28T10:00:00.123456Z",
        "updated_at": "2026-10-28T10:00:00.123456Z"
    });
    assert!(validates("Account", &valid_account));
    let mut invalid = valid_account.clone();
    invalid["id"] = json!("not-a-uuid");
    assert!(!validates("Account", &invalid), "uuid format is asserted");
    let mut invalid = valid_account;
    invalid["updated_at"] = json!("not-a-dateZ");
    assert!(
        !validates("Account", &invalid),
        "date-time format and UTC suffix are asserted"
    );
}

#[tokio::test]
async fn actual_bootstrap_dto_and_account_http_responses_match_openapi_components() {
    use std::collections::BTreeMap;

    use account_support::{
        call, json as response_json, migrated_database, production_router, request,
    };
    use axum::http::{Method, StatusCode};
    use ferl_server::config::BootstrapConfig;

    let database = migrated_database().await;
    let bootstrap_config = BootstrapConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        database.schema_database_url().to_owned(),
    )]))
    .expect("command-specific bootstrap profile needs only the database and bounded defaults");
    let output_dto = ferl_server::accounts::bootstrap::run(&bootstrap_config, "openapi_admin")
        .await
        .expect("fresh migrated schema bootstraps through the production service");
    let serialized = serde_json::to_value(output_dto).expect("bootstrap DTO serializes");
    assert!(validates("AccountCreation", &serialized));
    let key = serialized["api_key"].as_str().unwrap().to_owned();

    let app = production_router(&database).await;
    let response = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let current = response_json(response).await;
    assert!(validates("Account", &current));

    let response = call(
        app.clone(),
        request(
            Method::GET,
            "/api/v1/admin/accounts?limit=1",
            Some(&key),
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let collection = response_json(response).await;
    assert!(validates("AccountCollection", &collection));

    let response = call(
        app.clone(),
        request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&key),
            r#"{"username":"schema_user","role":"user"}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = response_json(response).await;
    assert!(validates("AccountCreation", &created));
    let created_key = created["api_key"].as_str().unwrap();
    assert!(created_key.starts_with("ferl_ak_v1_"));

    let response = call(
        app.clone(),
        request(Method::POST, "/api/v1/me/api-key", Some(&key), ""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let replaced = response_json(response).await;
    assert!(validates("ApiKeyReplacement", &replaced));

    for (schema, mut invalid) in [
        ("Account", current.clone()),
        ("AccountCreation", created.clone()),
        ("ApiKeyReplacement", replaced.clone()),
    ] {
        match schema {
            "Account" => {
                invalid.as_object_mut().unwrap().remove("updated_at");
            }
            "AccountCreation" => {
                invalid["account"]["api_key_digest"] = json!("must-not-leak");
            }
            "ApiKeyReplacement" => {
                invalid["api_key_digest"] = json!("must-not-leak");
            }
            _ => unreachable!(),
        }
        assert!(
            !validates(schema, &invalid),
            "validator rejects invalid {schema}"
        );
    }
    let mut invalid_timestamp = current;
    invalid_timestamp["created_at"] = json!("not-a-timestampZ");
    assert!(!validates("Account", &invalid_timestamp));
    let too_many = json!({
        "items": vec![created["account"].clone(); 201],
        "next_cursor": null
    });
    assert!(!validates("AccountCollection", &too_many));
    assert!(!created.to_string().contains(&key));
    database.close().await;
}

#[tokio::test]
async fn actual_redirect_head_and_failure_responses_match_openapi_components() {
    use account_support::{migrated_database, seed_anonymous_link};
    use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
    use ferl_server::app;
    use http_body_util::BodyExt;
    use openapi_validation::{validates_headers, validates_response};
    use redirect_support::{call, request, state};
    use std::{net::SocketAddr, time::Duration};
    use tokio::time::timeout;

    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                seed_anonymous_link(database.pool(), "openapi-redirect").await;
                let app_state = state(database.schema_database_url(), &[]);
                app_state.verify_schema().await.unwrap();
                let app = app::router(app_state.clone());
                let peer = SocketAddr::from(([192, 0, 2, 91], 45091));

                let get = call(
                    app.clone(),
                    request(Method::GET, "/openapi-redirect?credential=ignored", peer),
                )
                .await;
                assert_eq!(get.status(), StatusCode::FOUND);
                assert_eq!(
                    get.headers()[header::LOCATION],
                    "https://destination.example.test/path"
                );
                assert!(validates_headers("Redirect", get.headers()));
                assert!(valid_request_id_header(get.headers()));
                assert!(
                    get.into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );

                let head = call(
                    app.clone(),
                    request(Method::HEAD, "/openapi-redirect", peer),
                )
                .await;
                assert_eq!(head.status(), StatusCode::FOUND);
                assert!(validates_response("Redirect", head.headers(), None, true));
                assert!(valid_request_id_header(head.headers()));
                let head_headers = head.headers().clone();
                assert!(!validates_response(
                    "Redirect",
                    &head_headers,
                    Some(&json!({})),
                    true
                ));
                assert!(
                    head.into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );

                let mut invalid_redirect_headers = HeaderMap::new();
                invalid_redirect_headers
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                invalid_redirect_headers
                    .insert(header::LOCATION, HeaderValue::from_static("not-a-url"));
                invalid_redirect_headers
                    .insert("x-request-id", HeaderValue::from_static("not-a-uuid"));
                assert!(!validates_headers("Redirect", &invalid_redirect_headers));
                let mut invalid_request_id = head_headers.clone();
                invalid_request_id.insert("x-request-id", HeaderValue::from_static("not-a-uuid"));
                assert!(validates_headers("Redirect", &invalid_request_id));
                assert!(!valid_request_id_header(&invalid_request_id));
                let mut invalid_redirect_headers = head_headers.clone();
                invalid_redirect_headers.remove(header::LOCATION);
                assert!(!validates_headers("Redirect", &invalid_redirect_headers));
                let mut invalid_redirect_headers = head_headers.clone();
                invalid_redirect_headers
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("public"));
                assert!(!validates_headers("Redirect", &invalid_redirect_headers));

                let miss = call(app.clone(), request(Method::GET, "/openapi-missing", peer)).await;
                assert_eq!(miss.status(), StatusCode::NOT_FOUND);
                assert!(!miss.headers().contains_key(header::LOCATION));
                let miss_id = miss.headers()["x-request-id"].to_str().unwrap().to_owned();
                let miss_headers = miss.headers().clone();
                let miss_body: Value =
                    serde_json::from_slice(&miss.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                assert!(validates_response(
                    "NotFound",
                    &miss_headers,
                    Some(&miss_body),
                    false
                ));
                assert!(valid_request_id_header(&miss_headers));
                assert_eq!(miss_body["request_id"], miss_id);
                let mut malformed_error = miss_body.clone();
                malformed_error["unexpected"] = json!("invalid control");
                assert!(!validates("Error", &malformed_error));
                assert!(!validates_response(
                    "NotFound",
                    &miss_headers,
                    Some(&malformed_error),
                    false
                ));

                let head_miss =
                    call(app.clone(), request(Method::HEAD, "/openapi-missing", peer)).await;
                assert_eq!(head_miss.status(), StatusCode::NOT_FOUND);
                assert!(!head_miss.headers().contains_key(header::LOCATION));
                assert!(validates_headers("HeadNotFound", head_miss.headers()));
                assert!(
                    head_miss
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
                let mut invalid_error_headers = miss_headers.clone();
                invalid_error_headers.insert("x-request-id", HeaderValue::from_static("bad-id"));
                assert!(validates_headers("NotFound", &invalid_error_headers));
                assert!(!valid_request_id_header(&invalid_error_headers));

                let method = call(
                    app.clone(),
                    request(Method::POST, "/openapi-redirect", peer),
                )
                .await;
                assert_eq!(method.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(method.headers()[header::ALLOW], "GET, HEAD");
                assert!(validates_short_link_allow(method.headers()));
                assert!(valid_request_id_header(method.headers()));
                let method_id = method.headers()["x-request-id"]
                    .to_str()
                    .unwrap()
                    .to_owned();
                let method_headers = method.headers().clone();
                let method_body: Value =
                    serde_json::from_slice(&method.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                assert!(validates_response(
                    "MethodNotAllowed",
                    &method_headers,
                    Some(&method_body),
                    false
                ));
                assert_eq!(method_body["request_id"], method_id);
                let mut invalid_allow = method_headers.clone();
                invalid_allow.insert(header::ALLOW, HeaderValue::from_static("GET"));
                assert!(!validates_short_link_allow(&invalid_allow));

                let unavailable = redirect_support::state(
                    "postgres://ferl:ferl@127.0.0.1:1/ferl_test",
                    &[("FERL_DB_ACQUIRE_TIMEOUT_SECS", "1")],
                );
                let unavailable_app = app::router(unavailable.clone());
                let unavailable_get = call(
                    unavailable_app.clone(),
                    request(Method::GET, "/storage-unavailable", peer),
                )
                .await;
                assert_eq!(unavailable_get.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert!(!unavailable_get.headers().contains_key(header::LOCATION));
                let unavailable_id = unavailable_get.headers()["x-request-id"]
                    .to_str()
                    .unwrap()
                    .to_owned();
                let unavailable_get_headers = unavailable_get.headers().clone();
                let unavailable_body: Value = serde_json::from_slice(
                    &unavailable_get
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes(),
                )
                .unwrap();
                assert!(validates_response(
                    "Unavailable",
                    &unavailable_get_headers,
                    Some(&unavailable_body),
                    false
                ));
                assert!(valid_request_id_header(&unavailable_get_headers));
                assert_eq!(unavailable_body["request_id"], unavailable_id);
                let unavailable_head = call(
                    unavailable_app.clone(),
                    request(Method::HEAD, "/storage-unavailable", peer),
                )
                .await;
                assert_eq!(unavailable_head.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert!(validates_headers(
                    "HeadUnavailable",
                    unavailable_head.headers()
                ));
                let unavailable_head_headers = unavailable_head.headers().clone();
                assert!(
                    unavailable_head
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
                let mut invalid_head_headers = unavailable_head_headers;
                invalid_head_headers
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=1"));
                assert!(!validates_headers("HeadUnavailable", &invalid_head_headers));

                for (state, router) in [(unavailable, unavailable_app), (app_state, app)] {
                    drop(router);
                    let pool = state.pool.clone();
                    drop(state);
                    timeout(Duration::from_secs(5), pool.close())
                        .await
                        .expect("OpenAPI test pool closes before schema cleanup");
                }
            })
        })
        .await;
}

fn validates_short_link_allow(headers: &http::HeaderMap) -> bool {
    openapi_validation::validates_headers("MethodNotAllowed", headers)
        && headers
            .get(http::header::ALLOW)
            .is_some_and(|allow| allow == "GET, HEAD")
}

fn valid_request_id_header(headers: &http::HeaderMap) -> bool {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
}

#[test]
fn account_link_and_conditional_secret_fixtures_match_checked_in_components() {
    let account = json!({
        "id": "4a8ffcd5-3066-4519-a68e-060f956ab500",
        "username": "alice_1",
        "role": "user",
        "enabled": true,
        "created_at": "2026-10-28T10:00:00.000000Z",
        "updated_at": "2026-10-28T10:00:00.000000Z"
    });
    assert!(validates("Account", &account));
    let account_creation = json!({
        "account": account.clone(),
        "api_key": "ferl_ak_v1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    });
    assert!(validates("AccountCreation", &account_creation));
    assert!(validates(
        "ApiKeyReplacement",
        &json!({"api_key":"ferl_ak_v1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"})
    ));
    assert!(validates(
        "ManagementTokenReplacement",
        &json!({"management_token":"ferl_mt_v1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"})
    ));
    let mut account_with_unknown = account.clone();
    account_with_unknown["api_key_digest"] = json!("must never be public");
    assert!(!validates("Account", &account_with_unknown));

    let link = json!({
        "id": "1f1bf5f6-075c-4cf1-a44b-05fca986b100",
        "short_code": "rust-docs",
        "short_url": "https://s.example/rust-docs",
        "destination_url": "https://example.com/docs?edition=2#intro",
        "title": null,
        "tags": [],
        "owner_id": null,
        "enabled": true,
        "active": true,
        "visit_count": 0,
        "created_at": "2026-10-28T10:00:00.000000Z",
        "updated_at": "2026-10-28T10:00:00.000000Z"
    });
    assert!(validates("Link", &link));
    assert!(!validates("Link", &json!({"id": link["id"]})));

    let anonymous = json!({
        "link": link,
        "management_token": "ferl_mt_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    assert!(validates("LinkCreation", &anonymous));
    let owned_link = json!({
        "id": "1f1bf5f6-075c-4cf1-a44b-05fca986b100",
        "short_code": "rust-docs",
        "short_url": "https://s.example/rust-docs",
        "destination_url": "https://example.com/docs",
        "title": "Rust docs",
        "tags": [],
        "owner_id": "4a8ffcd5-3066-4519-a68e-060f956ab500",
        "enabled": true,
        "active": true,
        "visit_count": 0,
        "created_at": "2026-10-28T10:00:00Z",
        "updated_at": "2026-10-28T10:00:00Z"
    });
    assert!(validates("LinkCreation", &json!({"link": owned_link})));
    assert!(!validates(
        "LinkCreation",
        &json!({"link": owned_link, "management_token": "must-not-be-emitted"})
    ));
    let mut invalid_anonymous = anonymous.clone();
    invalid_anonymous
        .as_object_mut()
        .unwrap()
        .remove("management_token");
    assert!(!validates("LinkCreation", &invalid_anonymous));
}

#[test]
fn visit_report_tag_and_collection_nullability_match_actual_schemas() {
    let visit = json!({
        "id": "1d1cfe66-f9ea-4a16-aa03-d987c54df300",
        "link_id": "1f1bf5f6-075c-4cf1-a44b-05fca986b100",
        "short_code": "rust-docs",
        "redirect_url": "https://example.com/old?edition=1#intro",
        "occurred_at": "2026-10-28T10:01:00Z",
        "method": "HEAD",
        "client_ip": "192.0.2.10",
        "referrer": null,
        "user_agent": null,
        "device": "unknown",
        "browser": "unknown",
        "os": "unknown"
    });
    assert!(validates("Visit", &visit));
    assert!(!validates(
        "Visit",
        &json!({"id": visit["id"], "referrer": null})
    ));

    assert!(validates(
        "VisitStats",
        &json!({"lifetime_visits": 0, "matching_visits": 0})
    ));
    assert!(!validates(
        "VisitStats",
        &json!({"lifetime_visits": -1, "matching_visits": 0})
    ));
    assert!(validates(
        "BreakdownItem",
        &json!({"value": null, "visits": 0})
    ));
    assert!(!validates("BreakdownItem", &json!({"visits": 0})));
    assert!(validates(
        "TagInfo",
        &json!({"name": "rust", "link_count": 1})
    ));
    assert!(!validates(
        "TagInfo",
        &json!({"name": "rust", "link_count": 0})
    ));
    assert!(validates(
        "LinkCollection",
        &json!({"items": [], "next_cursor": null})
    ));
    assert!(!validates("LinkCollection", &json!({"items": []})));
    assert!(validates("UpdateLink", &json!({"title": null, "tags": []})));
    assert!(!validates("UpdateLink", &json!({})));
    assert!(!validates("UpdateLink", &json!({"tags": null})));

    assert!(validates(
        "CreateLink",
        &json!({"destination_url":"https://example.test/","title":null,"tags":[]})
    ));
    assert!(!validates(
        "CreateLink",
        &json!({"destination_url":"https://example.test/","tags":null})
    ));
    assert!(validates("CreateAccount", &json!({"username":"alice_1"})));
    assert!(!validates(
        "CreateAccount",
        &json!({"username":"alice_1","role":null})
    ));
    assert!(validates(
        "AccountCollection",
        &json!({"items":[],"next_cursor":null})
    ));
    assert!(validates("Healthy", &json!({"status":"ok"})));
    assert!(!validates("Healthy", &json!({"status":"unavailable"})));
}
