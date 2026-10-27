use std::collections::BTreeSet;

use serde_json::{Value, json};

fn document() -> Value {
    let yaml: yaml_serde::Value = yaml_serde::from_str(include_str!("../spec/openapi.yaml"))
        .expect("checked-in OpenAPI YAML parses");
    serde_json::to_value(yaml).expect("OpenAPI YAML converts to JSON values")
}

fn resolve_schema(name: &str) -> Value {
    let root = document();
    let schema = root["components"]["schemas"][name]
        .as_object()
        .unwrap_or_else(|| panic!("OpenAPI schema {name} exists"));
    let expanded = resolve_refs(&Value::Object(schema.clone()), &root, &mut Vec::new())
        .unwrap_or_else(|_| panic!("OpenAPI schema {name} has resolvable local references"));
    let mut expanded = expanded;
    expanded["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    expanded
}

fn resolve_refs(schema: &Value, root: &Value, stack: &mut Vec<String>) -> Result<Value, ()> {
    match schema {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if !reference.starts_with("#/components/schemas/")
                    || stack.iter().any(|r| r == reference)
                {
                    return Err(());
                }
                let target = root.pointer(reference.trim_start_matches('#')).ok_or(())?;
                stack.push(reference.to_owned());
                let mut expanded = resolve_refs(target, root, stack)?;
                stack.pop();
                let expanded_object = expanded.as_object_mut().ok_or(())?;
                for (key, value) in object.iter().filter(|(key, _)| key.as_str() != "$ref") {
                    expanded_object.insert(key.clone(), resolve_refs(value, root, stack)?);
                }
                Ok(expanded)
            } else {
                object
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), resolve_refs(value, root, stack)?)))
                    .collect::<Result<serde_json::Map<_, _>, ()>>()
                    .map(Value::Object)
            }
        }
        Value::Array(values) => values
            .iter()
            .map(|value| resolve_refs(value, root, stack))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

fn validates(schema_name: &str, value: &Value) -> bool {
    jsonschema::validator_for(&resolve_schema(schema_name))
        .expect("referenced OpenAPI component is valid Draft 2020-12 JSON Schema")
        .is_valid(value)
}

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
