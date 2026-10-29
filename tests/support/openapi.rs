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
    resolve_component_schema_in_document(&root, name)
        .unwrap_or_else(|error| panic!("OpenAPI schema {name}: {error}"))
}

/// Resolve an object or boolean component Schema Object using the dialect
/// declared by this supplied OpenAPI document.
#[allow(dead_code)]
pub fn resolve_component_schema_in_document(doc: &Value, name: &str) -> Result<Value, String> {
    let schema = doc["components"]["schemas"]
        .get(name)
        .ok_or_else(|| format!("component schema {name} does not exist"))?;
    resolve_schema_value_in_document(doc, schema)
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
    if !schema_dialect_supported(schema, document_dialect, doc, &mut Vec::new()) {
        return Err("schema declares an unsupported JSON Schema dialect".to_owned());
    }
    resolve_refs(schema, doc, &mut Vec::new())
        .map(adapt_to_draft_2020_12)
        .map_err(|error| match error {
            ReferenceError::DynamicReference => {
                "JSON Schema $dynamicRef semantics are not supported by this validator".to_owned()
            }
            ReferenceError::Unresolved => {
                "schema contains an unresolved, external, or cyclic $ref".to_owned()
            }
        })
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

    schema_roots(doc)
        .into_iter()
        .all(|schema| schema_dialect_supported(schema, document_dialect, doc, &mut Vec::new()))
}

/// Return only Schema Objects at OpenAPI-defined locations, not arbitrary
/// example payloads that happen to contain a member named `schema`.
#[allow(dead_code)]
pub fn schema_roots(doc: &Value) -> Vec<&Value> {
    let mut roots = Vec::new();
    if let Some(schemas) = doc["components"]["schemas"].as_object() {
        roots.extend(schemas.values());
    }
    if let Some(components) = doc.get("components") {
        collect_component_schema_roots(components, &mut roots);
    }
    collect_path_item_schema_roots(&doc["paths"], &mut roots);
    collect_path_item_schema_roots(&doc["webhooks"], &mut roots);
    roots
}

fn collect_component_schema_roots<'a>(components: &'a Value, roots: &mut Vec<&'a Value>) {
    for kind in ["parameters", "headers"] {
        if let Some(entries) = components[kind].as_object() {
            for entry in entries.values() {
                collect_parameter_or_header_schema(entry, roots);
            }
        }
    }
    if let Some(entries) = components["requestBodies"].as_object() {
        for entry in entries.values() {
            collect_content_schema_roots(&entry["content"], roots);
        }
    }
    if let Some(entries) = components["responses"].as_object() {
        for entry in entries.values() {
            collect_response_schema_roots(entry, roots);
        }
    }
    if let Some(callbacks) = components["callbacks"].as_object() {
        for callback in callbacks.values() {
            if let Some(expressions) = callback.as_object() {
                for path_item in expressions.values() {
                    collect_path_item_schema_roots(path_item, roots);
                }
            }
        }
    }
}

fn collect_path_item_schema_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
    let items = match value {
        Value::Object(object) => object.values().collect::<Vec<_>>(),
        Value::Array(items) => items.iter().collect(),
        _ => return,
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        if let Some(parameters) = object.get("parameters").and_then(Value::as_array) {
            for parameter in parameters {
                collect_parameter_or_header_schema(parameter, roots);
            }
        }
        for (name, operation) in object {
            if !matches!(
                name.as_str(),
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
            ) {
                continue;
            }
            let Some(operation) = operation.as_object() else {
                continue;
            };
            if let Some(parameters) = operation.get("parameters").and_then(Value::as_array) {
                for parameter in parameters {
                    collect_parameter_or_header_schema(parameter, roots);
                }
            }
            if let Some(content) = operation
                .get("requestBody")
                .and_then(|body| body.get("content"))
            {
                collect_content_schema_roots(content, roots);
            }
            if let Some(responses) = operation.get("responses") {
                collect_response_schema_roots(responses, roots);
            }
            if let Some(callbacks) = operation.get("callbacks").and_then(Value::as_object) {
                for callback in callbacks.values() {
                    if let Some(expressions) = callback.as_object() {
                        for path_item in expressions.values() {
                            collect_path_item_schema_roots(path_item, roots);
                        }
                    }
                }
            }
        }
    }
}

fn collect_parameter_or_header_schema<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
    if let Some(schema) = value.get("schema") {
        roots.push(schema);
    }
    if let Some(content) = value.get("content") {
        collect_content_schema_roots(content, roots);
    }
}

fn collect_response_schema_roots<'a>(value: &'a Value, roots: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object)
            if object.contains_key("content") || object.contains_key("headers") =>
        {
            collect_content_schema_roots(&value["content"], roots);
            if let Some(headers) = value["headers"].as_object() {
                for header in headers.values() {
                    collect_parameter_or_header_schema(header, roots);
                }
            }
        }
        Value::Object(object) => {
            for response in object.values() {
                collect_response_schema_roots(response, roots);
            }
        }
        Value::Array(items) => {
            for response in items {
                collect_response_schema_roots(response, roots);
            }
        }
        _ => {}
    }
}

