//! JSON <-> BSON for the queue's documents.
//!
//! serde_json is built with `arbitrary_precision` (so a number's text survives), which the BSON serializer does not understand, so the
//! conversion is written out. A JSON number keeps its kind the way Python's json does: no dot or exponent is an integer, anything else a
//! double. Integers beyond 64 bits are stored as doubles (they cannot occur in a valid claim: money has a few digits).

use cg_core::Num;
use mongodb::bson::{Bson, Document};
use num_traits::ToPrimitive;
use serde_json::{Map, Number, Value};

pub fn to_bson(v: &Value) -> Bson {
    match v {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::String(s) => Bson::String(s.clone()),
        Value::Array(a) => Bson::Array(a.iter().map(to_bson).collect()),
        Value::Object(m) => Bson::Document(m.iter().map(|(k, v)| (k.clone(), to_bson(v))).collect()),
        Value::Number(n) => match Num::from_text(&n.to_string()) {
            Some(Num::Int(i)) => i.to_i64().map(Bson::Int64).unwrap_or_else(|| Bson::Double(Num::Int(i).to_f64())),
            Some(Num::Float(f)) => Bson::Double(f),
            None => Bson::Null,
        },
    }
}

pub fn to_doc(v: &Value) -> Document {
    match to_bson(v) {
        Bson::Document(d) => d,
        _ => Document::new(),
    }
}

pub fn from_bson(b: &Bson) -> Value {
    match b {
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Boolean(x) => Value::Bool(*x),
        Bson::String(s) => Value::String(s.clone()),
        Bson::Int32(i) => Value::Number(Number::from(*i)),
        Bson::Int64(i) => Value::Number(Number::from(*i)),
        Bson::Double(f) => Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        Bson::Array(a) => Value::Array(a.iter().map(from_bson).collect()),
        Bson::Document(d) => from_doc(d),
        Bson::DateTime(d) => Number::from_f64(d.timestamp_millis() as f64 / 1000.0).map(Value::Number).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// A stored document as plain JSON, without Mongo's `_id`.
pub fn from_doc(d: &Document) -> Value {
    Value::Object(d.iter().filter(|(k, _)| k.as_str() != "_id").map(|(k, v)| (k.clone(), from_bson(v))).collect::<Map<_, _>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_round_trips_with_number_kinds_kept() {
        let v: Value = serde_json::from_str(r#"{"a": 1, "b": 2.0, "c": 0.335, "d": [true, null, "x"], "e": {"f": -7}, "g": 0.25}"#).unwrap();
        let back = from_doc(&to_doc(&v));
        assert_eq!(back, v);
        assert_eq!(back["a"].to_string(), "1");
        assert_eq!(back["b"].to_string(), "2.0");
        assert_eq!(to_bson(&json!({"i": 5}).get("i").cloned().unwrap()), Bson::Int64(5));
        assert_eq!(to_bson(&serde_json::from_str::<Value>("5.0").unwrap()), Bson::Double(5.0));
    }

    #[test]
    fn the_object_id_is_dropped_and_odd_values_become_null() {
        let mut d = Document::new();
        d.insert("_id", Bson::Int32(1));
        d.insert("x", Bson::Double(f64::NAN));
        assert_eq!(from_doc(&d), json!({"x": null}));
    }
}
