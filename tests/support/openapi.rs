use serde_json::{Value, json};

const OAS_31_BASE_DIALECT: &str = "https://spec.openapis.org/oas/3.1/dialect/base";
const DRAFT_2020_12_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

pub fn document() -> Value {
    let yaml: yaml_serde::Value = yaml_serde::from_str(include_str!("../../spec/openapi.yaml"))
        .expect("checked-in OpenAPI YAML parses");
    serde_json::to_value(yaml).expect("OpenAPI YAML converts to JSON values")
}

#[allow(dead_code)]
pub fn resolve_schema(name: &str) -> Value {
    let root = document();
    let schema = root["components"]["schemas"][name]
        .as_object()
        .unwrap_or_else(|| panic!("OpenAPI schema {name} exists"));
    let expanded = resolve_refs(&Value::Object(schema.clone()), &root, &mut Vec::new())
        .unwrap_or_else(|_| panic!("OpenAPI schema {name} has resolvable local references"));
    with_draft(expanded)
}

/// Resolve one OpenAPI Schema Object using JSON Schema 2020-12 `$ref`
/// semantics. Sibling keywords are preserved conjunctively as `allOf`.
#[allow(dead_code)]
pub fn resolve_schema_value(schema: &Value) -> Result<Value, String> {
    let root = document();
    resolve_schema_value_in_document(&root, schema)
}

#[allow(dead_code)]
pub fn resolve_schema_value_in_document(doc: &Value, schema: &Value) -> Result<Value, String> {
    let document_dialect = document_schema_dialect(doc)?;
    if !schema_dialect_supported(schema, document_dialect) {
        return Err("schema declares an unsupported JSON Schema dialect".to_owned());
    }
    resolve_refs(schema, doc, &mut Vec::new())
        .map(with_draft)
        .map_err(|()| "schema contains an unresolved, external, or cyclic reference".to_owned())
}

/// Confirm the effective OpenAPI 3.1 dialect before adapting its JSON Schema
/// keywords to the pinned Draft 2020-12 validator. The OpenAPI base dialect is
/// accepted because its validation vocabulary is Draft 2020-12; custom dialects
/// are rejected rather than silently overwritten.
#[allow(dead_code)]
pub fn document_schema_dialects_supported(doc: &Value) -> bool {
    let Ok(document_dialect) = document_schema_dialect(doc) else {
        return false;
    };

    let mut roots = Vec::new();
    if let Some(schemas) = doc["components"]["schemas"].as_object() {
        roots.extend(schemas.values());
    }
    collect_schema_roots(&doc["paths"], &mut roots);
    collect_schema_roots(&doc["webhooks"], &mut roots);
    if let Some(components) = doc["components"].as_object() {
        for (name, component) in components {
            if name != "schemas" {
                collect_schema_roots(component, &mut roots);
            }
        }
    }

    roots
        .into_iter()
        .all(|schema| schema_dialect_supported(schema, document_dialect))
}

fn document_schema_dialect(doc: &Value) -> Result<&'static str, String> {
    match doc.get("jsonSchemaDialect") {
        None => Ok(OAS_31_BASE_DIALECT),
        Some(Value::String(dialect)) if dialect == OAS_31_BASE_DIALECT => Ok(OAS_31_BASE_DIALECT),
        Some(Value::String(dialect)) if dialect == DRAFT_2020_12_DIALECT => {
            Ok(DRAFT_2020_12_DIALECT)
        }
        Some(Value::String(_)) => {
            Err("document declares an unsupported JSON Schema dialect".to_owned())
        }
        Some(_) => Err("document declares an invalid JSON Schema dialect value".to_owned()),
    }
}

#[allow(dead_code)]
fn collect_schema_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object) => {
            for (key, nested) in object {
                if key == "schema" {
                    roots.push(nested);
                } else {
                    collect_schema_roots(nested, roots);
                }
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_schema_roots(nested, roots);
            }
        }
        _ => {}
    }
}

fn schema_dialect_supported(schema: &Value, inherited: &str) -> bool {
    const NESTED_SCHEMA_KEYWORDS: &[&str] = &[
        "$defs",
        "additionalItems",
        "additionalProperties",
        "allOf",
        "anyOf",
        "contains",
        "contentSchema",
        "definitions",
        "dependencies",
        "dependentSchemas",
        "else",
        "if",
        "items",
        "not",
        "oneOf",
        "patternProperties",
        "prefixItems",
        "properties",
        "propertyNames",
        "then",
        "unevaluatedItems",
        "unevaluatedProperties",
    ];

    match schema {
        Value::Object(object) => {
            let effective = match object.get("$schema") {
                None => inherited,
                Some(Value::String(dialect))
                    if dialect == OAS_31_BASE_DIALECT || dialect == DRAFT_2020_12_DIALECT =>
                {
                    dialect
                }
                Some(_) => return false,
            };
            NESTED_SCHEMA_KEYWORDS.iter().all(|keyword| {
                object
                    .get(*keyword)
                    .is_none_or(|nested| schema_dialect_supported(nested, effective))
            })
        }
        Value::Array(values) => values
            .iter()
            .all(|nested| schema_dialect_supported(nested, inherited)),
        Value::Bool(_) => true,
        _ => true,
    }
}

