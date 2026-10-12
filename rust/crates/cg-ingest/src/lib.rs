//! Claim ingestion: whatever shape a claim arrives in, it ends as either an accepted claim in the ONE internal representation (the normalized
//! envelope, checked by `cg_core::transport::validate`) or a quarantined record with the reason. A bad record never aborts the batch, is never
//! silently dropped and never becomes a "passed" claim.
//!
//! Formats handled here: normalized JSON / JSONL, FHIR R4-style Bundles (JSON / JSONL), the relational CSV export (five files). Documents
//! (PDF, scanned images) are handled by `document` (text acquisition and extraction into a draft claim that a person must confirm).
//! This is the Rust port of `src/ingest.py`; `rust/tools/compare_ingest.py` checks the two against each other.

pub mod csvfmt;
pub mod fhir;
pub mod jsonl;
pub mod pyfmt;

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const NORMALIZED: &str = "normalized_json";
pub const FHIR: &str = "fhir_bundle";
pub const CSV_FOLDER: &str = "csv_folder";

#[derive(Debug, Clone)]
pub struct Ingested {
    pub source_format: String,
    pub source_ref: String,
    pub claim: Option<Value>,
    pub error: Option<String>,
    pub report: Value,
    pub raw: Option<Value>,
}

impl Ingested {
    pub fn accepted(&self) -> bool {
        self.claim.is_some()
    }

    fn rejected(fmt: &str, source_ref: &str, error: String, raw: Option<Value>) -> Ingested {
        Ingested { source_format: fmt.into(), source_ref: source_ref.into(), claim: None, error: Some(error), report: json!({}), raw }
    }
}

/// FHIR when the first 2000 characters hold both `"resourceType"` and `"Bundle"`, otherwise normalized JSON.
pub fn detect_format(bytes: &[u8]) -> &'static str {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(&body[..body.len().min(8000)]);
    let head: String = text.trim_start().chars().take(2000).collect();
    if head.contains("\"resourceType\"") && head.contains("\"Bundle\"") { FHIR } else { NORMALIZED }
}

/// The common gate: whatever the source, the result must pass the transport contract.
fn finish(mut item: Ingested) -> Ingested {
    if let Some(claim) = &item.claim {
        if let Err(e) = cg_core::transport::validate(claim) {
            item.error = Some(format!("Failed transport validation: {e}"));
            if item.raw.is_none() {
                item.raw = Some(claim.clone());
            }
            item.claim = None;
        }
    }
    item
}

/// One JSON / JSONL file (`name` is used only in the source references).
pub fn ingest_json_bytes(bytes: &[u8], name: &str, fmt: Option<&str>) -> Vec<Ingested> {
    let fmt = fmt.unwrap_or_else(|| detect_format(bytes));
    jsonl::records(bytes, name)
        .into_iter()
        .map(|(source_ref, rec, err)| {
            if let Some(e) = err {
                return Ingested::rejected(fmt, &source_ref, e, None);
            }
            let rec = rec.unwrap_or(Value::Null);
            if !rec.is_object() {
                let msg = format!("Expected a JSON object, got {}", jsonl::py_type_name(&rec));
                return Ingested::rejected(fmt, &source_ref, msg, Some(rec));
            }
            if fmt == FHIR {
                return match fhir::bundle_to_claim(&rec) {
                    Ok((claim, report)) => finish(Ingested { source_format: fmt.into(), source_ref, claim: Some(claim), error: None, report, raw: Some(rec) }),
                    Err(e) => Ingested::rejected(fmt, &source_ref, format!("FHIR mapping failed: {e}"), Some(rec)),
                };
            }
            finish(Ingested { source_format: fmt.into(), source_ref, claim: Some(rec.clone()), error: None, report: json!({"source_format": fmt}), raw: Some(rec) })
        })
        .collect()
}

