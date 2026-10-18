//! The reviewer's copilot: what to ask for or correct, what a correction would change, and a one-screen summary of a claim.
//!
//! Everything here is deterministic and built from the engine's own findings and the claim's own values, so every sentence names the
//! rule and the claim paths it rests on. No model is involved and nothing is invented: a value is proposed only when it follows by
//! arithmetic from other values on the same claim (a line amount from quantity and unit price, a total from the line amounts) or is
//! the policy's own constant (the currency); anything else becomes a request for information. A proposal is a suggestion to try in
//! a what-if, never an edit, and it never touches an identifier.

use bigdecimal::BigDecimal;
use cg_core::config::Config;
use cg_core::date::Day;
use cg_core::num::{cents, Num};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub const MAX_CHANGES: usize = 20;
const MAX_TEXT: usize = 100;
const FLAGGED: [&str; 2] = ["FAIL", "UNABLE_TO_ASSESS"];

const HEADER_FIELDS: [&str; 7] = ["invoice_number", "diagnosis_code", "submission_date", "currency", "total_amount", "provider_id", "policy_id"];
const COVERAGE_FIELDS: [&str; 3] = ["status", "start_date", "end_date"];
const LINE_FIELDS: [&str; 7] = ["service_code", "service_date", "modifier", "quantity", "unit_price", "net_amount", "authorization_id"];
const AUTH_FIELDS: [&str; 5] = ["service_code", "status", "valid_from", "valid_to", "max_quantity"];
const ATTACHMENT_FIELDS: [&str; 4] = ["type", "service_code", "service_date", "document_status"];

// ---- corrections ------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    pub value: Value,
}

/// `[{"path": "/lines/0/net_amount", "value": 420}, ...]`
pub fn parse_changes(v: &Value) -> Result<Vec<Change>, String> {
    let list = v.as_array().ok_or("changes must be a list")?;
    if list.is_empty() || list.len() > MAX_CHANGES {
        return Err(format!("send between 1 and {MAX_CHANGES} changes"));
    }
    let mut out = vec![];
    for item in list {
        let o = item.as_object().ok_or("each change is an object with a path and a value")?;
        if o.keys().any(|k| k != "path" && k != "value") || !o.contains_key("value") {
            return Err("each change has exactly a path and a value".into());
        }
        let path = o["path"].as_str().ok_or("a change path must be text")?.to_string();
        out.push(Change { path, value: o["value"].clone() });
    }
    Ok(out)
}

fn index_ok(claim: &Value, list: &str, idx: &str) -> bool {
    !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()) && (idx == "0" || !idx.starts_with('0')) && idx.parse::<usize>().ok().is_some_and(|i| claim[list].as_array().is_some_and(|a| i < a.len()))
}

/// Which fields a reviewer may correct. Identifiers (patient, member, claim, line and document ids), free text and notes are never
/// editable here: they come from the source system, and the reviewer sees them only as pseudonyms.
pub fn editable(claim: &Value, path: &str) -> bool {
    let seg: Vec<&str> = path.split('/').collect();
    if seg.first() != Some(&"") {
        return false;
    }
    match &seg[1..] {
        [f] => HEADER_FIELDS.contains(f),
        ["coverage", f] => COVERAGE_FIELDS.contains(f),
        ["lines", i, f] => LINE_FIELDS.contains(f) && index_ok(claim, "lines", i),
        ["authorizations", i, f] => AUTH_FIELDS.contains(f) && index_ok(claim, "authorizations", i),
        ["attachments", i, f] => ATTACHMENT_FIELDS.contains(f) && index_ok(claim, "attachments", i),
        _ => false,
    }
}

/// The corrected copy of a claim and the list of what changed (`path`, `from`, `to`). The original is never touched.
pub fn apply(claim: &Value, changes: &[Change]) -> Result<(Value, Vec<Value>), String> {
    if changes.is_empty() || changes.len() > MAX_CHANGES {
        return Err(format!("send between 1 and {MAX_CHANGES} changes"));
    }
    let mut seen = BTreeSet::new();
    let mut out = claim.clone();
    let mut applied = vec![];
    for c in changes {
        if !seen.insert(c.path.as_str()) {
            return Err(format!("{} appears twice", c.path.chars().take(80).collect::<String>()));
        }
        if !editable(claim, &c.path) {
            return Err(format!("{} cannot be corrected here", c.path.chars().take(80).collect::<String>()));
        }
        match &c.value {
            Value::Null | Value::Number(_) => {}
            Value::String(s) if s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control) => {}
            _ => return Err(format!("{}: a value is text (100 characters at most), a number or null", c.path.chars().take(80).collect::<String>())),
        }
        let slot = out.pointer_mut(&c.path).ok_or_else(|| format!("{} does not exist on this claim", c.path.chars().take(80).collect::<String>()))?;
        if *slot == c.value {
            return Err(format!("{} already has that value", c.path.chars().take(80).collect::<String>()));
        }
        applied.push(json!({"path": c.path, "from": slot.clone(), "to": c.value}));
        *slot = c.value.clone();
    }
    Ok((out, applied))
}

