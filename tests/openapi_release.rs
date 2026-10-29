#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi_validation;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use http::{Method, header};
use openapi_validation::{document, resolve_schema_value, validates, validates_response};
use serde_json::Value;
use serving_process::{ServingProcess, assert_headers, send};
use sha2::{Digest, Sha256};

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const OAS_31_SCHEMA_SHA256: &str =
    "da01ba28852cac0de53893797cb8d1942bc3b05084f526dcc216717dec314ed0";

type Operation = (&'static str, &'static str, &'static str);

// This independent, reviewed inventory ties every operationId in the checked-in
// contract to the public method/path. Runtime probes below use the same path
// templates but go over TCP to the production ferl-server executable.
const EXPECTED_OPERATIONS: &[Operation] = &[
    ("GET", "/api/v1/me", "getCurrentAccount"),
    ("POST", "/api/v1/me/api-key", "rotateCurrentApiKey"),
    ("GET", "/api/v1/admin/accounts", "listAccounts"),
    ("POST", "/api/v1/admin/accounts", "createAccount"),
    ("GET", "/api/v1/admin/accounts/{id}", "getAccount"),
    ("PATCH", "/api/v1/admin/accounts/{id}", "updateAccount"),
    ("DELETE", "/api/v1/admin/accounts/{id}", "deleteAccount"),
    (
        "POST",
        "/api/v1/admin/accounts/{id}/api-key",
        "resetAccountApiKey",
    ),
    ("GET", "/api/v1/links", "listLinks"),
    ("POST", "/api/v1/links", "createLink"),
    ("GET", "/api/v1/links/{code}", "getLink"),
    ("PATCH", "/api/v1/links/{code}", "updateLink"),
    ("DELETE", "/api/v1/links/{code}", "deleteLink"),
    (
        "POST",
        "/api/v1/links/{code}/management-token",
        "rotateManagementToken",
    ),
    ("GET", "/api/v1/links/{code}/visits", "listLinkVisits"),
    ("GET", "/api/v1/links/{code}/stats", "getLinkVisitStats"),
    (
        "GET",
        "/api/v1/links/{code}/stats/breakdown",
        "getLinkVisitBreakdown",
    ),
    ("GET", "/api/v1/tags", "listTags"),
    ("GET", "/api/health/live", "getLiveness"),
    ("GET", "/api/health/ready", "getReadiness"),
    ("GET", "/{short_code}", "followShortLink"),
    ("HEAD", "/{short_code}", "followShortLinkHead"),
];

#[test]
fn whole_openapi_document_and_every_json_schema_root_are_valid() {
    let doc = document();
    assert_eq!(doc["openapi"], "3.1.0");

    // The pinned OAI schema checks the complete OpenAPI 3.1 document shape.
    // Its published variant intentionally leaves Schema Objects to the selected
    // JSON Schema dialect; every schema root is therefore compiled separately
    // below with the locked Draft 2020-12 implementation.
    let oas_schema: Value =
        serde_json::from_str(include_str!("../schemas/openapi-3.1.0-2022-10-07.json"))
            .expect("vendored official OAS 3.1 schema is valid JSON");
    let schema_digest = Sha256::digest(include_bytes!("../schemas/openapi-3.1.0-2022-10-07.json"));
    let schema_digest = schema_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        schema_digest, OAS_31_SCHEMA_SHA256,
        "OAS meta-schema is pinned"
    );
    let validator = jsonschema::options()
        .should_validate_formats(false)
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&oas_schema)
        .expect("pinned OAS schema compiles as Draft 2020-12");
    let violations = validator
        .iter_errors(&doc)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "whole OpenAPI document conforms to the pinned OAS schema: {violations:#?}"
    );

    let schemas = doc["components"]["schemas"]
        .as_object()
        .expect("component schemas are an object");
    for (name, schema) in schemas {
        let expanded = resolve_schema_value(schema)
            .unwrap_or_else(|error| panic!("component schema {name}: {error}"));
        jsonschema::options()
            .should_validate_formats(true)
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&expanded)
            .unwrap_or_else(|error| panic!("component schema {name} is Draft 2020-12: {error}"));
    }

    // Inline operation/parameter/header schemas are roots too. Resolving their
    // local component references makes this a checker for the document's
    // applicable JSON Schema assertions, not only representative DTO samples.
    fn check_schema_keywords(value: &Value, pointer: &str) {
        match value {
            Value::Object(object) => {
                if let Some(schema) = object.get("schema") {
                    let expanded = resolve_schema_value(schema)
                        .unwrap_or_else(|error| panic!("schema at {pointer}: {error}"));
                    jsonschema::options()
                        .should_validate_formats(true)
                        .with_draft(jsonschema::Draft::Draft202012)
                        .build(&expanded)
                        .unwrap_or_else(|error| {
                            panic!("inline schema at {pointer} is Draft 2020-12: {error}")
                        });
                }
                for (key, nested) in object {
                    check_schema_keywords(nested, &format!("{pointer}/{key}"));
                }
            }
            Value::Array(values) => {
                for (index, nested) in values.iter().enumerate() {
                    check_schema_keywords(nested, &format!("{pointer}/{index}"));
                }
            }
            _ => {}
        }
    }
    check_schema_keywords(&doc["paths"], "#/paths");
    check_schema_keywords(&doc["components"], "#/components");

    assert!(
        document_references_resolve(&doc),
        "all local OpenAPI references resolve"
    );

    // Negative controls guard the validator itself: invalid OpenAPI structure,
    // unresolved references, invalid Schema Objects, and unsupported dialects
    // must all fail rather than producing a false-green release check.
    let mut invalid_oas = doc.clone();
    invalid_oas["info"]["version"] = serde_json::json!(17);
    assert!(
        !validator.is_valid(&invalid_oas),
        "invalid OpenAPI field types are rejected"
    );

    let mut missing_responses = doc.clone();
    missing_responses["paths"]["/api/v1/me"]["get"]
        .as_object_mut()
        .unwrap()
        .remove("responses");
    assert!(
        !all_operations_have_responses(&missing_responses),
        "OpenAPI's REQUIRED operation response map is enforced"
    );

    let mut unresolved = doc.clone();
    unresolved["components"]["responses"]["Missing"] =
        serde_json::json!({"$ref":"#/components/responses/doesNotExist"});
    assert!(
        !document_references_resolve(&unresolved),
        "unresolved local references are rejected"
    );

    let mut invalid_schema = serde_json::json!({"type":"decimal"});
    assert!(
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&invalid_schema)
            .is_err(),
        "invalid JSON Schema type controls are rejected"
    );
    invalid_schema["type"] = serde_json::json!("string");
    assert!(
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&invalid_schema)
            .unwrap()
            .is_valid(&serde_json::json!("valid"))
    );

    let unsupported_format: Value = serde_json::json!({
        "type":"string",
        "format":"date-time"
    });
    let format_validator = jsonschema::options()
        .should_validate_formats(true)
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&unsupported_format)
        .unwrap();
    assert!(!format_validator.is_valid(&serde_json::json!("not-a-date")));
}