fn collect_content_schema_roots<'a>(content: &'a Value, roots: &mut Vec<&'a Value>) {
    if let Some(media_types) = content.as_object() {
        for media_type in media_types.values() {
            if let Some(schema) = media_type.get("schema") {
                roots.push(schema);
            }
        }
    }
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

fn schema_dialect_supported(
    schema: &Value,
    inherited: &str,
    root: &Value,
    references: &mut Vec<String>,
) -> bool {
    const SINGLE_SCHEMA_KEYWORDS: &[&str] = &[
        "additionalItems",
        "additionalProperties",
        "contains",
        "contentSchema",
        "items",
        "not",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
        "if",
        "then",
        "else",
    ];
    const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
    const SCHEMA_MAP_KEYWORDS: &[&str] = &[
        "$defs",
        "definitions",
        "dependentSchemas",
        "patternProperties",
        "properties",
    ];

    let Value::Object(object) = schema else {
        return schema.is_boolean();
    };
    let effective = match object.get("$schema") {
        None => inherited,
        Some(Value::String(dialect))
            if dialect == OAS_31_BASE_DIALECT || dialect == DRAFT_2020_12_DIALECT =>
        {
            dialect
        }
        Some(_) => return false,
    };
    // This small gate does not implement dynamic scope/anchors. Reject the
    // keyword even when its text happens to name an existing JSON pointer.
    if object.contains_key("$dynamicRef") {
        return false;
    }
    // A reference target is another Schema Object whose dialect must be checked
    // before resolve_refs/adapt_to_draft_2020_12 can rewrite its declaration.
    // Leave malformed, external, unresolved, and cyclic reference diagnostics to
    // resolve_refs, which already reports those limitations precisely.
    if let Some(reference) = object.get("$ref").and_then(Value::as_str)
        && reference.starts_with("#/")
        && !references.iter().any(|entry| entry == reference)
        && let Some(target) = root.pointer(reference.trim_start_matches('#'))
    {
        references.push(reference.to_owned());
        let supported = schema_dialect_supported(target, effective, root, references);
        references.pop();
        if !supported {
            return false;
        }
    }
    for keyword in SINGLE_SCHEMA_KEYWORDS {
        if let Some(nested) = object.get(*keyword)
            && !schema_dialect_supported(nested, effective, root, references)
        {
            return false;
        }
    }
    for keyword in SCHEMA_ARRAY_KEYWORDS {
        if let Some(values) = object.get(*keyword) {
            let Some(values) = values.as_array() else {
                return false;
            };
            if !values
                .iter()
                .all(|nested| schema_dialect_supported(nested, effective, root, references))
            {
                return false;
            }
        }
    }
    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(entries) = object.get(*keyword) {
            let Some(entries) = entries.as_object() else {
                return false;
            };
            if !entries
                .values()
                .all(|nested| schema_dialect_supported(nested, effective, root, references))
            {
                return false;
            }
        }
    }
    if let Some(entries) = object.get("dependencies") {
        let Some(entries) = entries.as_object() else {
            return false;
        };
        if !entries.values().all(|nested| {
            !nested.is_object() || schema_dialect_supported(nested, effective, root, references)
        }) {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy)]
enum ReferenceError {
    Unresolved,
    DynamicReference,
}

/// Resolve one OpenAPI Schema Object using JSON Schema 2020-12 `$ref`
/// semantics. `$dynamicRef` is rejected rather than reduced to pointer
/// existence, which would not implement dynamic scope/anchor resolution.
fn resolve_refs(
    schema: &Value,
    root: &Value,
    stack: &mut Vec<String>,
) -> Result<Value, ReferenceError> {
    match schema {
        Value::Object(object) => {
            if object.contains_key("$dynamicRef") {
                return Err(ReferenceError::DynamicReference);
            }
            if let Some(reference_value) = object.get("$ref") {
                let Some(reference) = reference_value.as_str() else {
                    return Err(ReferenceError::Unresolved);
                };
                if !reference.starts_with("#/") || stack.iter().any(|entry| entry == reference) {
                    return Err(ReferenceError::Unresolved);
                }
                let target = root
                    .pointer(reference.trim_start_matches('#'))
                    .ok_or(ReferenceError::Unresolved)?;
                stack.push(reference.to_owned());
                let target = resolve_refs(target, root, stack)?;
                stack.pop();
                let siblings = resolve_schema_children(object, root, stack, true)?;
                if siblings.is_empty() {
                    Ok(target)
                } else {
                    Ok(json!({"allOf": [target, Value::Object(siblings)]}))
                }
            } else {
                resolve_schema_children(object, root, stack, false).map(Value::Object)
            }
        }
        other => Ok(other.clone()),
    }
}

