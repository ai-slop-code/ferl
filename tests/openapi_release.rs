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

use http::{HeaderMap, HeaderName, HeaderValue, Method, header};
use openapi_validation::{
    document, document_schema_dialects_supported, resolve_component_schema_in_document,
    resolve_schema_value_in_document, schema_roots, validates, validates_response,
};
use serde_json::Value;
use serving_process::{ServingProcess, assert_headers, json, send};
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
    assert!(
        validate_openapi_document(&doc, &validator).is_ok(),
        "normative OpenAPI document passes the single complete validation path"
    );

    let mut example_with_schema_property = doc.clone();
    example_with_schema_property["paths"]["/api/v1/links"]["post"]["requestBody"]["content"]["application/json"]
        ["examples"]["schemaPayload"] = serde_json::json!({"value":{"schema":{"$schema":"https://example.invalid/not-a-schema"},"$ref":"#/not-a-reference","$dynamicRef":"#/not-a-dynamic-reference"}});
    assert!(
        validate_openapi_document(&example_with_schema_property, &validator).is_ok(),
        "arbitrary example payload members are not mistaken for Schema Objects"
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
    assert!(
        validate_openapi_document(&invalid_oas, &validator).is_err(),
        "malformed variants use the same complete OpenAPI validation path"
    );

    let mut missing_responses = doc.clone();
    missing_responses["paths"]["/api/v1/me"]["get"]
        .as_object_mut()
        .unwrap()
        .remove("responses");
    assert!(
        validate_openapi_document(&missing_responses, &validator).is_err(),
        "OpenAPI's REQUIRED operation response map is enforced by the normative document gate"
    );

    let mut unsupported_dialect = doc.clone();
    unsupported_dialect["jsonSchemaDialect"] =
        serde_json::json!("https://example.invalid/schema/draft-07");
    assert!(
        !document_schema_dialects_supported(&unsupported_dialect),
        "an unsupported document dialect is not silently rewritten"
    );
    assert!(validate_openapi_document(&unsupported_dialect, &validator).is_err());
    assert!(
        resolve_schema_value_in_document(
            &unsupported_dialect,
            &unsupported_dialect["components"]["schemas"]["Account"]
        )
        .is_err(),
        "the schema resolver uses the same dialect rejection path"
    );
    let mut invalid_dialect_type = doc.clone();
    invalid_dialect_type["jsonSchemaDialect"] = serde_json::json!(17);
    assert!(
        !document_schema_dialects_supported(&invalid_dialect_type),
        "a non-string dialect value is rejected rather than treated as the default"
    );
    assert!(validate_openapi_document(&invalid_dialect_type, &validator).is_err());
    assert!(
        resolve_schema_value_in_document(
            &doc,
            &serde_json::json!({"$schema":"https://example.invalid/schema/draft-07","type":"string"})
        )
        .is_err(),
        "an unsupported schema-root dialect is not overwritten"
    );

    for (keyword, nested) in [
        (
            "properties",
            serde_json::json!({"nested":{"$schema":"https://example.invalid/schema/draft-07","type":"string"}}),
        ),
        (
            "$defs",
            serde_json::json!({"nested":{"$schema":17,"type":"string"}}),
        ),
        (
            "patternProperties",
            serde_json::json!({".*":{"$schema":"https://example.invalid/schema/draft-07","type":"string"}}),
        ),
        (
            "dependentSchemas",
            serde_json::json!({"enabled":{"$schema":false,"type":"object"}}),
        ),
    ] {
        let mut malformed = doc.clone();
        malformed["components"]["schemas"]["Account"][keyword] = nested;
        assert!(
            validate_openapi_document(&malformed, &validator).is_err(),
            "unsupported nested dialect under {keyword} is rejected before adaptation"
        );
        assert!(
            resolve_schema_value_in_document(
                &malformed,
                &malformed["components"]["schemas"]["Account"]
            )
            .is_err(),
            "public schema resolver enforces nested {keyword} dialects"
        );
    }

    let mut supplied_document = doc.clone();
    supplied_document["components"]["schemas"]["Role"] =
        serde_json::json!({"type":"string","enum":["admin"]});
    let resolved = resolve_schema_value_in_document(
        &supplied_document,
        &serde_json::json!({"$ref":"#/components/schemas/Role"}),
    )
    .expect("the resolver uses the supplied mutated document");
    let supplied_validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&resolved)
        .unwrap();
    assert!(
        !supplied_validator.is_valid(&serde_json::json!("user")),
        "a checked-in fallback schema cannot hide the supplied document mutation"
    );

    for (schema, expected) in [
        (serde_json::json!(true), true),
        (serde_json::json!(false), false),
    ] {
        let mut boolean_document = doc.clone();
        boolean_document["components"]["schemas"]["BooleanControl"] = schema.clone();
        let compiled = resolve_component_schema_in_document(&boolean_document, "BooleanControl")
            .expect("public component resolver accepts boolean Schema Objects");
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&compiled)
            .expect("boolean Schema Object compiles under Draft 2020-12");
        assert_eq!(
            validator.is_valid(&serde_json::json!({"value": 1})),
            expected,
            "boolean Schema Object semantics are preserved"
        );
    }

    let mut unresolved = doc.clone();
    unresolved["components"]["responses"]["Missing"] =
        serde_json::json!({"$ref":"#/components/responses/doesNotExist"});
    assert!(
        !document_references_resolve(&unresolved),
        "unresolved local references are rejected"
    );

    let mut unresolved_dynamic = doc.clone();
    unresolved_dynamic["components"]["schemas"]["Account"]["properties"]["role"]["$dynamicRef"] =
        serde_json::json!("#/components/schemas/Role");
    assert!(
        !document_schema_dialects_supported(&unresolved_dynamic),
        "even resolvable dynamic references are rejected because dynamic semantics are unsupported"
    );
    assert!(validate_openapi_document(&unresolved_dynamic, &validator).is_err());
    assert!(
        resolve_schema_value_in_document(
            &doc,
            &serde_json::json!({"$dynamicRef":"#/components/schemas/Role"})
        )
        .is_err(),
        "dynamic references are explicitly unsupported, not pointer-only validated"
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
                let (admin_id, mut admin_key) =
                    account_support::seed_account(database.pool(), "openapi_route_admin", "admin", true).await;
                let _ =
                    account_support::seed_anonymous_link(database.pool(), "inventory-code").await;
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
                    let expected_status = unauthenticated_route_status(method, template);
                    assert_eq!(
                        response.status, expected_status,
                        "{operation_id} owns {method} {path}; an unregistered route cannot pass as a documented 404"
                    );
                    let status = response.status.to_string();
                    let documented = doc["paths"][*template][method.to_ascii_lowercase()]["responses"]
                        .as_object()
                        .unwrap();
                    assert!(
                        documented.contains_key(&status),
                        "{operation_id} {method} {path} returned undocumented {status}; declared {:?}",
                        documented.keys().collect::<Vec<_>>()
                    );
                    let response_component = response_component(&doc, template, method, &status);
                    let received_headers = http_headers(&response.headers);
                    let bodyless_response = *method == "HEAD" || response.status == 302;
                    let body = if bodyless_response {
                        assert!(response.body.is_empty(), "bodyless responses have zero bytes");
                        None
                    } else {
                        Some(
                            serde_json::from_slice::<Value>(&response.body)
                                .unwrap_or_else(|error| {
                                    panic!("{operation_id} response is JSON: {error}")
                                }),
                        )
                    };
                    assert!(
                        validates_response(
                            &response_component,
                            &received_headers,
                            body.as_ref(),
                            bodyless_response,
                        ),
                        "{operation_id} actual status, response headers, and body conform to {response_component}"
                    );
                    if let Some(body) = body.as_ref()
                        && body.get("error").is_some()
                    {
                        assert_eq!(
                            response.headers["x-request-id"],
                            body["request_id"].as_str().unwrap_or_default(),
                            "{operation_id} error body and header share the same request ID"
                        );
                        assert!(validates("Error", body), "{operation_id} error schema");
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
                    let head_headers = http_headers(&head.headers);
                    assert_eq!(allow_set(&head.headers["allow"]), expected_allow(template),
                        "HEAD {path} returns the exact supported-method set");
                    assert!(validates_response(
                        "MethodNotAllowed",
                        &head_headers,
                        None,
                        true,
                    ));
                    let mut missing_cache_header = head_headers.clone();
                    missing_cache_header.remove(header::CACHE_CONTROL);
                    assert!(
                        !validates_response(
                            "MethodNotAllowed",
                            &missing_cache_header,
                            None,
                            true,
                        ),
                        "a missing received Cache-Control header fails the response gate"
                    );
                    let method_not_allowed = send(process.address, "PUT", &path, None, "")
                        .await
                        .unwrap_or_else(|error| panic!("PUT {path}: {error}"));
                    assert_eq!(method_not_allowed.status, 405, "PUT {path} is unsupported");
                    assert_headers(&method_not_allowed);
                    assert_eq!(allow_set(&method_not_allowed.headers["allow"]), expected_allow(template),
                        "PUT {path} returns the exact supported-method set");
                    let error: Value = serde_json::from_slice(&method_not_allowed.body).unwrap();
                    assert_eq!(error["error"]["code"], "method_not_allowed");
                    assert!(validates("Error", &error));
                    assert_eq!(
                        method_not_allowed.headers["x-request-id"],
                        error["request_id"]
                    );
                    let method_headers = http_headers(&method_not_allowed.headers);
                    assert!(validates_response(
                        "MethodNotAllowed",
                        &method_headers,
                        Some(&error),
                        false,
                    ));
                    assert!(expected_allow(template).contains(*method), "{path} Allow includes {method}");
                }

                // A plausible 401 from a different protected handler is not
                // route-ownership proof. Exercise each documented operation
                // with a valid authority and an operation-specific outcome.
                let account_path = format!("/api/v1/admin/accounts/{}", admin_id.as_uuid());
                for (operation_id, method, path, body, expected_status) in [
                    ("getCurrentAccount", "GET", "/api/v1/me", "", 200),
                    ("listAccounts", "GET", "/api/v1/admin/accounts", "", 200),
                    ("createAccount", "POST", "/api/v1/admin/accounts", r#"{"username":"openapi_route_user"}"#, 201),
                    ("getAccount", "GET", account_path.as_str(), "", 200),
                    ("updateAccount", "PATCH", account_path.as_str(), r#"{"enabled":true}"#, 200),
                    ("deleteAccount", "DELETE", account_path.as_str(), "", 409),
                    ("listLinks", "GET", "/api/v1/links", "", 200),
                    ("getLink", "GET", "/api/v1/links/inventory-code", "", 200),
                    ("rotateManagementToken", "POST", "/api/v1/links/inventory-code/management-token", "", 200),
                    ("listLinkVisits", "GET", "/api/v1/links/inventory-code/visits", "", 200),
                    ("getLinkVisitStats", "GET", "/api/v1/links/inventory-code/stats", "", 200),
                    ("getLinkVisitBreakdown", "GET", "/api/v1/links/inventory-code/stats/breakdown?group_by=day", "", 200),
                    ("listTags", "GET", "/api/v1/tags", "", 200),
                ] {
                    let response = send(process.address, method, path, Some(&admin_key), body)
                        .await
                        .unwrap_or_else(|error| panic!("authorized {operation_id} reaches its handler: {error}"));
                    assert_headers(&response);
                    assert_eq!(response.status, expected_status, "authorized {operation_id} has its operation-specific outcome");
                    if operation_id == "deleteAccount" {
                        assert_eq!(json(&response)["error"]["code"], "last_admin_required");
                    }
                    if operation_id == "getCurrentAccount" {
                        assert_eq!(json(&response)["id"], admin_id.as_uuid().to_string());
                    }
                }

                let created = send(
                    process.address,
                    "POST",
                    "/api/v1/links",
                    Some(&admin_key),
                    r#"{"destination_url":"https://route-owner.example/path","short_code":"inventory-owned"}"#,
                ).await.unwrap();
                assert_headers(&created);
                assert_eq!(created.status, 201, "createLink reaches its committed handler");
                let detail = send(process.address, "PATCH", "/api/v1/links/inventory-owned", Some(&admin_key), r#"{"title":"operation-specific"}"#).await.unwrap();
                assert_headers(&detail);
                assert_eq!(detail.status, 200);
                assert_eq!(json(&detail)["title"], "operation-specific");
                let deleted_link = send(process.address, "DELETE", "/api/v1/links/inventory-owned", Some(&admin_key), "").await.unwrap();
                assert_headers(&deleted_link);
                assert_eq!(deleted_link.status, 204);

                let reset = send(process.address, "POST", &format!("{account_path}/api-key"), Some(&admin_key), "").await.unwrap();
                assert_headers(&reset);
                assert_eq!(reset.status, 200, "resetAccountApiKey reaches the admin reset handler");
                admin_key = json(&reset)["api_key"].as_str().unwrap().to_owned();
                let rotated = send(process.address, "POST", "/api/v1/me/api-key", Some(&admin_key), "").await.unwrap();
                assert_headers(&rotated);
                assert_eq!(rotated.status, 200, "rotateCurrentApiKey reaches the account rotation handler");

                for (method, path, expected_status) in [
                    ("GET", "/api/health/live", 200),
                    ("GET", "/api/health/ready", 200),
                    ("GET", "/inventory-code", 302),
                    ("HEAD", "/inventory-code", 302),
                ] {
                    let response = send(process.address, method, path, None, "").await.unwrap();
                    assert_headers(&response);
                    assert_eq!(response.status, expected_status, "public/health operation {method} {path}");
                    if method == "HEAD" || expected_status == 302 {
                        assert!(response.body.is_empty(), "redirect and HEAD responses have zero body bytes");
                    }
                    if expected_status == 302 {
                        assert_eq!(response.headers["location"], "https://destination.example.test/path");
                    }
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

fn validate_openapi_document(
    doc: &Value,
    openapi_validator: &jsonschema::Validator,
) -> Result<(), String> {
    if doc["openapi"] != "3.1.0" {
        return Err("document is not OpenAPI 3.1.0".to_owned());
    }
    let violations = openapi_validator
        .iter_errors(doc)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !violations.is_empty() {
        return Err(format!(
            "pinned OpenAPI meta-schema violations: {violations:#?}"
        ));
    }
    if !all_operations_have_responses(doc) {
        return Err("every Operation Object must declare a nonempty responses map".to_owned());
    }
    if !document_schema_dialects_supported(doc) {
        return Err("document or nested Schema Object uses an unsupported dialect".to_owned());
    }
    if !document_references_resolve(doc) {
        return Err("document contains an unresolved or unsupported reference".to_owned());
    }
    for (index, schema) in schema_roots(doc).into_iter().enumerate() {
        let expanded = resolve_schema_value_in_document(doc, schema)
            .map_err(|error| format!("Schema Object root {index}: {error}"))?;
        jsonschema::options()
            .should_validate_formats(true)
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&expanded)
            .map_err(|error| format!("Schema Object root {index} is invalid: {error}"))?;
    }
    Ok(())
}

fn document_references_resolve(doc: &Value) -> bool {
    fn visit(value: &Value, root: &Value, seen: &mut BTreeSet<String>) -> bool {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref") {
                    let Some(reference) = reference.as_str() else {
                        return false;
                    };
                    if !reference.starts_with("#/")
                        || root.pointer(reference.trim_start_matches('#')).is_none()
                    {
                        return false;
                    }
                    seen.insert(reference.to_owned());
                }
                for (keyword, child) in object {
                    // Payloads and defaults are data, not OpenAPI Reference
                    // Objects or JSON Schema keywords.
                    if matches!(keyword.as_str(), "value" | "default" | "const" | "enum") {
                        continue;
                    }
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

fn expected_allow(path: &str) -> BTreeSet<String> {
    match path {
        "/api/v1/me" => method_set(&["GET"]),
        "/api/v1/me/api-key"
        | "/api/v1/admin/accounts/{id}/api-key"
        | "/api/v1/links/{code}/management-token" => method_set(&["POST"]),
        "/api/v1/admin/accounts" | "/api/v1/links" => method_set(&["GET", "POST"]),
        "/api/v1/admin/accounts/{id}" => method_set(&["DELETE", "GET", "PATCH"]),
        "/api/v1/links/{code}" => method_set(&["DELETE", "GET", "PATCH"]),
        "/api/v1/links/{code}/visits"
        | "/api/v1/links/{code}/stats"
        | "/api/v1/links/{code}/stats/breakdown"
        | "/api/v1/tags"
        | "/api/health/live"
        | "/api/health/ready" => method_set(&["GET"]),
        "/{short_code}" => method_set(&["GET", "HEAD"]),
        _ => panic!("reviewed route {path} has an explicit Allow set"),
    }
}

fn method_set(methods: &[&str]) -> BTreeSet<String> {
    methods.iter().map(|method| (*method).to_owned()).collect()
}

fn allow_set(value: &str) -> BTreeSet<String> {
    value.split(',').map(str::trim).map(str::to_owned).collect()
}

fn unauthenticated_route_status(method: &str, path: &str) -> u16 {
    match (method, path) {
        ("GET", "/api/health/live" | "/api/health/ready") => 200,
        ("POST", "/api/v1/links") => 403,
        (_, "/{short_code}") => 302,
        _ => 401,
    }
}

fn response_component(doc: &Value, path: &str, method: &str, status: &str) -> String {
    let reference = doc["paths"][path][method.to_ascii_lowercase()]["responses"][status]["$ref"]
        .as_str()
        .unwrap_or_else(|| panic!("{method} {path} {status} uses a referenced response"));
    reference
        .strip_prefix("#/components/responses/")
        .filter(|name| !name.contains('/'))
        .unwrap_or_else(|| panic!("response reference {reference} targets a response component"))
        .to_owned()
}

fn http_headers(headers: &BTreeMap<String, String>) -> HeaderMap {
    headers
        .iter()
        .map(|(name, value)| {
            (
                HeaderName::from_bytes(name.as_bytes()).expect("received header name is valid"),
                HeaderValue::from_str(value).expect("received header value is valid"),
            )
        })
        .collect()
}

#[allow(dead_code)]
fn _method_from_str(method: &str) -> Method {
    Method::from_bytes(method.as_bytes()).expect("documented methods are valid HTTP methods")
}