/// OpenAPI 3.1 inherits JSON Schema 2020-12 reference semantics: `$ref` and
/// sibling assertions are conjunctive, not object-merge overrides.
fn resolve_refs(schema: &Value, root: &Value, stack: &mut Vec<String>) -> Result<Value, ()> {
    match schema {
        Value::Object(object) => {
            if let Some(dynamic_reference) = object.get("$dynamicRef") {
                let Some(reference) = dynamic_reference.as_str() else {
                    return Err(());
                };
                if !reference.starts_with("#/")
                    || root.pointer(reference.trim_start_matches('#')).is_none()
                {
                    return Err(());
                }
            }
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if !reference.starts_with("#/") || stack.iter().any(|entry| entry == reference) {
                    return Err(());
                }
                let target = root.pointer(reference.trim_start_matches('#')).ok_or(())?;
                stack.push(reference.to_owned());
                let target = resolve_refs(target, root, stack)?;
                stack.pop();
                let siblings = object
                    .iter()
                    .filter(|(key, _)| key.as_str() != "$ref")
                    .map(|(key, value)| Ok((key.clone(), resolve_refs(value, root, stack)?)))
                    .collect::<Result<serde_json::Map<_, _>, ()>>()?;
                if siblings.is_empty() {
                    Ok(target)
                } else {
                    Ok(json!({"allOf": [target, Value::Object(siblings)]}))
                }
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

#[allow(dead_code)]
pub fn validates(schema_name: &str, value: &Value) -> bool {
    validates_schema(&resolve_schema(schema_name), value)
}

pub fn validates_ref_with_siblings(schema: &Value, value: &Value) -> bool {
    let root = document();
    let resolved = resolve_refs(schema, &root, &mut Vec::new())
        .expect("OpenAPI schema references and siblings resolve");
    validates_schema(&with_draft(resolved), value)
}

fn with_draft(schema: Value) -> Value {
    match schema {
        Value::Object(mut object) => {
            object.insert("$schema".to_owned(), json!(DRAFT_2020_12_DIALECT));
            Value::Object(object)
        }
        // Boolean Schema Objects are valid in OpenAPI 3.1. Preserve their
        // allow-all/deny-all semantics while declaring the validator dialect.
        boolean_schema => json!({
            "$schema": DRAFT_2020_12_DIALECT,
            "allOf": [boolean_schema]
        }),
    }
}

fn validates_schema(schema: &Value, value: &Value) -> bool {
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .expect("OpenAPI schema is valid Draft 2020-12 JSON Schema")
        .is_valid(value)
}

/// Validate the mandatory normative response headers and checked-in body schema.
/// Header integer values are decoded from their HTTP wire representation.
#[allow(dead_code)]
pub fn validates_response(
    component: &str,
    headers: &http::HeaderMap,
    body: Option<&Value>,
    head: bool,
) -> bool {
    let root = document();
    let response = &root["components"]["responses"][component];
    if !validates_headers(component, headers) {
        return false;
    }
    if head {
        return body.is_none();
    }
    body.is_some_and(|body| {
        validates_ref_with_siblings(&response["content"]["application/json"]["schema"], body)
    })
}

/// Validate only the headers declared by an OpenAPI response, useful for
/// bodyless 204 operations whose schemas intentionally have no content section.
pub fn validates_headers(component: &str, headers: &http::HeaderMap) -> bool {
    let root = document();
    let response = &root["components"]["responses"][component];
    let Some(expected) = response["headers"].as_object() else {
        return false;
    };
    for (name, specification) in expected {
        let Some(reference) = specification["$ref"].as_str() else {
            return false;
        };
        let Some(specification) = root.pointer(reference.trim_start_matches('#')) else {
            return false;
        };
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            return false;
        };
        let value = if specification["schema"]["type"] == "integer" {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            let Ok(value) = value.parse::<u64>() else {
                return false;
            };
            json!(value)
        } else {
            json!(value)
        };
        if !validates_ref_with_siblings(&specification["schema"], &value) {
            return false;
        }
    }
    true
}