/// Resolve references only in JSON Schema positions. Values beneath annotations
/// such as `const`, `default`, and `examples` are instance data even when their
/// object members resemble JSON Schema keywords.
fn resolve_schema_children(
    object: &serde_json::Map<String, Value>,
    root: &Value,
    stack: &mut Vec<String>,
    skip_ref: bool,
) -> Result<serde_json::Map<String, Value>, ReferenceError> {
    const SINGLE_SCHEMA_KEYWORDS: &[&str] = &[
        "additionalItems",
        "additionalProperties",
        "contains",
        "contentSchema",
        "items",
        "not",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
        "if",
        "then",
        "else",
    ];
    const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
    const SCHEMA_MAP_KEYWORDS: &[&str] = &[
        "$defs",
        "definitions",
        "dependentSchemas",
        "patternProperties",
        "properties",
    ];

    object
        .iter()
        .filter(|(key, _)| !skip_ref || key.as_str() != "$ref")
        .map(|(key, value)| {
            let resolved = if SINGLE_SCHEMA_KEYWORDS.contains(&key.as_str()) {
                resolve_refs(value, root, stack)?
            } else if SCHEMA_ARRAY_KEYWORDS.contains(&key.as_str()) {
                match value.as_array() {
                    Some(values) => Value::Array(
                        values
                            .iter()
                            .map(|nested| resolve_refs(nested, root, stack))
                            .collect::<Result<_, _>>()?,
                    ),
                    None => value.clone(),
                }
            } else if SCHEMA_MAP_KEYWORDS.contains(&key.as_str()) {
                match value.as_object() {
                    Some(entries) => Value::Object(
                        entries
                            .iter()
                            .map(|(name, nested)| {
                                Ok((name.clone(), resolve_refs(nested, root, stack)?))
                            })
                            .collect::<Result<_, ReferenceError>>()?,
                    ),
                    None => value.clone(),
                }
            } else if key == "dependencies" {
                match value.as_object() {
                    Some(entries) => Value::Object(
                        entries
                            .iter()
                            .map(|(name, nested)| {
                                let resolved = if nested.is_object() || nested.is_boolean() {
                                    resolve_refs(nested, root, stack)?
                                } else {
                                    nested.clone()
                                };
                                Ok((name.clone(), resolved))
                            })
                            .collect::<Result<_, ReferenceError>>()?,
                    ),
                    None => value.clone(),
                }
            } else {
                value.clone()
            };
            Ok((key.clone(), resolved))
        })
        .collect()
}

fn adapt_to_draft_2020_12(schema: Value) -> Value {
    const SINGLE_SCHEMA_KEYWORDS: &[&str] = &[
        "additionalItems",
        "additionalProperties",
        "contains",
        "contentSchema",
        "items",
        "not",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
        "if",
        "then",
        "else",
    ];
    const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
    const SCHEMA_MAP_KEYWORDS: &[&str] = &[
        "$defs",
        "definitions",
        "dependentSchemas",
        "patternProperties",
        "properties",
    ];
    match schema {
        Value::Object(mut object) => {
            for keyword in SINGLE_SCHEMA_KEYWORDS {
                if let Some(nested) = object.get_mut(*keyword) {
                    *nested = adapt_to_draft_2020_12(nested.take());
                }
            }
            for keyword in SCHEMA_ARRAY_KEYWORDS {
                if let Some(values) = object.get_mut(*keyword).and_then(Value::as_array_mut) {
                    for nested in values {
                        *nested = adapt_to_draft_2020_12(nested.take());
                    }
                }
            }
            for keyword in SCHEMA_MAP_KEYWORDS {
                if let Some(entries) = object.get_mut(*keyword).and_then(Value::as_object_mut) {
                    for nested in entries.values_mut() {
                        *nested = adapt_to_draft_2020_12(nested.take());
                    }
                }
            }
            if let Some(entries) = object
                .get_mut("dependencies")
                .and_then(Value::as_object_mut)
            {
                for nested in entries.values_mut().filter(|value| value.is_object()) {
                    *nested = adapt_to_draft_2020_12(nested.take());
                }
            }
            object.insert("$schema".to_owned(), json!(DRAFT_2020_12_DIALECT));
            Value::Object(object)
        }
        boolean_schema => boolean_schema,
    }
}

#[allow(dead_code)]
pub fn validates(schema_name: &str, value: &Value) -> bool {
    validates_schema(&resolve_schema(schema_name), value)
}

pub fn validates_ref_with_siblings(schema: &Value, value: &Value) -> bool {
    let root = document();
    let Ok(resolved) = resolve_schema_value_in_document(&root, schema) else {
        return false;
    };
    validates_schema(&resolved, value)
}

fn validates_schema(schema: &Value, value: &Value) -> bool {
    jsonschema::options()
        .should_validate_formats(true)
        .with_draft(jsonschema::Draft::Draft202012)
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