fn is_flagged(r: &Value) -> bool {
    r["status"].as_str().is_some_and(|s| FLAGGED.contains(&s))
}

/// What a correction did to each rule: resolved, still flagged, newly flagged, or untouched.
pub fn diff(before: &Value, after: &Value) -> Value {
    let rows = |v: &Value| -> Vec<Value> { v.as_array().cloned().unwrap_or_default() };
    let (b, a) = (rows(before), rows(after));
    let mut out = vec![];
    let (mut resolved, mut still, mut newly, mut same) = (0, 0, 0, 0);
    for ra in &a {
        let id = ra["rule_id"].as_str().unwrap_or("");
        let rb = b.iter().find(|r| r["rule_id"] == id).cloned().unwrap_or(Value::Null);
        let outcome = match (is_flagged(&rb), is_flagged(ra)) {
            (true, false) => {
                resolved += 1;
                "resolved"
            }
            (true, true) => {
                still += 1;
                "still_flagged"
            }
            (false, true) => {
                newly += 1;
                "newly_flagged"
            }
            (false, false) => {
                same += 1;
                "unchanged"
            }
        };
        out.push(json!({"rule_id": id, "severity": ra["severity"], "before": rb["status"], "after": ra["status"], "outcome": outcome,
            "changed_status": rb["status"] != ra["status"]}));
    }
    let flagged_before = b.iter().filter(|r| is_flagged(r)).count();
    let flagged_after = a.iter().filter(|r| is_flagged(r)).count();
    json!({"rules": out, "counts": {"resolved": resolved, "still_flagged": still, "newly_flagged": newly, "unchanged": same},
        "flagged_before": flagged_before, "flagged_after": flagged_after, "would_clear": flagged_after == 0})
}

// ---- suggestions ------------------------------------------------------------------------------------------------------------------

fn num_of(v: &Value) -> Option<BigDecimal> {
    Num::from_json(v).map(|n| n.to_decimal())
}

