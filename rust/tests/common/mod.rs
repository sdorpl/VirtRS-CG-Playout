// Reference vectors of the Node.js build (test/vectors/*.json, written by
// test/vectors/generate.mjs).

use std::path::PathBuf;

use virtrs_cg::json::{self, Value};

pub fn vectors(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test/vectors").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    json::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Equal JSON values, object keys in any order.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w))),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w)),
        _ => a == b,
    }
}

pub fn assert_same(actual: &Value, expected: &Value, what: &str) {
    assert!(same(actual, expected), "{what}\n  actual:   {actual}\n  expected: {expected}");
}
