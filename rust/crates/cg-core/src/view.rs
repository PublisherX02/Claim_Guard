//! `facts_extractor.rule_view`: the claim as the rules see it.
//!
//! A value of the wrong type (a number where text belongs, a string where a number belongs, a bool, a non-finite number), a missing key
//! and a row that is not an object all become "unknown" (`None`), so a malformed claim degrades to UNABLE_TO_ASSESS and never to a
//! crash or a silent PASS. Evidence is still read from the original claim, so a reviewer sees what was submitted.

use crate::Num;
use serde_json::{Map, Value};

fn s(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_string)
}

fn n(row: &Map<String, Value>, key: &str) -> Option<Num> {
    row.get(key).and_then(Num::from_json)
}

#[derive(Debug, Clone, Default)]
pub struct CoverageView {
    pub status: Option<String>,
    pub beneficiary_patient_id: Option<String>,
    pub member_id: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LineView {
    pub line_id: String,
    pub service_code: Option<String>,
    pub service_date: Option<String>,
    pub modifier: Option<String>,
    pub authorization_id: Option<String>,
    pub quantity: Option<Num>,
    pub unit_price: Option<Num>,
    pub net_amount: Option<Num>,
}

#[derive(Debug, Clone, Default)]
pub struct AuthView {
    pub authorization_id: Option<String>,
    pub patient_id: Option<String>,
    pub service_code: Option<String>,
    pub status: Option<String>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub max_quantity: Option<Num>,
}

#[derive(Debug, Clone, Default)]
pub struct AttachmentView {
    pub kind: Option<String>,
    pub patient_id: Option<String>,
    pub service_code: Option<String>,
    pub service_date: Option<String>,
    pub document_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct View {
    pub claim_id: String,
    pub invoice_number: Option<String>,
    pub patient_id: Option<String>,
    pub member_id: Option<String>,
    pub provider_id: Option<String>,
    pub policy_id: Option<String>,
    pub diagnosis_code: Option<String>,
    pub submission_date: Option<String>,
    pub currency: Option<String>,
    pub total_amount: Option<Num>,
    pub coverage: CoverageView,
    pub lines: Vec<LineView>,
    pub authorizations: Vec<AuthView>,
    pub attachments: Vec<AttachmentView>,
}

impl View {
    /// Builds the view of an object claim (a claim that passed `transport::validate`; anything else is the caller's fail-closed case).
    pub fn of(claim: &Value) -> Option<View> {
        let c = claim.as_object()?;
        let empty = Map::new();
        let cov = c.get("coverage").and_then(Value::as_object).unwrap_or(&empty);
        let rows = |key: &str| -> Vec<&Map<String, Value>> {
            match c.get(key) {
                Some(Value::Array(items)) => items.iter().map(|r| r.as_object().unwrap_or(&empty)).collect(),
                _ => vec![],
            }
        };
        Some(View {
            claim_id: s(c, "claim_id")?,
            invoice_number: s(c, "invoice_number"),
            patient_id: s(c, "patient_id"),
            member_id: s(c, "member_id"),
            provider_id: s(c, "provider_id"),
            policy_id: s(c, "policy_id"),
            diagnosis_code: s(c, "diagnosis_code"),
            submission_date: s(c, "submission_date"),
            currency: s(c, "currency"),
            total_amount: n(c, "total_amount"),
            coverage: CoverageView {
                status: s(cov, "status"),
                beneficiary_patient_id: s(cov, "beneficiary_patient_id"),
                member_id: s(cov, "member_id"),
                start_date: s(cov, "start_date"),
                end_date: s(cov, "end_date"),
            },
            lines: rows("lines")
                .into_iter()
                .map(|l| LineView {
                    line_id: s(l, "line_id").unwrap_or_default(),
                    service_code: s(l, "service_code"),
                    service_date: s(l, "service_date"),
                    modifier: s(l, "modifier"),
                    authorization_id: s(l, "authorization_id"),
                    quantity: n(l, "quantity"),
                    unit_price: n(l, "unit_price"),
                    net_amount: n(l, "net_amount"),
                })
                .collect(),
            authorizations: rows("authorizations")
                .into_iter()
                .map(|a| AuthView {
                    authorization_id: s(a, "authorization_id"),
                    patient_id: s(a, "patient_id"),
                    service_code: s(a, "service_code"),
                    status: s(a, "status"),
                    valid_from: s(a, "valid_from"),
                    valid_to: s(a, "valid_to"),
                    max_quantity: n(a, "max_quantity"),
                })
                .collect(),
            attachments: rows("attachments")
                .into_iter()
                .map(|a| AttachmentView {
                    kind: s(a, "type"),
                    patient_id: s(a, "patient_id"),
                    service_code: s(a, "service_code"),
                    service_date: s(a, "service_date"),
                    document_status: s(a, "document_status"),
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wrong_types_become_unknown() {
        let v = View::of(&json!({
            "claim_id": "C", "invoice_number": 5, "total_amount": "10", "coverage": {"status": true, "start_date": "2026-01-01"},
            "lines": [{"line_id": "L", "quantity": true, "unit_price": 2.5, "net_amount": null, "service_code": 7}, "not a row"],
            "authorizations": [null], "attachments": [{"type": 3, "document_status": "final"}]
        }))
        .unwrap();
        assert_eq!(v.invoice_number, None);
        assert!(v.total_amount.is_none());
        assert_eq!(v.coverage.status, None);
        assert_eq!(v.coverage.start_date.as_deref(), Some("2026-01-01"));
        assert!(v.lines[0].quantity.is_none(), "a bool is not a number");
        assert_eq!(v.lines[0].unit_price, Some(Num::Float(2.5)));
        assert_eq!(v.lines[0].service_code, None);
        assert_eq!(v.lines[1].line_id, "");
        assert!(v.lines[1].quantity.is_none());
        assert_eq!(v.authorizations.len(), 1);
        assert!(v.authorizations[0].status.is_none());
        assert_eq!(v.attachments[0].kind, None);
        assert_eq!(v.attachments[0].document_status.as_deref(), Some("final"));
    }
}
