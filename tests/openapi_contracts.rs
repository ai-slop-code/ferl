#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi_validation;

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
