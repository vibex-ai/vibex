use serde_json::{Map, Value};

/// Serialize a JSON value with all object keys sorted recursively.
///
/// Handshake transcripts are MAC'd as raw bytes, so both peers must produce
/// identical serialization. `serde_json`'s object ordering depends on the
/// `preserve_order` feature, which is enabled transitively in some build
/// graphs (e.g. wasm) and not others; canonical key order removes that
/// dependency. Sorted order matches the historical `BTreeMap` output, so
/// native peers remain wire-compatible.
pub fn canonical_json_vec(value: &Value) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&normalize(value))
}

fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<(&String, &Value)> = object.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut normalized = Map::new();
            for (key, entry) in entries {
                normalized.insert(key.clone(), normalize(entry));
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_keys_are_sorted_recursively() {
        let value = json!({
            "zeta": {"beta": 2, "alpha": 1},
            "alpha": [{"b": 1, "a": {"d": 4, "c": 3}}],
        });
        let bytes = canonical_json_vec(&value).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"alpha":[{"a":{"c":3,"d":4},"b":1}],"zeta":{"alpha":1,"beta":2}}"#
        );
    }
}
