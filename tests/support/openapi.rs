use serde_json::{Value, json};

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

/// OpenAPI 3.1 inherits JSON Schema 2020-12 reference semantics: `$ref` and
/// sibling assertions are conjunctive, not object-merge overrides.
fn resolve_refs(schema: &Value, root: &Value, stack: &mut Vec<String>) -> Result<Value, ()> {
    match schema {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                if !reference.starts_with("#/components/schemas/")
                    || stack.iter().any(|entry| entry == reference)
                {
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

fn with_draft(mut schema: Value) -> Value {
    schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    schema
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
