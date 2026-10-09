//! `engine_core.validate_transport`: the documented envelope every claim must satisfy before the rules run.
//! A claim that fails it is never given a PASS: it gets fifteen fail-closed UNABLE_TO_ASSESS results (see cg-engine).

use crate::Day;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const TOP_KEYS: [&str; 17] = [
    "schema_version", "claim_id", "invoice_number", "patient_id", "member_id", "provider_id", "payer_id", "policy_id", "diagnosis_code",
    "submission_date", "currency", "total_amount", "coverage", "lines", "authorizations", "attachments", "notes",
];
const LINE_KEYS: [&str; 8] = ["line_id", "service_code", "service_date", "modifier", "quantity", "unit_price", "net_amount", "authorization_id"];

fn keys_are(map: &Map<String, Value>, expected: &[&str]) -> bool {
    map.len() == expected.len() && expected.iter().all(|k| map.contains_key(*k))
}

fn nonempty_string(v: &Value) -> bool {
    matches!(v, Value::String(s) if !s.is_empty())
}

fn optional_string(v: &Value) -> bool {
    matches!(v, Value::Null | Value::String(_))
}

fn number_or_null(v: &Value) -> bool {
    matches!(v, Value::Null | Value::Number(_))
}

/// Ok(()) for a valid claim, Err(reason) otherwise. The reasons are the Python messages.
pub fn validate(c: &Value) -> Result<(), &'static str> {
    let obj = c.as_object().ok_or("Unexpected or missing envelope keys")?;
    if !keys_are(obj, &TOP_KEYS) {
        return Err("Unexpected or missing envelope keys");
    }
    for k in ["schema_version", "claim_id", "patient_id", "provider_id", "payer_id", "policy_id", "submission_date", "currency", "notes"] {
        if !nonempty_string(&obj[k]) {
            return Err("Expected nonempty string");
        }
    }
    for k in ["invoice_number", "member_id", "diagnosis_code"] {
        if !optional_string(&obj[k]) {
            return Err("Expected string or null");
        }
    }
    if Day::parse(obj["submission_date"].as_str().unwrap_or("")).is_none() {
        return Err("Invalid submission date");
    }
    let lines = match &obj["lines"] {
        Value::Array(l) if !l.is_empty() => l,
        _ => return Err("Expected nonempty lines"),
    };
    let mut ids = BTreeSet::new();
    for l in lines {
        let line = l.as_object().ok_or("Line keys")?;
        if !keys_are(line, &LINE_KEYS) {
            return Err("Line keys");
        }
        if !nonempty_string(&line["line_id"]) {
            return Err("line_id");
        }
        ids.insert(line["line_id"].as_str().unwrap_or("").to_string());
        for k in ["service_code", "service_date", "modifier", "authorization_id"] {
            if !optional_string(&line[k]) {
                return Err("Line string");
            }
        }
        if let Some(d) = line["service_date"].as_str() {
            if Day::parse(d).is_none() {
                return Err("Invalid service date");
            }
        }
        for k in ["quantity", "unit_price", "net_amount"] {
            if !number_or_null(&line[k]) {
                return Err("Line number");
            }
        }
    }
    if ids.len() != lines.len() {
        return Err("Duplicate line ID");
    }
    if !obj["coverage"].is_object() || !obj["authorizations"].is_array() || !obj["attachments"].is_array() {
        return Err("Expected context structures");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({"schema_version": "1.0.0", "claim_id": "C1", "invoice_number": "I", "patient_id": "P", "member_id": "M", "provider_id": "PR", "payer_id": "PA",
            "policy_id": "EDU-BASIC", "diagnosis_code": "D", "submission_date": "2026-10-09", "currency": "SAR", "total_amount": 10,
            "coverage": {}, "lines": [{"line_id": "L1", "service_code": "SVC-LAB", "service_date": "2026-10-01", "modifier": null, "quantity": 1,
            "unit_price": 10, "net_amount": 10, "authorization_id": null}], "authorizations": [], "attachments": [], "notes": "n"})
    }

    #[test]
    fn a_good_claim_passes() {
        assert_eq!(validate(&good()), Ok(()));
    }

    #[test]
    fn each_breach_is_refused() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
            ("extra key", Box::new(|c| { c["extra"] = json!(1); })),
            ("missing key", Box::new(|c| { c.as_object_mut().unwrap().remove("notes"); })),
            ("empty claim id", Box::new(|c| { c["claim_id"] = json!(""); })),
            ("number as claim id", Box::new(|c| { c["claim_id"] = json!(5); })),
            ("bad submission date", Box::new(|c| { c["submission_date"] = json!("2026-02-30"); })),
            ("no lines", Box::new(|c| { c["lines"] = json!([]); })),
            ("line as string", Box::new(|c| { c["lines"] = json!(["x"]); })),
            ("bool quantity", Box::new(|c| { c["lines"][0]["quantity"] = json!(true); })),
            ("string quantity", Box::new(|c| { c["lines"][0]["quantity"] = json!("1"); })),
            ("bad service date", Box::new(|c| { c["lines"][0]["service_date"] = json!("tomorrow"); })),
            ("duplicate line ids", Box::new(|c| { let l = c["lines"][0].clone(); c["lines"].as_array_mut().unwrap().push(l); })),
            ("coverage as list", Box::new(|c| { c["coverage"] = json!([]); })),
            ("top-level array", Box::new(|c| { *c = json!([]); })),
        ];
        for (name, mutate) in cases {
            let mut c = good();
            mutate(&mut c);
            assert!(validate(&c).is_err(), "{name}");
        }
    }
}
