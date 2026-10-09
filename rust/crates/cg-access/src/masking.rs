//! Data minimization for the reviewer API: what a response may contain depends on the caller's permissions (`src/access/masking.py`).
//!
//! * Patient and member identifiers are replaced by stable pseudonyms (a keyed hash, so the same person is the same pseudonym everywhere
//!   but nothing can be recovered from it). The replacement is applied to every string in the response, so an identifier quoted inside an
//!   evidence value or an explanation is covered too.
//! * Free text (the claim notes and attachment text) is removed, not blanked, unless the caller may read it: hide, not disable.
//! * After masking, the response is checked for any leftover raw identifier; if one is found the request fails instead of leaking.

use hmac::{Hmac, Mac};
use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};
use sha2::Sha256;
use std::collections::BTreeMap;

pub const PSEUDONYM_HEX: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum MaskError {
    #[error("cannot make a pseudonym from this input")]
    Input,
    #[error("an identifier survived masking")]
    Redaction,
}

pub fn pseudonym(key: &str, prefix: &str, value: &str) -> Result<String, MaskError> {
    if !["PAT", "MEM"].contains(&prefix) || value.is_empty() || key.chars().count() < 32 {
        return Err(MaskError::Input);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).map_err(|_| MaskError::Input)?;
    mac.update(format!("{prefix}|{value}").as_bytes());
    Ok(format!("{prefix}-{}", hex::encode(mac.finalize().into_bytes())[..PSEUDONYM_HEX].to_uppercase()))
}

/// {raw identifier: pseudonym} for every patient or member identifier the claim carries.
pub fn identifier_map(claim: &Value, key: &str) -> Result<BTreeMap<String, String>, MaskError> {
    let mut pairs: Vec<(String, &str)> = vec![];
    let mut add = |v: Option<&Value>, prefix: &'static str| {
        if let Some(s) = v.and_then(Value::as_str).filter(|s| !s.is_empty()) {
            pairs.push((s.to_string(), prefix));
        }
    };
    add(claim.get("patient_id"), "PAT");
    add(claim.get("member_id"), "MEM");
    if let Some(cov) = claim.get("coverage").filter(|c| c.is_object()) {
        add(cov.get("beneficiary_patient_id"), "PAT");
        add(cov.get("member_id"), "MEM");
    }
    for pool in ["authorizations", "attachments"] {
        if let Some(rows) = claim.get(pool).and_then(Value::as_array) {
            for row in rows.iter().filter(|r| r.is_object()) {
                add(row.get("patient_id"), "PAT");
            }
        }
    }
    let mut mapping = BTreeMap::new();
    for (raw, prefix) in pairs {
        if !mapping.contains_key(&raw) {
            let p = pseudonym(key, prefix, &raw)?;
            mapping.insert(raw, p);
        }
    }
    Ok(mapping)
}

fn pattern(mapping: &BTreeMap<String, String>) -> Option<Regex> {
    // one case-insensitive alternation, longest identifier first, so a short identifier never eats part of a longer one
    let mut raws: Vec<&String> = mapping.keys().collect();
    raws.sort_by_key(|r| std::cmp::Reverse(r.chars().count()));
    let alternation = raws.iter().map(|r| regex::escape(r)).collect::<Vec<_>>().join("|");
    RegexBuilder::new(&alternation).case_insensitive(true).build().ok()
}

/// A deep copy of `obj` with every raw identifier in every string replaced by its pseudonym.
pub fn scrub(obj: &Value, mapping: &BTreeMap<String, String>) -> Value {
    if mapping.is_empty() {
        return obj.clone();
    }
    let lookup: BTreeMap<String, &String> = mapping.iter().map(|(raw, masked)| (raw.to_lowercase(), masked)).collect();
    let Some(re) = pattern(mapping) else { return obj.clone() };
    fn walk(o: &Value, re: &Regex, lookup: &BTreeMap<String, &String>) -> Value {
        match o {
            Value::String(s) => Value::String(
                re.replace_all(s, |c: &regex::Captures| lookup.get(&c[0].to_lowercase()).map(|m| (*m).clone()).unwrap_or_else(|| c[0].to_string())).into_owned(),
            ),
            Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), walk(v, re, lookup))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(|v| walk(v, re, lookup)).collect()),
            other => other.clone(),
        }
    }
    walk(obj, &re, &lookup)
}

/// The raw identifiers that appear anywhere in `obj`.
pub fn leaks(obj: &Value, raw_ids: &BTreeMap<String, String>) -> Vec<String> {
    let text = obj.to_string();
    raw_ids
        .keys()
        .filter(|raw| RegexBuilder::new(&regex::escape(raw)).case_insensitive(true).build().map(|re| re.is_match(&text)).unwrap_or(true))
        .cloned()
        .collect()
}