#[test]
fn documented_method_path_operation_ids_match_the_reviewed_inventory() {
    let doc = document();
    let mut actual = BTreeSet::new();
    let methods = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    let mut operation_ids = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in methods {
            if let Some(operation) = item.get(method).filter(|value| value.is_object()) {
                let operation_id = operation["operationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{method} {path} declares operationId"));
                assert!(
                    operation_ids.insert(operation_id.to_owned()),
                    "operationId is unique"
                );
                actual.insert((
                    method.to_ascii_uppercase(),
                    path.clone(),
                    operation_id.to_owned(),
                ));
            }
        }
    }
    let expected = EXPECTED_OPERATIONS
        .iter()
        .map(|(method, path, operation_id)| {
            (
                (*method).to_owned(),
                (*path).to_owned(),
                (*operation_id).to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "checked-in route inventory is complete and stable"
    );
}

#[tokio::test]
async fn production_executable_routes_and_global_method_errors_match_openapi() {
    let database = account_support::migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let process = ServingProcess::start_with(
                    database.schema_database_url(),
                    CURSOR_KEY,
                    &[("FERL_ANONYMOUS_CREATION", "false")],
                )
                .await;
                let doc = document();
                let mut observed = BTreeMap::new();
                for (method, template, operation_id) in EXPECTED_OPERATIONS {
                    let path = template
                        .replace("{id}", "00000000-0000-4000-8000-000000000001")
                        .replace("{code}", "inventory-code")
                        .replace("{short_code}", "inventory-code");
                    let response = send(process.address, method, &path, None, "")
                        .await
                        .unwrap_or_else(|error| panic!("{method} {path} reaches ferl: {error}"));
                    assert_headers(&response);
                    let status = response.status.to_string();
                    let documented = doc["paths"][*template][method.to_ascii_lowercase()]["responses"]
                        .as_object()
                        .unwrap();
                    assert!(
                        documented.contains_key(&status),
                        "{operation_id} {method} {path} returned undocumented {status}; declared {:?}",
                        documented.keys().collect::<Vec<_>>()
                    );
                    if *method == "HEAD" {
                        assert!(response.body.is_empty(), "HEAD responses are byte-empty");
                    }
                    if let Ok(body) = serde_json::from_slice::<Value>(&response.body)
                        && body.get("error").is_some()
                    {
                        assert_eq!(
                            response.headers["x-request-id"],
                            body["request_id"].as_str().unwrap_or_default(),
                            "{operation_id} error body and header share the same request ID"
                        );
                        assert!(validates("Error", &body), "{operation_id} error schema");
                    }
                    observed.insert((*method, *template), response.status);
                }
                assert_eq!(observed.len(), EXPECTED_OPERATIONS.len());

                for (method, template, _) in EXPECTED_OPERATIONS {
                    if *method == "HEAD" {
                        continue;
                    }
                    let path = template
                        .replace("{id}", "00000000-0000-4000-8000-000000000001")
                        .replace("{code}", "inventory-code")
                        .replace("{short_code}", "inventory-code");
                    // HEAD is documented only for the public redirect. For all
                    // other operation paths it is the globally bodyless 405.
                    if *template == "/{short_code}" {
                        continue;
                    }
                    let head = send(process.address, "HEAD", &path, None, "")
                        .await
                        .unwrap_or_else(|error| panic!("HEAD {path}: {error}"));
                    assert_eq!(head.status, 405, "HEAD {path} is not implicit GET");
                    assert!(head.body.is_empty(), "HEAD {path} has no body bytes");
                    assert!(head.headers.contains_key("allow"), "HEAD {path} includes Allow");
                    let mut head_headers = http::HeaderMap::new();
                    head_headers.insert(header::ALLOW, head.headers["allow"].parse().unwrap());
                    head_headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
                    head_headers.insert(
                        http::HeaderName::from_static("x-request-id"),
                        head.headers["x-request-id"].parse().unwrap(),
                    );
                    assert!(validates_response(
                        "MethodNotAllowed",
                        &head_headers,
                        None,
                        true,
                    ));
                    let method_not_allowed = send(process.address, "PUT", &path, None, "")
                        .await
                        .unwrap_or_else(|error| panic!("PUT {path}: {error}"));
                    assert_eq!(method_not_allowed.status, 405, "PUT {path} is unsupported");
                    assert!(!method_not_allowed.headers["allow"].is_empty());
                    let error: Value = serde_json::from_slice(&method_not_allowed.body).unwrap();
                    assert_eq!(error["error"]["code"], "method_not_allowed");
                    assert!(validates("Error", &error));
                    assert_eq!(
                        method_not_allowed.headers["x-request-id"],
                        error["request_id"]
                    );
                    let mut method_headers = http::HeaderMap::new();
                    method_headers.insert(
                        header::ALLOW,
                        method_not_allowed.headers["allow"].parse().unwrap(),
                    );
                    method_headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
                    method_headers.insert(
                        http::HeaderName::from_static("x-request-id"),
                        method_not_allowed.headers["x-request-id"].parse().unwrap(),
                    );
                    assert!(validates_response(
                        "MethodNotAllowed",
                        &method_headers,
                        Some(&error),
                        false,
                    ));
                    let allowed = method_not_allowed.headers["allow"]
                        .split(',')
                        .map(str::trim)
                        .collect::<BTreeSet<_>>();
                    assert!(allowed.contains(method), "{path} Allow includes {method}");
                }

                let unsupported_api = send(process.address, "GET", "/api/v1/no-such-operation", None, "")
                    .await
                    .unwrap();
                assert_eq!(unsupported_api.status, 404);
                assert!(!unsupported_api.headers.contains_key("location"));
                let direct_root = send(process.address, "GET", "/", None, "")
                    .await
                    .unwrap();
                assert_eq!(direct_root.status, 404, "backend root belongs to the proxy");

                let output = process.stop_and_capture().await;
                assert!(output.exit_success, "production process exits naturally on SIGTERM");
                assert!(output.shutdown_elapsed < Duration::from_secs(25));
            })
        })
        .await;
}

