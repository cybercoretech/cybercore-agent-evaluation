//! Canonical JSON encoding and SHA-256 helpers.
//!
//! Stored bodies are hashed, so the encoding must not depend on map insertion
//! order (which changes if any crate enables serde_json's `preserve_order`).

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Returns `value` with every object's keys in sorted order.
pub fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = Map::with_capacity(map.len());
            for key in keys {
                sorted.insert(key.clone(), canonicalize(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// Compact canonical JSON text: sorted keys, no insignificant whitespace.
pub fn canonical_json(value: &Value) -> String {
    canonicalize(value).to_string()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest.iter() {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    hex
}

pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_is_independent_of_key_order() {
        let a = json!({"b": 1, "a": {"y": [2, {"d": 0, "c": 1}], "x": null}});
        let b = json!({"a": {"x": null, "y": [2, {"c": 1, "d": 0}]}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"x":null,"y":[2,{"c":1,"d":0}]},"b":1}"#
        );
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_hex(&sha256_hex(b"")));
        assert!(!is_sha256_hex("ABC"));
    }
}
