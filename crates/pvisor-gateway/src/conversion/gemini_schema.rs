//! Gemini's supported schema subset, shared by production request renderers.

use serde_json::{Map, Value, json};

pub(super) fn normalize_gemini_schema(schema: &Value) -> Value {
    let mut output = schema.clone();
    let definitions = take_definitions(&mut output);
    inline_refs(&mut output, &definitions, &mut Vec::new());
    clean_schema_node(&mut output);
    output
}

fn take_definitions(root: &mut Value) -> Map<String, Value> {
    let mut definitions = Map::new();
    if let Value::Object(object) = root {
        for key in ["$defs", "definitions"] {
            if let Some(Value::Object(values)) = object.remove(key) {
                definitions.extend(values);
            }
        }
    }
    definitions
}

fn for_each_child_schema(object: &mut Map<String, Value>, mut visit: impl FnMut(&mut Value)) {
    if let Some(items) = object.get_mut("items") {
        visit(items);
    }
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        properties.values_mut().for_each(&mut visit);
    }
    for key in ["anyOf", "allOf"] {
        if let Some(Value::Array(values)) = object.get_mut(key) {
            values.iter_mut().for_each(&mut visit);
        }
    }
}

fn inline_refs(value: &mut Value, definitions: &Map<String, Value>, chain: &mut Vec<String>) {
    let reference_name = value.get("$ref").and_then(Value::as_str).map(|reference| {
        reference
            .rsplit('/')
            .next()
            .unwrap_or(reference)
            .to_string()
    });
    if let Some(name) = reference_name {
        if chain.contains(&name) {
            return;
        }
        if let Some(definition) = definitions.get(&name) {
            let mut resolved = definition.clone();
            if let (Value::Object(resolved), Value::Object(original)) = (&mut resolved, &*value) {
                for (key, child) in original {
                    if key != "$ref" {
                        resolved.insert(key.clone(), child.clone());
                    }
                }
            }
            chain.push(name);
            inline_refs(&mut resolved, definitions, chain);
            chain.pop();
            *value = resolved;
        }
        return;
    }
    match value {
        Value::Object(object) => {
            object.remove("$defs");
            object.remove("definitions");
            for_each_child_schema(object, |child| inline_refs(child, definitions, chain));
        }
        Value::Array(array) => array
            .iter_mut()
            .for_each(|child| inline_refs(child, definitions, chain)),
        _ => {}
    }
}

fn rewrite_schema_structure(object: &mut Map<String, Value>) -> bool {
    let mut changed = false;

    if let Some(Value::Array(members)) = object.remove("allOf") {
        changed = true;
        for member in members {
            let Value::Object(member) = member else {
                continue;
            };
            for (key, value) in member {
                if key == "properties"
                    && let Value::Object(ref source) = value
                    && let Value::Object(target) =
                        object.entry("properties").or_insert_with(|| json!({}))
                {
                    for (name, schema) in source {
                        target.entry(name).or_insert(schema.clone());
                    }
                } else if key == "required"
                    && let Value::Array(ref source) = value
                    && let Value::Array(target) =
                        object.entry("required").or_insert_with(|| json!([]))
                {
                    for required in source {
                        if !target.contains(required) {
                            target.push(required.clone());
                        }
                    }
                } else {
                    object.entry(key).or_insert(value);
                }
            }
        }
    }

    if let Some(constant) = object.remove("const") {
        changed = true;
        let schema_type = schema_type_for_constant(&constant);
        object.insert("enum".into(), Value::Array(vec![constant]));
        object.entry("type").or_insert_with(|| json!(schema_type));
    }

    if let Some(Value::Array(types)) = object.get("type").cloned() {
        changed = true;
        let names = types
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        let nullable = names.iter().any(|name| name == "null");
        let non_null = names
            .into_iter()
            .filter(|name| name != "null")
            .collect::<Vec<_>>();
        object.remove("type");
        if nullable {
            object.insert("nullable".into(), Value::Bool(true));
        }
        match non_null.as_slice() {
            [] => {}
            [only] => {
                object.insert("type".into(), json!(only));
            }
            many => {
                object.insert(
                    "anyOf".into(),
                    Value::Array(many.iter().map(|kind| json!({"type":kind})).collect()),
                );
            }
        }
    }

    let collapsible_any_of = object
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|members| {
            members.len() == 1
                || members
                    .iter()
                    .any(|member| member.get("type").and_then(Value::as_str) == Some("null"))
        });
    if collapsible_any_of && let Some(Value::Array(members)) = object.remove("anyOf") {
        changed = true;
        let nullable = members
            .iter()
            .any(|member| member.get("type").and_then(Value::as_str) == Some("null"));
        let non_null = members
            .into_iter()
            .filter(|member| member.get("type").and_then(Value::as_str) != Some("null"))
            .collect::<Vec<_>>();
        if nullable {
            object.insert("nullable".into(), Value::Bool(true));
        }
        match non_null.len() {
            0 => {}
            1 => {
                if let Some(Value::Object(member)) = non_null.into_iter().next() {
                    for (key, value) in member {
                        object.entry(key).or_insert(value);
                    }
                }
            }
            _ => {
                object.insert("anyOf".into(), Value::Array(non_null));
            }
        }
    }
    changed
}

fn schema_type_for_constant(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::Array(_) => "array",
        _ => "object",
    }
}

fn clean_schema_node(value: &mut Value) {
    const ALLOWED: &[&str] = &[
        "type",
        "format",
        "description",
        "title",
        "nullable",
        "enum",
        "items",
        "properties",
        "required",
        "anyOf",
        "default",
        "minLength",
        "maxLength",
        "pattern",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "propertyOrdering",
    ];
    let object = match value {
        Value::Object(object) => object,
        Value::Array(values) => {
            values.iter_mut().for_each(clean_schema_node);
            return;
        }
        _ => return,
    };

    let mut guard = 0;
    while rewrite_schema_structure(object) && guard < 64 {
        guard += 1;
    }

    if object.get("type").and_then(Value::as_str) == Some("array") && !object.contains_key("items")
    {
        object.insert("items".into(), json!({"type":"object"}));
    }
    if object.contains_key("enum") {
        match object.get("type").and_then(Value::as_str) {
            Some("string") => {}
            Some(_) => {
                object.remove("enum");
            }
            None => {
                object.insert("type".into(), json!("string"));
            }
        }
    }
    object.remove("additionalProperties");
    if !object.contains_key("type") && !object.contains_key("anyOf") && !object.contains_key("enum")
    {
        object.insert("type".into(), json!("object"));
    }
    if object
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| format != "enum" && format != "date-time")
    {
        object.remove("format");
    }
    for_each_child_schema(object, clean_schema_node);
    object.retain(|key, _| ALLOWED.contains(&key.as_str()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_normalization_flattens_and_makes_nullable() {
        let schema = json!({
            "allOf":[
                {"type":"object","properties":{"name":{"type":"string"}},"required":["name"]},
                {"properties":{"age":{"type":["integer","null"]}},"required":["age"]}
            ],
            "additionalProperties":false
        });
        let normalized = normalize_gemini_schema(&schema);
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["properties"]["name"]["type"], "string");
        assert_eq!(normalized["properties"]["age"]["type"], "integer");
        assert_eq!(normalized["properties"]["age"]["nullable"], true);
        assert_eq!(normalized["required"], json!(["name", "age"]));
        assert!(normalized.get("allOf").is_none());
        assert!(normalized.get("additionalProperties").is_none());
    }
}
