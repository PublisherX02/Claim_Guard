//! FHIR R4-style Bundle -> normalized ClaimGuard claim envelope: a port of `src/fhir_adapter.py`, written to the same contract.
//!
//! It maps only what the bundle carries; whatever FHIR cannot carry stays null and is listed in `not_carried_by_fhir`, so a rule sees "unknown"
//! (UNABLE_TO_ASSESS) instead of an invented value. Nothing in here panics on a malformed bundle: every unexpected shape becomes an `Err` and the
//! caller quarantines that one record. The Python adapter raises on exactly the same shapes (wrong type where an object or a list is read);
//! `rust/tools/compare_ingest.py` mutates thousands of bundles and checks that the two agree on accept versus quarantine and on every accepted claim.
//!
//! One deliberate difference, always in the safe direction: where Python would silently iterate a string or a dictionary as if it were a list of
//! pre-authorization references, this adapter refuses the bundle.

use crate::pyfmt::{py_repr, py_str, truthy};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

pub const SCHEMA_VERSION: &str = "1.0.0";
pub const NOTES_PLACEHOLDER: &str = "[notes not carried by FHIR bundle]";
pub const NOT_CARRIED_BY_FHIR: [&str; 7] = [
    "authorizations[].patient_id", "authorizations[].service_code", "authorizations[].status", "authorizations[].valid_from", "authorizations[].valid_to",
    "authorizations[].max_quantity", "notes",
];
pub const INFERRED: [(&str, &str); 6] = [
    ("lines[].line_id", "L<Claim.item.sequence>"),
    ("attachments[].service_code", "parsed from DocumentReference.description (\"Synthetic <SERVICE-CODE>\")"),
    ("attachments[].document_status", "DocumentReference.docStatus (preliminary -> draft)"),
    ("attachments[].service_date", "DocumentReference.context.period.start"),
    ("coverage.member_id", "Coverage.subscriberId"),
    ("policy_id", "Coverage.class[type=plan].value"),
];

/// Why a bundle could not be mapped. `Mapping` is the adapter's own "cannot derive a required field" (Python `FhirMappingError`); `Shape` is any
/// other malformed structure (Python raises AttributeError, TypeError and so on and the ingest loop catches them all).
#[derive(Debug, PartialEq)]
pub enum FhirError {
    Mapping(String),
    Shape(String),
}

impl std::fmt::Display for FhirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FhirError::Mapping(m) => write!(f, "{m}"),
            FhirError::Shape(m) => write!(f, "MalformedBundle: {m}"),
        }
    }
}

type R<T> = Result<T, FhirError>;

fn shape<T>(msg: impl Into<String>) -> R<T> {
    Err(FhirError::Shape(msg.into()))
}

fn mapping<T>(msg: impl Into<String>) -> R<T> {
    Err(FhirError::Mapping(msg.into()))
}

static NULL: Value = Value::Null;

fn empty_map() -> &'static Map<String, Value> {
    static E: OnceLock<Map<String, Value>> = OnceLock::new();
    E.get_or_init(Map::new)
}

fn get<'a>(m: &'a Map<String, Value>, k: &str) -> &'a Value {
    m.get(k).unwrap_or(&NULL)
}

/// `x` must be a JSON object (Python: `x.get(...)` on anything else raises).
fn obj<'a>(v: &'a Value, what: &str) -> R<&'a Map<String, Value>> {
    match v {
        Value::Object(m) => Ok(m),
        _ => shape(format!("{what} is not an object")),
    }
}

/// `(x or {})`: a falsy value reads as an empty object, a truthy non-object is an error.
fn or_empty<'a>(v: &'a Value, what: &str) -> R<&'a Map<String, Value>> {
    if truthy(v) { obj(v, what) } else { Ok(empty_map()) }
}

/// `x.get(key, [])` followed by iteration: absent reads as an empty list; a list is iterated; an empty object or string iterates zero times in
/// Python as well; anything else (null, a number, a non-empty string or object) is an error.
fn list_default<'a>(m: &'a Map<String, Value>, key: &str, what: &str) -> R<Vec<&'a Value>> {
    match m.get(key) {
        None => Ok(vec![]),
        Some(Value::Array(a)) => Ok(a.iter().collect()),
        Some(Value::Object(o)) if o.is_empty() => Ok(vec![]),
        Some(Value::String(s)) if s.is_empty() => Ok(vec![]),
        Some(_) => shape(format!("{what} is not a list")),
    }
}