/// The five CSV tables (file name without `.csv` -> bytes).
pub fn ingest_csv_files(files: &BTreeMap<String, Vec<u8>>, label: &str) -> Vec<Ingested> {
    match csvfmt::convert(files) {
        Err(e) => vec![Ingested::rejected(CSV_FOLDER, label, format!("CSV folder could not be read: {e}"), None)],
        Ok(claims) => claims
            .into_iter()
            .map(|c| {
                let id = c.get("claim_id").map(pyfmt::py_str).unwrap_or_else(|| "None".into());
                finish(Ingested { source_format: CSV_FOLDER.into(), source_ref: format!("{label}/claims.csv:{id}"), claim: Some(c), error: None, report: json!({"source_format": CSV_FOLDER}), raw: None })
            })
            .collect(),
    }
}

fn count_by_prefix<'a>(items: impl Iterator<Item = &'a str>) -> Map<String, Value> {
    let mut m: BTreeMap<String, i64> = BTreeMap::new();
    for t in items {
        *m.entry(t.split(':').next().unwrap_or("").to_string()).or_insert(0) += 1;
    }
    m.into_iter().map(|(k, v)| (k, json!(v))).collect()
}

/// The ingest summary (same fields as the Python `summarize`).
pub fn summarize(items: &[Ingested]) -> Value {
    let quarantined: Vec<&Ingested> = items.iter().filter(|i| !i.accepted()).collect();
    let warnings: Vec<&str> = items.iter().flat_map(|i| i.report["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str)).collect();
    let mut not_carried: Vec<String> = items.iter().flat_map(|i| i.report["not_carried_by_fhir"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string)).collect();
    not_carried.sort();
    not_carried.dedup();
    let mut inferred: Vec<String> = items.iter().flat_map(|i| i.report["inferred"].as_object().into_iter().flat_map(|m| m.keys().cloned())).collect();
    inferred.sort();
    inferred.dedup();
    let encounters: usize = items.iter().map(|i| i.report["encounters"].as_array().map(Vec::len).unwrap_or(0)).sum();
    json!({
        "source_format": items.first().map(|i| i.source_format.clone()),
        "records": items.len(),
        "accepted": items.len() - quarantined.len(),
        "quarantined": quarantined.len(),
        "quarantine_reasons": count_by_prefix(quarantined.iter().filter_map(|i| i.error.as_deref())),
        "warnings": count_by_prefix(warnings.into_iter()),
        "not_carried_by_source": not_carried,
        "inferred_fields": inferred,
        "encounters_carried": encounters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_detection() {
        assert_eq!(detect_format(b"{\"resourceType\": \"Bundle\", \"entry\": []}"), FHIR);
        assert_eq!(detect_format(b"\xEF\xBB\xBF  {\"claim_id\": \"x\"}"), NORMALIZED);
        assert_eq!(detect_format(b""), NORMALIZED);
    }

    #[test]
    fn a_mixed_file_never_loses_a_neighbour() {
        let data = b"{\"resourceType\":\"Bundle\",\"entry\":[]}\n\"just a string\"\n[1,2]\n{\"resourceType\":\"Bundle\",\"entry\":\"x\"}\n{\"resourceType\":\"Bundle\",\"entry\":[null,5,\"y\"]}\n";
        let items = ingest_json_bytes(data, "mixed.jsonl", Some(FHIR));
        assert_eq!(items.len(), 5);
        assert!(items.iter().all(|i| !i.accepted()));
        assert!(items[1].error.as_ref().unwrap().contains("got str"));
        assert!(items[2].error.as_ref().unwrap().contains("got list"));
        let s = summarize(&items);
        assert_eq!(s["records"], 5);
        assert_eq!(s["quarantined"], 5);
    }

    #[test]
    fn a_normalized_claim_that_breaks_the_contract_is_quarantined_with_the_reason() {
        let items = ingest_json_bytes(b"{\"claim_id\": \"C1\"}\n", "c.jsonl", None);
        assert_eq!(items.len(), 1);
        assert!(!items[0].accepted());
        assert!(items[0].error.as_ref().unwrap().starts_with("Failed transport validation"));
        assert!(items[0].raw.is_some());
    }
}