fn document_references_resolve(doc: &Value) -> bool {
    fn visit(value: &Value, root: &Value, seen: &mut BTreeSet<String>) -> bool {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    if !reference.starts_with("#/")
                        || root.pointer(reference.trim_start_matches('#')).is_none()
                    {
                        return false;
                    }
                    seen.insert(reference.to_owned());
                }
                for child in object.values() {
                    if !visit(child, root, seen) {
                        return false;
                    }
                }
            }
            Value::Array(children) => {
                for child in children {
                    if !visit(child, root, seen) {
                        return false;
                    }
                }
            }
            _ => {}
        }
        true
    }
    let mut references = BTreeSet::new();
    visit(doc, doc, &mut references) && references.len() > 40
}

fn all_operations_have_responses(doc: &Value) -> bool {
    let methods = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    doc["paths"].as_object().is_some_and(|paths| {
        paths.values().all(|item| {
            item.as_object().is_some_and(|item| {
                methods.iter().all(|method| {
                    item.get(*method).is_none_or(|operation| {
                        operation["responses"]
                            .as_object()
                            .is_some_and(|responses| !responses.is_empty())
                    })
                })
            })
        })
    })
}

#[allow(dead_code)]
fn _method_from_str(method: &str) -> Method {
    Method::from_bytes(method.as_bytes()).expect("documented methods are valid HTTP methods")
}