/// `x or []` followed by iteration (elements are read with `.get`, so a non-list truthy value always fails there in Python as well).
fn list_or<'a>(v: &'a Value, what: &str) -> R<Vec<&'a Value>> {
    if !truthy(v) {
        return Ok(vec![]);
    }
    match v {
        Value::Array(a) => Ok(a.iter().collect()),
        _ => shape(format!("{what} is not a list")),
    }
}

fn last_segment(v: &Value) -> R<String> {
    match v {
        Value::String(s) => Ok(s.rsplit('/').next().unwrap_or("").to_string()),
        _ => shape("reference is not text"),
    }
}

fn first_code(codeable: &Value) -> R<Option<Value>> {
    let m = or_empty(codeable, "codeable concept")?;
    for c in list_default(m, "coding", "coding")? {
        let c = obj(c, "coding entry")?;
        if truthy(get(c, "code")) {
            return Ok(Some(get(c, "code").clone()));
        }
    }
    Ok(None)
}

fn ident(resource: &Map<String, Value>, suffix: &str) -> R<Value> {
    for i in list_default(resource, "identifier", "identifier")? {
        let i = obj(i, "identifier entry")?;
        let system = get(i, "system");
        let text = if truthy(system) {
            match system {
                Value::String(s) => s.as_str(),
                _ => return shape("identifier system is not text"),
            }
        } else {
            ""
        };
        if text.ends_with(suffix) {
            return Ok(get(i, "value").clone());
        }
    }
    Ok(Value::Null)
}

fn opt(v: Option<Value>) -> Value {
    v.unwrap_or(Value::Null)
}

/// Python dict/set key for a JSON value: text as itself, other scalars tagged so they never equal a text key; containers are unhashable (error).
fn key_of(v: &Value) -> R<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok("\u{0}None".into()),
        Value::Bool(_) | Value::Number(_) => Ok(format!("\u{0}{}", py_repr(v))),
        _ => shape("unhashable value"),
    }
}

struct Bundle<'a> {
    by_url: HashMap<String, &'a Map<String, Value>>,
    by_type: HashMap<String, Vec<&'a Map<String, Value>>>,
    warnings: Vec<String>,
}

impl<'a> Bundle<'a> {
    fn new(bundle: &'a Map<String, Value>) -> R<Bundle<'a>> {
        if get(bundle, "resourceType") != &Value::String("Bundle".into()) {
            return mapping("Not a FHIR Bundle");
        }
        let mut b = Bundle { by_url: HashMap::new(), by_type: HashMap::new(), warnings: vec![] };
        for e in list_default(bundle, "entry", "entry")? {
            let e = obj(e, "bundle entry")?;
            let r = or_empty(get(e, "resource"), "entry.resource")?;
            let url = get(e, "fullUrl");
            if truthy(url) {
                b.by_url.insert(key_of(url)?, r);
            }
            b.by_type.entry(key_of(get(r, "resourceType"))?).or_default().push(r);
        }
        Ok(b)
    }

    fn of_type(&self, t: &str) -> Vec<&'a Map<String, Value>> {
        self.by_type.get(t).cloned().unwrap_or_default()
    }

    /// (resource or none, its id). Never guesses beyond the trailing id of the reference.
    fn resolve(&mut self, reference_obj: &Value, what: &str) -> R<(Option<&'a Map<String, Value>>, Value)> {
        let m = or_empty(reference_obj, what)?;
        let reference = get(m, "reference");
        if !truthy(reference) {
            return Ok((None, Value::Null));
        }
        let key = key_of(reference)?;
        match self.by_url.get(&key) {
            Some(res) => Ok((Some(*res), get(res, "id").clone())),
            None => {
                self.warnings.push(format!("unresolved_reference: {what} -> {}", py_str(reference)));
                Ok((None, Value::String(last_segment(reference)?)))
            }
        }
    }
}