/// A money value as JSON: whole amounts without decimals (like the claims themselves), otherwise two decimals at most.
fn money(d: &BigDecimal) -> Value {
    let text = cents(d).normalized().to_plain_string();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn money_text(d: &BigDecimal) -> String {
    cents(d).normalized().to_plain_string()
}

fn line_label(claim: &Value, idx: usize) -> String {
    claim["lines"][idx]["line_id"].as_str().map(str::to_string).unwrap_or_else(|| format!("line {}", idx + 1))
}

fn line_index(claim: &Value, line_id: &str) -> Option<usize> {
    claim["lines"].as_array()?.iter().position(|l| l["line_id"] == line_id)
}

fn field_label(path: &str) -> String {
    let seg: Vec<&str> = path.split('/').skip(1).collect();
    match seg.as_slice() {
        ["lines", i, f] => format!("{} of line {}", f.replace('_', " "), i.parse::<usize>().map(|n| n + 1).unwrap_or(0)),
        [a, b] => format!("{} {}", a.replace('_', " "), b.replace('_', " ")),
        [f] => f.replace('_', " "),
        _ => path.to_string(),
    }
}

fn missing_paths(finding: &Value) -> Vec<String> {
    finding["evidence"].as_array().into_iter().flatten().filter(|e| e["value"].is_null() || e["value"] == "").filter_map(|e| e["path"].as_str().map(str::to_string)).collect()
}

fn edit(path: &str, from: &Value, to: Value, basis: &str) -> Value {
    json!({"path": path, "from": from, "to": to, "basis": basis})
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The one value missing on a line, from the other two (net amount = quantity x unit price). Only when exactly that one is missing, the others are numbers, and
/// the result is exact (a unit price in whole cents, a whole positive quantity). Anything else stays a request.
fn derive_line_value(claim: &Value, idx: usize, field: &str) -> Option<(Value, &'static str)> {
    let l = claim["lines"].get(idx)?;
    let (q, p, n) = (num_of(&l["quantity"]), num_of(&l["unit_price"]), num_of(&l["net_amount"]));
    let zero = BigDecimal::from(0);
    match (field, q, p, n) {
        ("net_amount", Some(q), Some(p), None) => Some((money(&cents(&(q * p))), "quantity x unit price")),
        ("unit_price", Some(q), None, Some(n)) if q > zero => {
            let p = cents(&(n.clone() / q.clone()));
            (cents(&(q * p.clone())) == cents(&n)).then(|| (money(&p), "line amount / quantity"))
        }
        ("quantity", None, Some(p), Some(n)) if p > zero => {
            let q = (n / p).normalized();
            (q > zero && q.fractional_digit_count() <= 0).then(|| (money(&q), "line amount / unit price"))
        }
        _ => None,
    }
}

/// The claim's own approved authorization that covers a line: same patient and service, the service date inside its validity, and room left in its maximum
/// quantity. Only when there is exactly one; two candidates is a choice a person makes.
fn matching_authorization(claim: &Value, idx: usize) -> Option<String> {
    let l = claim["lines"].get(idx)?;
    let date = l["service_date"].as_str()?;
    let qty = num_of(&l["quantity"])?;
    let mut found: Vec<String> = vec![];
    for a in claim["authorizations"].as_array()? {
        let id = a["authorization_id"].as_str()?;
        let ok = a["status"] == "approved"
            && a["service_code"] == l["service_code"]
            && a["patient_id"] == claim["patient_id"]
            && a["valid_from"].as_str().is_some_and(|f| f <= date)
            && a["valid_to"].as_str().is_some_and(|t| date <= t);
        if !ok {
            continue;
        }
        let others: BigDecimal = claim["lines"].as_array().into_iter().flatten().enumerate().filter(|(j, x)| *j != idx && x["authorization_id"] == id).filter_map(|(_, x)| num_of(&x["quantity"])).sum();
        if num_of(&a["max_quantity"]).is_none_or(|max| others + qty.clone() <= max) {
            found.push(id.to_string());
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

fn suggestion(finding: &Value, kind: &str, text: String, edits: Vec<Value>, options: Vec<String>) -> Value {
    let cites: Vec<&Value> = finding["evidence"].as_array().into_iter().flatten().filter_map(|e| e.get("path")).collect();
    json!({"rule_id": finding["rule_id"], "severity": finding["severity"], "status": finding["status"], "kind": kind, "text": text,
        "proposed_edits": edits, "options": options, "cites": cites, "rule_source": finding["rule_source"],
        "needs_source_check": !edits.is_empty() || kind == "verify"})
}

fn request(finding: &Value, what: &[String], fallback: &str) -> Value {
    let text = if what.is_empty() { fallback.to_string() } else { format!("Ask the provider for: {}. It has to come from the source record; it is not filled in here.", what.join(", ")) };
    suggestion(finding, "request_information", text, vec![], vec![])
}

/// One suggestion for a flagged finding. `cfg` supplies the policy tables and the service catalogue the rule itself used.
pub fn suggest_one(claim: &Value, finding: &Value, cfg: &Config) -> Value {
    let rule = finding["rule_id"].as_str().unwrap_or("");
    let unable = finding["status"] == "UNABLE_TO_ASSESS";
    let policy = claim["policy_id"].as_str().and_then(|p| cfg.policies.get(p));
    let lines: Vec<usize> = finding["affected_line_ids"].as_array().into_iter().flatten().filter_map(|l| l.as_str()).filter_map(|l| line_index(claim, l)).collect();
    let missing: Vec<String> = missing_paths(finding).iter().map(|p| field_label(p)).collect();
    if unable && rule != "R006" {
        let why = if policy.is_none() { "the policy named on the claim is not one the rulebook knows, so the check could not run" } else { "a value the rule needs is missing" };
        let text = if missing.is_empty() { format!("{rule} could not be assessed: {why}. Confirm the policy and the missing input with the provider.") } else { format!("{rule} could not be assessed because {} is missing. Ask the provider for it; it is not filled in here.", missing.join(", ")) };
        return suggestion(finding, "request_information", text, vec![], vec![]);
    }
    match rule {
        "R001" => {
            let (mut edits, mut notes, mut rest) = (vec![], vec![], vec![]);
            for path in missing_paths(finding) {
                let seg: Vec<&str> = path.split('/').skip(1).collect();
                let derived = match seg.as_slice() {
                    ["lines", i, f] => i.parse::<usize>().ok().and_then(|idx| derive_line_value(claim, idx, f).map(|d| (idx, d))),
                    _ => None,
                };
                match derived {
                    Some((idx, (value, basis))) => {
                        notes.push(format!("{} = {} ({basis})", field_label(&path), money_text(&num_of(&value).unwrap_or_default())));
                        edits.push(edit(&path, &Value::Null, value, basis));
                        let _ = idx;
                    }
                    None => rest.push(field_label(&path)),
                }
            }
            if edits.is_empty() {
                return request(finding, &missing, "A required field is empty. Request it from the source.");
            }
            let mut text = format!("A required value is empty, and it follows from the other values on the same line: {}. Check it against the bill before accepting it.", notes.join("; "));
            if !rest.is_empty() {
                text.push_str(&format!(" Still missing, to ask the provider for: {}.", rest.join(", ")));
            }
            let mut s = suggestion(finding, "correct_value", text, edits, vec![]);
            s["still_missing"] = json!(rest);
            s
        }
        "R002" => {
            let sub = claim["submission_date"].as_str().unwrap_or("?");
            let late: Vec<String> = lines.iter().map(|i| format!("{} ({})", line_label(claim, *i), claim["lines"][*i]["service_date"].as_str().unwrap_or("?"))).collect();
            suggestion(finding, "verify", format!("Service date {} is after the submission date {sub}. Check both dates against the original record; one of them was entered wrongly.", late.join(", ")), vec![], vec![])
        }
        "R003" => {
            let cov = &claim["coverage"];
            let status = cov["status"].as_str().unwrap_or("?");
            let text = if status != "active" { format!("Coverage is recorded as '{status}', not active. Confirm the member's coverage on the service date with the payer's records.") } else { format!("A service date falls outside the coverage period {} to {}. Check the service date, or ask the payer for the coverage in force on that date.", cov["start_date"].as_str().unwrap_or("?"), cov["end_date"].as_str().unwrap_or("?")) };
            suggestion(finding, "verify", text, vec![], vec![])
        }
        "R004" => {
            let mut parts = vec![];
            if claim["patient_id"] != claim["coverage"]["beneficiary_patient_id"] {
                parts.push("the patient on the claim and the coverage's beneficiary");
            }
            if claim["member_id"] != claim["coverage"]["member_id"] {
                parts.push("the member on the claim and the coverage's member");
            }
            suggestion(finding, "request_information", format!("These identifiers do not match: {}. Identifiers cannot be edited here; ask the registration desk for the authoritative record and resubmit.", if parts.is_empty() { "patient and member".to_string() } else { parts.join("; ") }), vec![], vec![])
        }
        "R005" => {
            let net = policy.map(|p| p.allowed_providers.join(", ")).unwrap_or_default();
            suggestion(finding, "verify", format!("Provider {} is not in the network for {}. In-network providers: {}. Confirm the provider on the bill.", claim["provider_id"].as_str().unwrap_or("?"), claim["policy_id"].as_str().unwrap_or("?"), net), vec![], vec![])
        }
        "R006" => {
            let names: Vec<String> = lines.iter().map(|i| line_label(claim, *i)).collect();
            suggestion(finding, "verify", format!("Lines {} repeat the same service, date and modifier. If they are separate documented services, ask the provider for the modifier that tells them apart; if not, one is a duplicate.", names.join(", ")), vec![], vec![])
        }
        "R007" => {
            let mut edits = vec![];
            let mut notes = vec![];
            for i in &lines {
                let l = &claim["lines"][*i];
                if let (Some(q), Some(p)) = (num_of(&l["quantity"]), num_of(&l["unit_price"])) {
                    let expect = cents(&(q * p));
                    let label = line_label(claim, *i);
                    notes.push(format!("{label}: quantity x unit price = {}, billed {}", money_text(&expect), l["net_amount"]));
                    edits.push(edit(&format!("/lines/{i}/net_amount"), &l["net_amount"], money(&expect), "quantity x unit price"));
                }
            }
            suggestion(finding, "correct_value", format!("The line amount does not equal quantity x unit price ({}). If the quantity and price are right, the amount is wrong; if the amount is right, one of them is. Check the bill before changing any.", notes.join("; ")), edits, vec![])
        }
        "R008" => {
            let (mut edits, mut notes, mut rest) = (vec![], vec![], vec![]);
            for i in &lines {
                let label = format!("{} ({})", line_label(claim, *i), claim["lines"][*i]["service_code"].as_str().unwrap_or("?"));
                match matching_authorization(claim, *i) {
                    Some(id) => {
                        notes.push(format!("{label}: the claim already carries one approved authorization for this patient, service and date ({id})"));
                        edits.push(edit(&format!("/lines/{i}/authorization_id"), &claim["lines"][*i]["authorization_id"], json!(id), "the claim's own approved authorization"));
                    }
                    None => rest.push(label),
                }
            }
            if edits.is_empty() {
                return suggestion(finding, "request_information", format!("This service needs an authorization reference and line {} has none. Ask the provider for the reference; do not enter one that was not issued.", rest.join(", ")), vec![], vec![]);
            }
            let mut text = format!("This service needs an authorization reference and the line has none. {}. Check that it is the one the provider meant.", notes.join("; "));
            if !rest.is_empty() {
                text.push_str(&format!(" No single authorization on the claim fits {}: ask the provider for the reference; do not enter one that was not issued.", rest.join(", ")));
            }
            let mut s = suggestion(finding, "correct_value", text, edits, vec![]);
            s["still_missing"] = json!(rest.iter().map(|l| format!("authorization reference for {l}")).collect::<Vec<_>>());
            s
        }
        "R009" => {
            let mut reasons = vec![];
            for i in &lines {
                let l = &claim["lines"][*i];
                let label = line_label(claim, *i);
                let id = l["authorization_id"].as_str().unwrap_or("");
                match claim["authorizations"].as_array().and_then(|a| a.iter().find(|x| x["authorization_id"] == id)) {
                    None => reasons.push(format!("{label}: the authorization {id} is not among the supplied records")),
                    Some(a) => {
                        if a["status"] != "approved" {
                            reasons.push(format!("{label}: the authorization status is '{}', not approved", a["status"].as_str().unwrap_or("?")));
                        }
                        if a["service_code"] != l["service_code"] {
                            reasons.push(format!("{label}: the authorization is for a different service"));
                        }
                        if a["patient_id"] != claim["patient_id"] {
                            reasons.push(format!("{label}: the authorization belongs to a different patient"));
                        }
                        let (d, from, to) = (l["service_date"].as_str(), a["valid_from"].as_str(), a["valid_to"].as_str());
                        if let (Some(d), Some(f), Some(t)) = (d, from, to) {
                            if d < f || d > t {
                                reasons.push(format!("{label}: the service date {d} is outside the authorization's validity {f} to {t}"));
                            }
                        }
                        let total: BigDecimal = claim["lines"].as_array().into_iter().flatten().filter(|x| x["authorization_id"] == id).filter_map(|x| num_of(&x["quantity"])).sum();
                        if let Some(max) = num_of(&a["max_quantity"]) {
                            if total > max {
                                reasons.push(format!("{label}: the claim bills {} against an approved maximum of {}", money_text(&total), money_text(&max)));
                            }
                        }
                    }
                }
            }
            let (mut edits, mut swaps, mut rest) = (vec![], vec![], vec![]);
            for i in &lines {
                let cur = claim["lines"][*i]["authorization_id"].clone();
                match matching_authorization(claim, *i) {
                    Some(id) if cur != json!(id) => {
                        swaps.push(format!("{}: {id} is the only authorization on the claim that fits this patient, service and date", line_label(claim, *i)));
                        edits.push(edit(&format!("/lines/{i}/authorization_id"), &cur, json!(id), "the claim's own approved authorization"));
                    }
                    _ => rest.push(line_label(claim, *i)),
                }
            }
            let why = if reasons.is_empty() { "see the evidence".to_string() } else { reasons.join("; ") };
            if edits.is_empty() {
                return suggestion(finding, "request_information", format!("The authorization does not match the service: {why}. Ask the provider or the payer for the correct approval; the authorization record is not edited here."), vec![], vec![]);
            }
            let mut text = format!("The authorization does not match the service: {why}. {}. Check that it is the one the provider meant.", swaps.join("; "));
            if !rest.is_empty() {
                text.push_str(&format!(" For {} no single authorization on the claim fits: ask the provider or the payer for the correct approval.", rest.join(", ")));
            }
            let mut s = suggestion(finding, "correct_value", text, edits, vec![]);
            s["still_missing"] = json!(rest.iter().map(|l| format!("a matching authorization for {l}")).collect::<Vec<_>>());
            s
        }
        "R010" => {
            let mut parts = vec![];
            for i in &lines {
                let l = &claim["lines"][*i];
                let code = l["service_code"].as_str().unwrap_or("");
                let need = policy.and_then(|p| p.required_documents.get(code)).cloned().unwrap_or_default();
                let draft = claim["attachments"].as_array().into_iter().flatten().any(|a| a["type"] == need.as_str() && a["service_code"] == code && a["service_date"] == l["service_date"] && a["patient_id"] == claim["patient_id"] && a["document_status"] != "final");
                parts.push(format!("{}: a final '{need}' document for {code} on {}{}", line_label(claim, *i), l["service_date"].as_str().unwrap_or("?"), if draft { " (a draft exists; ask for the final version)" } else { "" }));
            }
            suggestion(finding, "request_information", format!("Supporting document needed: {}.", parts.join("; ")), vec![], vec![])
        }
        "R011" => {
            let mut options = vec![];
            for i in &lines {
                let code = claim["lines"][*i]["service_code"].as_str().unwrap_or("");
                let mut near: Vec<(usize, &String)> = cfg.services.iter().map(|s| (levenshtein(&code.to_uppercase(), s), s)).filter(|(d, _)| *d <= 4).collect();
                near.sort();
                for (_, s) in near.into_iter().take(3) {
                    options.push(format!("{}: {s}", line_label(claim, *i)));
                }
            }
            let list = cfg.services.iter().cloned().collect::<Vec<_>>().join(", ");
            suggestion(finding, "verify", format!("The service code is not in the catalogue. Catalogue codes: {list}. Check the intended code on the bill; the closest ones are listed as options, not applied."), vec![], options)
        }
        "R012" => {
            let sum: Option<BigDecimal> = claim["lines"].as_array().into_iter().flatten().map(|l| num_of(&l["net_amount"])).sum::<Option<BigDecimal>>();
            match sum {
                Some(s) => {
                    let edits = vec![edit("/total_amount", &claim["total_amount"], money(&s), "sum of the line amounts")];
                    suggestion(finding, "correct_value", format!("The claim total {} does not equal the sum of the line amounts, {}. If the lines are right, the total is wrong. Check R007 first: if a line amount is wrong, fix that instead.", claim["total_amount"], money_text(&s)), edits, vec![])
                }
                _ => suggestion(finding, "request_information", "The claim total cannot be reconciled because a line amount is missing.".into(), vec![], vec![]),
            }
        }
        "R013" => {
            let mut parts = vec![];
            for i in &lines {
                let l = &claim["lines"][*i];
                let code = l["service_code"].as_str().unwrap_or("");
                let label = line_label(claim, *i);
                if let (Some(q), Some(max)) = (num_of(&l["quantity"]), policy.and_then(|p| p.max_quantity_per_line.get(code)).map(|n| n.to_decimal())) {
                    if q > max {
                        parts.push(format!("{label}: quantity {} is above the limit of {} for {code}", money_text(&q), money_text(&max)));
                    }
                    if q <= BigDecimal::from(0) || q.normalized().fractional_digit_count() > 0 {
                        parts.push(format!("{label}: quantity must be a positive whole number"));
                    }
                }
                if let (Some(p), Some(max)) = (num_of(&l["unit_price"]), policy.and_then(|p| p.max_unit_price.get(code)).map(|n| n.to_decimal())) {
                    if p > max {
                        parts.push(format!("{label}: unit price {} is above the limit of {} for {code}", money_text(&p), money_text(&max)));
                    }
                    if p <= BigDecimal::from(0) {
                        parts.push(format!("{label}: unit price must be greater than zero"));
                    }
                }
            }
            suggestion(finding, "verify", format!("A quantity or price is outside the limits: {}. Check the billed values; they are not changed for you.", if parts.is_empty() { "see the evidence".into() } else { parts.join("; ") }), vec![], vec![])
        }
        "R014" => {
            let latest = claim["lines"].as_array().into_iter().flatten().filter_map(|l| l["service_date"].as_str()).filter_map(Day::parse).max();
            let sub = claim["submission_date"].as_str().and_then(Day::parse);
            let lag = latest.zip(sub).map(|(l, s)| s.days_since(&l));
            let window = policy.map(|p| p.submission_window_days);
            suggestion(finding, "verify", format!("The claim was submitted {} days after the latest service; the window for {} is {} days. Check the dates, or send it for a person to decide on a late-submission exception.", lag.map(|d| d.to_string()).unwrap_or("?".into()), claim["policy_id"].as_str().unwrap_or("?"), window.map(|w| w.to_string()).unwrap_or("?".into())), vec![], vec![])
        }
        "R015" => {
            let want = policy.map(|p| p.currency.clone()).unwrap_or_default();
            let edits = if want.is_empty() { vec![] } else { vec![edit("/currency", &claim["currency"], json!(want), "the policy's currency")] };
            suggestion(finding, "correct_value", format!("The claim is in {}, the policy pays in {want}. Change it only if the source bill is in {want}; no conversion is done.", claim["currency"].as_str().unwrap_or("?")), edits, vec![])
        }
        _ => suggestion(finding, "verify", finding["corrective_action"].as_str().unwrap_or("Review this finding.").to_string(), vec![], vec![]),
    }
}

pub fn suggest(claim: &Value, results: &Value, cfg: &Config) -> Vec<Value> {
    let flagged: Vec<&Value> = results.as_array().into_iter().flatten().filter(|r| is_flagged(r)).collect();
    let mut out: Vec<Value> = flagged.iter().map(|r| suggest_one(claim, r, cfg)).collect();
    // A corrected line amount changes the sum of the lines, so the claim total has to follow or R012 flags it. When R007 proposes
    // corrections, the total is proposed on the corrected lines (R012 flagged), or added to the R007 proposal (R012 passes today).
    let r007_edits: Vec<Change> = out
        .iter()
        .filter(|s| s["rule_id"] == "R007")
        .flat_map(|s| s["proposed_edits"].as_array().cloned().unwrap_or_default())
        .map(|e| Change { path: e["path"].as_str().unwrap_or("").to_string(), value: e["to"].clone() })
        .collect();
    if r007_edits.is_empty() {
        return out;
    }
    let Ok((assumed, _)) = apply(claim, &r007_edits) else { return out };
    let new_sum: Option<BigDecimal> = assumed["lines"].as_array().into_iter().flatten().map(|l| num_of(&l["net_amount"])).sum::<Option<BigDecimal>>();
    let Some(new_sum) = new_sum else { return out };
    let r012 = flagged.iter().position(|r| r["rule_id"] == "R012");
    if let Some(i) = r012 {
        let mut s = suggest_one(&assumed, flagged[i], cfg);
        if num_of(&claim["total_amount"]).is_some_and(|t| cents(&t) == cents(&new_sum)) {
            // the corrected lines already add up to the total as billed: correcting R007 clears this finding by itself
            s["kind"] = json!("verify");
            s["text"] = json!(format!("The claim total {} does not equal the sum of the billed line amounts. If the line amounts are corrected as proposed for R007, they add up to {}, the total as billed, and this finding clears with it. Nothing else needs to change.", claim["total_amount"], money_text(&new_sum)));
            s["proposed_edits"] = json!([]);
            out[i] = s;
            return out;
        }
        s["text"] = json!(format!("If the line amounts are corrected as proposed for R007, they add up to {}; the claim total is {}. {}", money_text(&new_sum), claim["total_amount"], "Change the total to match, or keep the total and recheck the lines against the bill."));
        s["proposed_edits"] = json!([edit("/total_amount", &claim["total_amount"], money(&new_sum), "sum of the corrected line amounts")]);
        out[i] = s;
    } else if let Some(j) = out.iter().position(|s| s["rule_id"] == "R007") {
        if num_of(&claim["total_amount"]).is_some_and(|t| cents(&t) != cents(&new_sum)) {
            let mut edits = out[j]["proposed_edits"].as_array().cloned().unwrap_or_default();
            edits.push(edit("/total_amount", &claim["total_amount"], money(&new_sum), "keeps the total equal to the corrected lines"));
            out[j]["proposed_edits"] = json!(edits);
            let text = format!("{} Correcting the amounts also changes the sum of the lines to {}, so the total is proposed as a second edit; without it R012 would flag the claim.", out[j]["text"].as_str().unwrap_or(""), money_text(&new_sum));
            out[j]["text"] = json!(text);
        }
    }
    out
}

/// A one-screen summary: the headline, one sentence per finding with the rule it rests on, and the order to work in. Findings are put
/// in the order that clears a claim fastest: values the reviewer can correct from the claim itself first, then verifications, then
/// requests for information, high severity before medium within each.
pub fn summary(claim: &Value, results: &Value, suggestions: &[Value]) -> Value {
    let all = results.as_array().cloned().unwrap_or_default();
    let flagged: Vec<&Value> = all.iter().filter(|r| is_flagged(r)).collect();
    if flagged.is_empty() {
        let n = all.len();
        return json!({"headline": format!("Pre-validation clearance: all {n} checks completed and none found a problem. This is not payer approval or a promise of payment."),
            "cleared": true, "counts": {"high": 0, "medium": 0, "unable": 0}, "items": [], "next_steps": [], "claim_id": claim["claim_id"]});
    }
    let high = flagged.iter().filter(|r| r["severity"] == "high").count();
    let unable = flagged.iter().filter(|r| r["status"] == "UNABLE_TO_ASSESS").count();
    let med = flagged.len() - high;
    let order = |s: &Value| -> (u8, u8) {
        let kind = match s["kind"].as_str().unwrap_or("") { "correct_value" => 0, "verify" => 1, _ => 2 };
        (kind, u8::from(s["severity"] != "high"))
    };
    let mut ranked: Vec<&Value> = suggestions.iter().collect();
    ranked.sort_by_key(|s| order(s));
    let items: Vec<Value> = ranked
        .iter()
        .map(|s| {
            let word = if s["status"] == "UNABLE_TO_ASSESS" { "could not be assessed" } else { "failed" };
            json!({"rule_id": s["rule_id"], "severity": s["severity"], "status": s["status"], "kind": s["kind"],
                "sentence": format!("{} {} ({} severity): {}", s["rule_id"].as_str().unwrap_or(""), word, s["severity"].as_str().unwrap_or(""), s["text"].as_str().unwrap_or("")), "cites": s["cites"]})
        })
        .collect();
    let fixable = suggestions.iter().filter(|s| s["kind"] == "correct_value" && s["proposed_edits"].as_array().is_some_and(|e| !e.is_empty())).count();
    let ask = suggestions.iter().filter(|s| s["kind"] == "request_information").count();
    let mut steps = vec![];
    if fixable > 0 {
        steps.push(format!("Try the {fixable} proposed correction(s) in a what-if and check the amounts against the bill."));
    }
    if suggestions.iter().any(|s| s["kind"] == "verify") {
        steps.push("Verify the flagged values against the source record.".to_string());
    }
    if ask > 0 {
        steps.push(format!("Request information from the provider for {ask} finding(s); nothing missing is filled in."));
    }
    steps.push("After a correction, recheck: the claim is validated again as a new version and the original stays on record.".to_string());
    json!({"headline": format!("{} finding(s) need attention: {high} high, {med} medium; {unable} could not be assessed.", flagged.len()),
        "cleared": false, "counts": {"high": high, "medium": med, "unable": unable}, "items": items, "next_steps": steps,
        "fixable_by_edit": fixable, "claim_id": claim["claim_id"]})
}

pub fn bundle(claim: &Value, results: &Value, cfg: &Config) -> Value {
    let s = suggest(claim, results, cfg);
    json!({"summary": summary(claim, results, &s), "suggestions": s})
}

/// Which fields appear in a map, for tests and the console: the rules whose finding carries at least one proposed edit.
pub fn rules_with_edits(suggestions: &[Value]) -> Vec<String> {
    let mut out: Map<String, Value> = Map::new();
    for s in suggestions.iter().filter(|s| s["proposed_edits"].as_array().is_some_and(|e| !e.is_empty())) {
        out.insert(s["rule_id"].as_str().unwrap_or("").to_string(), Value::Null);
    }
    out.keys().cloned().collect()
}

// ---- the request for information that leaves the system ---------------------------------------------------------------------------

/// The message the system drafts for the findings that need something from outside (a provider's missing value, an authorization, a document). It is
/// complete: who it goes to, what is asked, in the rulebook's own words. The officer's only part is to confirm that it is sent. The text names no patient,
/// invents no deadline and promises nothing; it asks for the information and says it is not a decision. None when no finding needs a request.
pub fn info_request(claim: &Value, results: &Value, suggestions: &[Value], version: i64) -> Option<Value> {
    use sha2::{Digest, Sha256};
    let flagged: Vec<&Value> = results.as_array().into_iter().flatten().filter(|r| is_flagged(r)).collect();
    let mut items = vec![];
    for s in suggestions.iter().filter(|s| s["kind"] == "request_information" || s["still_missing"].as_array().is_some_and(|a| !a.is_empty())) {
        let Some(f) = flagged.iter().find(|r| r["rule_id"] == s["rule_id"]) else { continue };
        let still: Vec<String> = s["still_missing"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
        let missing: Vec<String> = if still.is_empty() { missing_paths(f).iter().map(|p| field_label(p)).collect() } else { still };
        let step: String = f["corrective_action"].as_str().unwrap_or("").trim().chars().take(300).collect();
        let ask = if !missing.is_empty() {
            format!("Please send: {}.", missing.join(", "))
        } else if !step.is_empty() {
            step
        } else {
            "Please check this part of the claim and send the correct information.".to_string()
        };
        let lines: Vec<&str> = f["affected_line_ids"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        items.push(json!({"rule_id": s["rule_id"], "severity": s["severity"], "lines": lines, "ask": ask}));
    }
    if items.is_empty() {
        return None;
    }
    let id = claim["claim_id"].as_str().unwrap_or("");
    let mut body = format!("Claim {id} could not be fully validated. Please send the following so that it can be checked again:\n");
    for (n, it) in items.iter().enumerate() {
        let lines = it["lines"].as_array().filter(|l| !l.is_empty()).map(|l| format!(", lines {}", l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")));
        body.push_str(&format!("{}. ({}{}) {}\n", n + 1, it["rule_id"].as_str().unwrap_or(""), lines.unwrap_or_default(), it["ask"].as_str().unwrap_or("")));
    }
    body.push_str("This is a request for information only. It is not a decision on the claim and not a promise of payment.");
    let request_id: String = Sha256::digest(format!("{id}|{version}|{body}").as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    Some(json!({"request_id": request_id, "claim_id": id, "version": version, "to": {"role": "provider", "provider_id": claim["provider_id"]},
        "subject": format!("Claim {id}: information needed"), "body": body, "items": items, "status": "drafted", "channel": "outbox"}))
}
