//! Parameter coercion and JSON Schema validation.

use crate::error::{Result, StpError};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Coerce a raw string value to the type declared by a property schema.
///
/// `prop` is the JSON Schema object for the parameter (may be `Null` when the param isn't in
/// the schema — then the value is kept as a string).
pub fn coerce(prop: Option<&Value>, raw: &str) -> std::result::Result<Value, String> {
    let ty = prop
        .and_then(|p| p.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or("string");
    match ty {
        "integer" => raw
            .trim()
            .parse::<i64>()
            .map(|n| Value::Number(n.into()))
            .map_err(|_| format!("expected an integer, got {raw:?}")),
        "number" => raw
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| format!("expected a number, got {raw:?}")),
        "boolean" => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(format!("expected a boolean, got {raw:?}")),
        },
        "array" | "object" => {
            serde_json::from_str(raw).map_err(|e| format!("expected JSON for an {ty} value: {e}"))
        }
        // "string" and unknown types: keep verbatim.
        _ => Ok(Value::String(raw.to_string())),
    }
}

/// Parse an explicit raw-JSON value (the `:=` / `--set k:=v` form).
pub fn parse_json_value(raw: &str) -> std::result::Result<Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))
}

/// Property names declared by a parameter schema.
pub fn known_properties(parameter_schema: &Value) -> BTreeSet<String> {
    parameter_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Reject keys not present in the schema (typo guard). Returns the offending names.
pub fn unknown_keys(parameter_schema: &Value, params: &Map<String, Value>) -> Vec<String> {
    let known = known_properties(parameter_schema);
    if known.is_empty() {
        // Schema declares no properties — can't meaningfully judge; allow.
        return Vec::new();
    }
    params
        .keys()
        .filter(|k| !known.contains(*k))
        .cloned()
        .collect()
}

/// Validate a full parameters object against `parameter_schema`. Reports **all** failures,
/// one per line, as `path: message`. `x-*` keywords are non-standard and ignored.
pub fn validate(parameter_schema: &Value, params: &Value) -> Result<()> {
    // An empty/absent schema validates everything.
    if !parameter_schema.is_object()
        || parameter_schema
            .as_object()
            .map(|o| o.is_empty())
            .unwrap_or(true)
    {
        return Ok(());
    }
    let validator = jsonschema::validator_for(parameter_schema)
        .map_err(|e| StpError::runtime(format!("invalid parameter_schema: {e}")))?;

    let mut msgs: Vec<String> = Vec::new();
    for err in validator.iter_errors(params) {
        let path = err.instance_path.to_string();
        let loc = path.trim_start_matches('/').replace('/', ".");
        let msg = friendly(&err);
        if loc.is_empty() {
            msgs.push(format!("  {msg}"));
        } else {
            msgs.push(format!("  {loc}: {msg}"));
        }
    }
    if msgs.is_empty() {
        Ok(())
    } else {
        msgs.sort();
        msgs.dedup();
        Err(StpError::Validation(msgs.join("\n")))
    }
}

/// Turn a jsonschema error into a CLI-friendly line. Range/enum/type messages already read
/// well (`999 is greater than the maximum of 20`); only the required-property phrasing is raw
/// JSON-Schema-ese, so we rewrite `"prompt" is a required property` → `prompt: required`.
fn friendly(err: &jsonschema::ValidationError) -> String {
    match &err.kind {
        jsonschema::error::ValidationErrorKind::Required { property } => {
            format!("{}: required", property.as_str().unwrap_or("value"))
        }
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coercion_by_type() {
        let int = json!({"type": "integer"});
        assert_eq!(coerce(Some(&int), "-1").unwrap(), json!(-1));
        assert!(coerce(Some(&int), "1.5").is_err());

        let num = json!({"type": "number"});
        assert_eq!(coerce(Some(&num), "1.5").unwrap(), json!(1.5));

        let b = json!({"type": "boolean"});
        assert_eq!(coerce(Some(&b), "YES").unwrap(), json!(true));
        assert_eq!(coerce(Some(&b), "0").unwrap(), json!(false));

        let s = json!({"type": "string"});
        assert_eq!(coerce(Some(&s), "").unwrap(), json!(""));
        assert_eq!(coerce(Some(&s), "a=b").unwrap(), json!("a=b"));

        let arr = json!({"type": "array"});
        assert_eq!(coerce(Some(&arr), "[1,2]").unwrap(), json!([1, 2]));
        assert!(coerce(Some(&arr), "notjson").is_err());

        // No schema → string.
        assert_eq!(coerce(None, "hello").unwrap(), json!("hello"));
    }

    #[test]
    fn validation_reports_all_failures() {
        let schema = json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {"type": "string"},
                "steps": {"type": "integer", "minimum": 1, "maximum": 20},
                "sampler": {"type": "string", "enum": ["euler", "heun"]}
            }
        });
        let bad = json!({"steps": 999, "sampler": "nope"});
        let err = validate(&schema, &bad).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("steps"), "{msg}");
        assert!(msg.contains("sampler") || msg.contains("euler"), "{msg}");

        let good = json!({"prompt": "hi", "steps": 10, "sampler": "euler"});
        assert!(validate(&schema, &good).is_ok());
    }

    #[test]
    fn required_message_is_friendly() {
        let schema = json!({
            "type": "object", "required": ["prompt"],
            "properties": {"prompt": {"type": "string"}}
        });
        let err = validate(&schema, &json!({})).unwrap_err().to_string();
        assert!(err.contains("prompt: required"), "{err}");
        assert!(!err.contains("is a required property"), "{err}");
    }

    #[test]
    fn unknown_keys_detected() {
        let schema = json!({"type":"object","properties":{"prompt":{"type":"string"}}});
        let mut m = Map::new();
        m.insert("prompt".into(), json!("x"));
        m.insert("promtp".into(), json!("typo"));
        assert_eq!(unknown_keys(&schema, &m), vec!["promtp".to_string()]);
    }

    #[test]
    fn x_keywords_ignored() {
        // x-control etc. must not break validation.
        let schema = json!({
            "type":"object",
            "properties":{"w":{"type":"integer","x-control":"resolution","x-step":64}}
        });
        assert!(validate(&schema, &json!({"w": 1024})).is_ok());
    }
}