fn read_encounters(b: &mut Bundle, cl: &Map<String, Value>, patient_id: &Value) -> R<Vec<Value>> {
    let mut out = vec![];
    for enc in b.of_type("Encounter") {
        let (_, enc_patient) = b.resolve(get(enc, "subject"), "Encounter.subject")?;
        let (_, enc_provider) = b.resolve(get(enc, "serviceProvider"), "Encounter.serviceProvider")?;
        let period = or_empty(get(enc, "period"), "Encounter.period")?;
        if truthy(&enc_patient) && truthy(patient_id) && &enc_patient != patient_id {
            b.warnings.push(format!("encounter_patient_mismatch: {} is for {}, claim is for {}", py_str(get(enc, "id")), py_str(&enc_patient), py_str(patient_id)));
        }
        let class = or_empty(get(enc, "class"), "Encounter.class")?;
        out.push(json!({"encounter_id": get(enc, "id"), "status": get(enc, "status"), "class": get(class, "code"), "patient_id": enc_patient, "provider_id": enc_provider,
                        "period_start": get(period, "start"), "period_end": get(period, "end")}));
    }
    let mut known = vec![];
    for e in &out {
        known.push(key_of(&e["encounter_id"])?);
    }
    for it in list_default(cl, "item", "Claim.item")? {
        let it = obj(it, "Claim.item entry")?;
        for r in list_or(get(it, "encounter"), "item.encounter")? {
            let r = obj(r, "item.encounter entry")?;
            let reference = get(r, "reference");
            let seg = if truthy(reference) { last_segment(reference)? } else { String::new() };
            if !known.contains(&seg) {
                b.warnings.push(format!("unresolved_reference: Claim.item.encounter -> {}", py_str(reference)));
            }
        }
    }
    Ok(out)
}

/// Strict base64 (Python `b64decode(validate=True)`): the alphabet only, optional `=` padding at the end, a whole number of quanta.
fn strict_b64(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() {
        return None;
    }
    let b = s.as_bytes();
    let pad = b.iter().rev().take_while(|c| **c == b'=').count();
    if pad > 2 {
        return None;
    }
    let data = &b[..b.len() - pad];
    if data.iter().any(|c| !(c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')) {
        return None;
    }
    if (data.len() + pad) % 4 != 0 || (pad > 0 && data.len() % 4 < 2) || data.len() % 4 == 1 {
        return None;
    }
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            _ => 63,
        }
    };
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut n: u32 = 0;
        for c in chunk {
            n = (n << 6) | val(*c);
        }
        n <<= 6 * (4 - chunk.len()) as u32;
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..1 + (chunk.len() * 6 / 8)]);
    }
    Some(out)
}

fn doc_status(v: &Value) -> R<Option<&'static str>> {
    match v {
        Value::Array(_) | Value::Object(_) => shape("docStatus is unhashable"),
        Value::String(s) if s == "preliminary" => Ok(Some("draft")),
        Value::String(s) if s == "final" => Ok(Some("final")),
        _ => Ok(None),
    }
}

/// `re.fullmatch(r"Synthetic (SVC-[A-Z0-9-]+)", s)` -> the service code.
fn synthetic_service(s: &str) -> Option<&str> {
    let code = s.strip_prefix("Synthetic ")?;
    let rest = code.strip_prefix("SVC-")?;
    if !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-') { Some(code) } else { None }
}

fn first_of<'a>(v: &'a Value, what: &str) -> R<&'a Value> {
    // `(x or [default])[0]` for a value that is truthy: a non-empty list's first element; anything else is an error
    match v {
        Value::Array(a) if !a.is_empty() => Ok(&a[0]),
        _ => shape(format!("{what} is not a non-empty list")),
    }
}

