use serde_json::{Value, json};

pub fn document() -> Value {
    let yaml: yaml_serde::Value = yaml_serde::from_str(include_str!("../../spec/openapi.yaml"))
        .expect("checked-in OpenAPI YAML parses");
    serde_json::to_value(yaml).expect("OpenAPI YAML converts to JSON values")
}

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