fn strip_free_text(o: &Value) -> Value {
    match o {
        Value::Object(m) => Value::Object(m.iter().filter(|(k, _)| *k != "notes" && *k != "text").map(|(k, v)| (k.clone(), strip_free_text(v))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(strip_free_text).collect()),
        other => other.clone(),
    }
}

fn is_free_text_path(path: &Value) -> bool {
    let Some(p) = path.as_str() else { return false };
    if p == "/notes" || p.starts_with("/notes/") {
        return true;
    }
    p.strip_prefix("/attachments/").and_then(|r| r.strip_suffix("/text")).is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
}

/// The claim and its results as this caller may see them.
pub fn shape_claim(claim: &Value, results: &Value, permissions: &std::collections::BTreeSet<&'static str>, key: &str, unmasked: bool, verify: bool) -> Result<(Value, Value), MaskError> {
    let mapping = identifier_map(claim, key)?;
    let (mut claim_out, mut results_out) = (claim.clone(), results.clone());
    if !permissions.contains("claims.view_notes") {
        if let Some(obj) = claim_out.as_object_mut() {
            obj.remove("notes");
            if let Some(atts) = obj.get_mut("attachments").and_then(Value::as_array_mut) {
                for a in atts.iter_mut().filter_map(Value::as_object_mut) {
                    a.remove("text");
                }
            }
        }
        if let Some(list) = results_out.as_array_mut() {
            for result in list.iter_mut().filter_map(Value::as_object_mut) {
                let kept: Vec<Value> = result
                    .get("evidence")
                    .and_then(Value::as_array)
                    .map(|ev| {
                        ev.iter()
                            .filter(|e| !(e.is_object() && is_free_text_path(&e["path"])))
                            .map(|e| match e.as_object() {
                                Some(o) if o.contains_key("value") => {
                                    let mut copy: Map<String, Value> = o.clone();
                                    copy.insert("value".into(), strip_free_text(&o["value"]));
                                    Value::Object(copy)
                                }
                                _ => e.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                result.insert("evidence".into(), Value::Array(kept));
            }
        }
    }
    if !unmasked {
        claim_out = scrub(&claim_out, &mapping);
        results_out = scrub(&results_out, &mapping);
        if verify && !leaks(&serde_json::json!({"claim": claim_out, "results": results_out}), &mapping).is_empty() {
            return Err(MaskError::Redaction);
        }
    }
    Ok((claim_out, results_out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123";

    fn claim() -> Value {
        json!({"claim_id": "C1", "patient_id": "PAT-7", "member_id": "MEM-9", "notes": "call PAT-7 back", "coverage": {"beneficiary_patient_id": "PAT-7", "member_id": "MEM-9"},
               "authorizations": [{"patient_id": "pat-7x"}], "attachments": [{"patient_id": "PAT-7", "text": "secret free text"}], "lines": []})
    }

    #[test]
    fn pseudonyms_are_stable_keyed_and_prefixed() {
        let a = pseudonym(KEY, "PAT", "PAT-7").unwrap();
        assert_eq!(a, pseudonym(KEY, "PAT", "PAT-7").unwrap());
        assert!(a.starts_with("PAT-") && a.len() == 12);
        assert_ne!(a, pseudonym(KEY, "MEM", "PAT-7").unwrap());
        assert_ne!(a, pseudonym(&KEY.replace('0', "9"), "PAT", "PAT-7").unwrap());
        assert!(pseudonym("short", "PAT", "x").is_err() && pseudonym(KEY, "XXX", "x").is_err() && pseudonym(KEY, "PAT", "").is_err());
    }

    #[test]
    fn every_identifier_everywhere_is_replaced_longest_first() {
        let c = claim();
        let m = identifier_map(&c, KEY).unwrap();
        assert_eq!(m.len(), 3, "PAT-7, MEM-9 and pat-7x");
        let out = scrub(&json!({"explanation": "see PAT-7 and pat-7x and mem-9", "list": ["PAT-7"]}), &m);
        let text = out.to_string();
        assert!(!text.to_lowercase().contains("pat-7") && !text.to_lowercase().contains("mem-9"), "{text}");
        assert!(text.contains(&m["PAT-7"]) && text.contains(&m["pat-7x"]));
    }

    #[test]
    fn free_text_is_removed_for_callers_without_the_permission() {
        let results = json!([{"evidence": [{"path": "/notes", "value": "x"}, {"path": "/attachments/0/text", "value": "y"}, {"path": "/patient_id", "value": "PAT-7"},
                                            {"path": "/attachments", "value": [{"text": "z", "patient_id": "PAT-7"}]}]}]);
        let none: BTreeSet<&'static str> = ["claims.view"].into_iter().collect();
        let (c, r) = shape_claim(&claim(), &results, &none, KEY, false, true).unwrap();
        assert!(c.get("notes").is_none() && c["attachments"][0].get("text").is_none());
        let paths: Vec<&str> = r[0]["evidence"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/patient_id", "/attachments"]);
        assert!(!r.to_string().contains("\"z\""));
        let notes: BTreeSet<&'static str> = ["claims.view", "claims.view_notes"].into_iter().collect();
        let (c, _) = shape_claim(&claim(), &results, &notes, KEY, false, true).unwrap();
        assert!(c.get("notes").is_some() && !c["notes"].as_str().unwrap().contains("PAT-7"), "masked but present");
    }

    #[test]
    fn unmasking_returns_the_raw_identifiers() {
        let all: BTreeSet<&'static str> = ["claims.view", "claims.view_notes"].into_iter().collect();
        let (c, _) = shape_claim(&claim(), &json!([]), &all, KEY, true, true).unwrap();
        assert_eq!(c["patient_id"], "PAT-7");
    }
}