pub fn bundle_to_claim(bundle: &Value) -> R<(Value, Value)> {
    let bm = obj(bundle, "bundle")?;
    let mut b = Bundle::new(bm)?;
    let claims = b.of_type("Claim");
    if claims.len() != 1 {
        return mapping(format!("Expected exactly one Claim, found {}", claims.len()));
    }
    let cl = claims[0];
    if !truthy(get(cl, "id")) {
        return mapping("Claim.id missing");
    }

    let (patient, patient_id) = b.resolve(get(cl, "patient"), "Claim.patient")?;
    let member_id = match patient {
        Some(p) if !p.is_empty() => ident(p, "/member")?,
        _ => Value::Null,
    };
    let (_, provider_id) = b.resolve(get(cl, "provider"), "Claim.provider")?;
    let (_, payer_id) = b.resolve(get(cl, "insurer"), "Claim.insurer")?;

    let insurance_default = [Value::Object(Map::new())];
    let insurance: Vec<&Value> = if truthy(get(cl, "insurance")) {
        match get(cl, "insurance") {
            Value::Array(a) => a.iter().collect(),
            _ => return shape("insurance is not a list"),
        }
    } else {
        insurance_default.iter().collect()
    };
    let ins0 = obj(insurance[0], "insurance entry")?;
    let (cov_res, _) = b.resolve(get(ins0, "coverage"), "Claim.insurance.coverage")?;
    let Some(cov_res) = cov_res else { return mapping("Claim.insurance.coverage does not resolve to a Coverage in the bundle") };
    let mut policy_id = Value::Null;
    for c in list_default(cov_res, "class", "Coverage.class")? {
        let c = obj(c, "Coverage.class entry")?;
        if first_code(get(c, "type"))? == Some(Value::String("plan".into())) {
            policy_id = get(c, "value").clone();
            break;
        }
    }
    let (_, beneficiary_id) = b.resolve(get(cov_res, "beneficiary"), "Coverage.beneficiary")?;
    let period = or_empty(get(cov_res, "period"), "Coverage.period")?;
    let coverage = json!({"coverage_id": get(cov_res, "id"), "status": get(cov_res, "status"), "beneficiary_patient_id": beneficiary_id,
                          "member_id": get(cov_res, "subscriberId"), "start_date": get(period, "start"), "end_date": get(period, "end")});

    let mut lines: Vec<Value> = vec![];
    let mut auth_ids: Vec<Value> = vec![];
    let mut currency_seen = Value::Null;
    for (pos, it) in list_default(cl, "item", "Claim.item")?.into_iter().enumerate() {
        let it = obj(it, "Claim.item entry")?;
        let mut auth = Value::Null;
        for e in list_default(it, "extension", "item.extension")? {
            let e = obj(e, "extension entry")?;
            let url = get(e, "url");
            let text = if truthy(url) {
                match url {
                    Value::String(s) => s.as_str(),
                    _ => return shape("extension url is not text"),
                }
            } else {
                ""
            };
            if text.ends_with("line-authorization-id") {
                auth = get(e, "valueString").clone();
                break;
            }
        }
        if truthy(&auth) && !auth_ids.contains(&auth) {
            auth_ids.push(auth.clone());
        }
        if !truthy(&currency_seen) {
            currency_seen = get(or_empty(get(it, "unitPrice"), "item.unitPrice")?, "currency").clone();
        }
        let seq = match it.get("sequence") {
            Some(v) => py_str(v),
            None => (pos + 1).to_string(),
        };
        let modifier_src = if truthy(get(it, "modifier")) { first_of(get(it, "modifier"), "item.modifier")? } else { &NULL };
        let qty = or_empty(get(it, "quantity"), "item.quantity")?;
        let unit = or_empty(get(it, "unitPrice"), "item.unitPrice")?;
        let net = or_empty(get(it, "net"), "item.net")?;
        lines.push(json!({
            "line_id": format!("L{seq}"),
            "service_code": opt(first_code(get(it, "productOrService"))?),
            "service_date": get(it, "servicedDate"),
            "modifier": opt(first_code(modifier_src)?),
            "quantity": get(qty, "value"),
            "unit_price": get(unit, "value"),
            "net_amount": get(net, "value"),
            "authorization_id": auth,
        }));
    }
    for ins in &insurance {
        let ins = obj(ins, "insurance entry")?;
        for a in list_default(ins, "preAuthRef", "insurance.preAuthRef")? {
            if !auth_ids.contains(a) {
                auth_ids.push(a.clone());
            }
        }
    }
    let authorizations: Vec<Value> = auth_ids
        .iter()
        .map(|a| json!({"authorization_id": a, "patient_id": null, "service_code": null, "status": null, "valid_from": null, "valid_to": null, "max_quantity": null}))
        .collect();

    let mut attachments: Vec<Value> = vec![];
    let mut have: Vec<String> = vec![];
    for doc in b.of_type("DocumentReference") {
        let (_, doc_patient) = b.resolve(get(doc, "subject"), "DocumentReference.subject")?;
        let content = get(doc, "content");
        let first = if truthy(content) { obj(first_of(content, "DocumentReference.content")?, "content entry")? } else { empty_map() };
        let attachment = or_empty(get(first, "attachment"), "content.attachment")?;
        let raw = get(attachment, "data");
        let mut text = String::new();
        if truthy(raw) {
            match raw {
                Value::String(s) => match strict_b64(s).map(String::from_utf8) {
                    Some(Ok(t)) => text = t,
                    _ => b.warnings.push(format!("undecodable_attachment: {}", py_str(get(doc, "id")))),
                },
                _ => return shape("attachment data is not text"),
            }
        }
        let desc = get(doc, "description");
        let service = if truthy(desc) {
            match desc {
                Value::String(s) => synthetic_service(s).map(str::to_string),
                _ => return shape("description is not text"),
            }
        } else {
            None
        };
        let status_value = get(doc, "docStatus");
        let mapped = doc_status(status_value)?;
        if mapped.is_none() {
            b.warnings.push(format!("unknown_docStatus: {} = {}", py_str(get(doc, "id")), py_repr(status_value)));
        }
        let status_out = match mapped {
            Some(s) => Value::String(s.into()),
            None => {
                if truthy(status_value) { status_value.clone() } else { Value::String(String::new()) }
            }
        };
        let ctx = or_empty(get(doc, "context"), "DocumentReference.context")?;
        let ctx_period = or_empty(get(ctx, "period"), "context.period")?;
        let start = get(ctx_period, "start");
        let id = get(doc, "id");
        have.push(key_of(id)?);
        let blank = || Value::String(String::new());
        let or_blank = |v: Value| if truthy(&v) { v } else { blank() };
        let type_out = or_blank(opt(first_code(get(doc, "type"))?));
        let patient_out = or_blank(doc_patient.clone());
        let service_out = service.map(Value::String).unwrap_or_else(blank);
        let date_out = or_blank(start.clone());
        attachments.push(json!({
            "attachment_id": id,
            "type": type_out,
            "patient_id": patient_out,
            "service_code": service_out,
            "service_date": date_out,
            "document_status": status_out,
            "text": text,
        }));
    }
    let mut linked: Vec<String> = vec![];
    for s in list_default(cl, "supportingInfo", "Claim.supportingInfo")? {
        let s = obj(s, "supportingInfo entry")?;
        let vr = or_empty(get(s, "valueReference"), "supportingInfo.valueReference")?;
        if truthy(get(vr, "reference")) {
            let seg = last_segment(get(vr, "reference"))?;
            if !linked.contains(&seg) {
                linked.push(seg);
            }
        }
    }
    linked.sort();
    for missing in linked.iter().filter(|m| !have.contains(m)) {
        b.warnings.push(format!("unresolved_reference: Claim.supportingInfo -> {missing} (no DocumentReference in bundle)"));
    }

    let total = or_empty(get(cl, "total"), "Claim.total")?;
    let currency = if truthy(get(total, "currency")) { get(total, "currency").clone() } else { currency_seen.clone() };
    for (name, value) in [("patient", &patient_id), ("provider", &provider_id), ("payer", &payer_id), ("policy", &policy_id), ("currency", &currency), ("submission date", get(cl, "created"))] {
        if !truthy(value) {
            return mapping(format!("Cannot derive required field: {name}"));
        }
    }
    let diagnosis = get(cl, "diagnosis");
    let dx0 = if truthy(diagnosis) { obj(first_of(diagnosis, "Claim.diagnosis")?, "diagnosis entry")? } else { empty_map() };

    let claim = json!({
        "schema_version": SCHEMA_VERSION, "claim_id": get(cl, "id"), "invoice_number": ident(cl, "/invoice")?, "patient_id": patient_id, "member_id": member_id,
        "provider_id": provider_id, "payer_id": payer_id, "policy_id": policy_id, "diagnosis_code": opt(first_code(get(dx0, "diagnosisCodeableConcept"))?),
        "submission_date": get(cl, "created"), "currency": currency, "total_amount": get(total, "value"), "coverage": coverage, "lines": lines,
        "authorizations": authorizations, "attachments": attachments, "notes": NOTES_PLACEHOLDER,
    });
    let encounters = read_encounters(&mut b, cl, &patient_id)?;
    let inferred: Map<String, Value> = INFERRED.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let report = json!({"source_format": "fhir_bundle", "claim_id": get(cl, "id"), "not_carried_by_fhir": NOT_CARRIED_BY_FHIR, "inferred": inferred,
                        "encounters": encounters, "warnings": b.warnings});
    Ok((claim, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> Value {
        json!({"resourceType": "Bundle", "entry": [
            {"fullUrl": "urn:p", "resource": {"resourceType": "Patient", "id": "PAT-1", "identifier": [{"system": "http://x/member", "value": "MEM-1"}]}},
            {"fullUrl": "urn:o", "resource": {"resourceType": "Organization", "id": "EDU-PROV-01"}},
            {"fullUrl": "urn:i", "resource": {"resourceType": "Organization", "id": "EDU-PAYER"}},
            {"fullUrl": "urn:c", "resource": {"resourceType": "Coverage", "id": "COV-1", "status": "active", "subscriberId": "MEM-1", "beneficiary": {"reference": "urn:p"},
                "period": {"start": "2026-01-01", "end": "2026-12-31"}, "class": [{"type": {"coding": [{"code": "plan"}]}, "value": "EDU-BASIC"}]}},
            {"fullUrl": "urn:cl", "resource": {"resourceType": "Claim", "id": "CG-1", "created": "2026-04-01", "patient": {"reference": "urn:p"}, "provider": {"reference": "urn:o"},
                "insurer": {"reference": "urn:i"}, "insurance": [{"coverage": {"reference": "urn:c"}, "preAuthRef": ["AUTH-1"]}],
                "identifier": [{"system": "http://x/invoice", "value": "INV-1"}], "total": {"value": 100, "currency": "SAR"},
                "diagnosis": [{"diagnosisCodeableConcept": {"coding": [{"code": "DX-EDU-01"}]}}],
                "item": [{"sequence": 1, "productOrService": {"coding": [{"code": "SVC-CONSULT"}]}, "servicedDate": "2026-03-30", "quantity": {"value": 1},
                          "unitPrice": {"value": 100, "currency": "SAR"}, "net": {"value": 100}}]}}]})
    }

    #[test]
    fn a_complete_bundle_maps_and_passes_the_transport_contract() {
        let (claim, report) = bundle_to_claim(&bundle()).unwrap();
        assert_eq!(claim["claim_id"], "CG-1");
        assert_eq!(claim["policy_id"], "EDU-BASIC");
        assert_eq!(claim["lines"][0]["line_id"], "L1");
        assert_eq!(claim["authorizations"][0]["authorization_id"], "AUTH-1");
        assert_eq!(report["warnings"], json!([]));
        assert!(cg_core::transport::validate(&claim).is_ok(), "{:?}", cg_core::transport::validate(&claim));
    }

    #[test]
    fn every_shape_error_is_an_error_not_a_panic() {
        let junk = [json!(null), json!(""), json!(0), json!(-1), json!(1.5), json!(true), json!([]), json!({}), json!("x"), json!([null]), json!(["a"]), json!({"a": {"b": {}}})];
        let base = bundle();
        // replace every value in the bundle, one at a time, with every kind of junk
        fn paths(v: &Value, here: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
            match v {
                Value::Object(m) => m.iter().for_each(|(k, c)| {
                    here.push(k.clone());
                    out.push(here.clone());
                    paths(c, here, out);
                    here.pop();
                }),
                Value::Array(a) => a.iter().enumerate().for_each(|(i, c)| {
                    here.push(i.to_string());
                    out.push(here.clone());
                    paths(c, here, out);
                    here.pop();
                }),
                _ => {}
            }
        }
        let mut all = vec![];
        paths(&base, &mut vec![], &mut all);
        let (mut ok, mut bad) = (0, 0);
        for p in &all {
            for j in &junk {
                let mut b = base.clone();
                let mut cur = &mut b;
                for (n, k) in p.iter().enumerate() {
                    let last = n + 1 == p.len();
                    cur = match cur {
                        Value::Object(m) => {
                            if last {
                                m.insert(k.clone(), j.clone());
                                break;
                            }
                            m.get_mut(k).unwrap()
                        }
                        Value::Array(a) => {
                            let i: usize = k.parse().unwrap();
                            if last {
                                a[i] = j.clone();
                                break;
                            }
                            &mut a[i]
                        }
                        _ => unreachable!(),
                    };
                }
                match bundle_to_claim(&b) {
                    Ok((c, _)) => {
                        ok += 1;
                        let _ = cg_core::transport::validate(&c);
                    }
                    Err(_) => bad += 1,
                }
            }
        }
        assert!(ok > 100 && bad > 100, "{ok} {bad}");
    }

    #[test]
    fn base64_is_strict_like_python() {
        assert_eq!(strict_b64("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(strict_b64("aGVsbG8"), None);
        assert_eq!(strict_b64("aGVs bG8="), None);
        assert_eq!(strict_b64("A==="), None);
        assert_eq!(strict_b64(""), Some(vec![]));
        assert_eq!(strict_b64("QQ=="), Some(b"A".to_vec()));
        assert_eq!(strict_b64("é"), None);
    }

    #[test]
    fn no_claim_or_two_claims_are_refused() {
        let mut b = bundle();
        b["entry"].as_array_mut().unwrap().pop();
        assert_eq!(bundle_to_claim(&b), Err(FhirError::Mapping("Expected exactly one Claim, found 0".into())));
        assert!(matches!(bundle_to_claim(&json!({"resourceType": "Patient"})), Err(FhirError::Mapping(_))));
        assert!(matches!(bundle_to_claim(&json!([1])), Err(FhirError::Shape(_))));
    }
}
