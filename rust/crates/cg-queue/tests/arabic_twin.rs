//! docs/55: every public claim (development + validation) is rewritten the way an Arabic-writing submitter would write it (Arabic field names, Arabic
//! values, Arabic-Indic digits and decimal marks), read back, and must come out as the SAME canonical claim with the SAME fifteen results.

use cg_core::localize::{glossary_json, localize};
use cg_engine::Engine;
use cg_queue::flow::engine_fn;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::Arc;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn lines(path: PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn arabic_digits(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_digit() { char::from_u32(0x0660 + c as u32 - 0x30).unwrap() } else { c }).collect()
}

fn first_label(scope: &str, key: &str) -> String {
    glossary_json()["scopes"][scope][key]["ar"][0].as_str().unwrap_or_else(|| panic!("{scope}.{key} has an Arabic label")).to_string()
}

fn first_word(enum_name: &str, canon: &str) -> Option<String> {
    glossary_json()["enums"][enum_name][canon][0].as_str().map(str::to_string)
}

/// The Arabic twin of one object of `scope`.
fn twin(scope: &str, obj: &Map<String, Value>) -> Map<String, Value> {
    let g = &glossary_json()["scopes"][scope];
    let mut out = Map::new();
    for (k, v) in obj {
        let f = &g[k];
        let name = first_label(scope, k);
        let value = match (f["container"].as_str(), v) {
            (Some("object"), Value::Object(o)) => Value::Object(twin("coverage", o)),
            (Some("array"), Value::Array(a)) => {
                let sub = match k.as_str() {
                    "lines" => "line",
                    "authorizations" => "authorization",
                    _ => "attachment",
                };
                Value::Array(a.iter().map(|x| Value::Object(twin(sub, x.as_object().unwrap()))).collect())
            }
            _ => match v {
                Value::Number(n) => Value::String(arabic_digits(&n.to_string()).replace('.', "٫")),
                Value::String(s) if f["date"] == true || k == "submission_date" => Value::String(arabic_digits(s)),
                Value::String(s) => {
                    let en = if k == "currency" { Some("currency") } else { f["enum"].as_str() };
                    match en.and_then(|e| first_word(e, s)) {
                        Some(w) => Value::String(w),
                        None => v.clone(),
                    }
                }
                other => other.clone(),
            },
        };
        out.insert(name, value);
    }
    out
}

#[test]
fn every_public_claim_has_an_arabic_twin_that_reads_back_identical() {
    let engine = engine_fn(Arc::new(Engine::load(&root()).expect("the rule engine loads")));
    let (mut n, mut converted) = (0, 0);
    for dir in ["development", "validation"] {
        for claim in lines(root().join("data").join(dir).join("claims.jsonl")) {
            let obj = claim.as_object().unwrap();
            if cg_core::transport::validate(&claim).is_err() {
                continue; // an invalid claim has no canonical form to compare
            }
            let arabic = Value::Object(twin("claim", obj));
            let l = localize(&arabic).unwrap_or_else(|r| panic!("{}: {} {}", claim["claim_id"], r.path, r.reason));
            assert_eq!(l.claim, claim, "{}", claim["claim_id"]);
            assert_eq!(engine(&l.claim).unwrap(), engine(&claim).unwrap(), "{}", claim["claim_id"]);
            assert!(!l.changes.is_empty());
            converted += l.changes.len();
            n += 1;
        }
    }
    assert!(n >= 300, "{n} claims compared");
    let _ = json!(converted);
}
