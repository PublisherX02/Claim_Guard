//! Triage: where a claim goes, decided by a published formula and written down as a receipt (`src/workqueue/triage.py`).
//!
//! A finding is *flagged* when its status is FAIL or UNABLE_TO_ASSESS. Each flagged finding scores points from the routing
//! configuration (severity-weighted), and the claim lands in one lane:
//!
//! * green: nothing flagged
//! * A: one to three flagged, score below the lane-B score
//! * B: `lane_b_flagged` or more flagged, or a score at or above `lane_b_score`
//!
//! Who may take the claim is separate: any flagged high-severity finding needs `claims.decide_high`, everything else `claims.decide`.
//! The result set is checked before it is trusted. If it is not exactly the 15 official rules, each once, with known statuses and
//! severities, it is *degraded* and fails closed to lane B with senior eligibility; a damaged result set can never look clean.

use crate::routing_config::RoutingConfig;
use cg_audit::canonical::digest;
use serde_json::{json, Map, Value};

pub const FLAGGED: [&str; 2] = ["FAIL", "UNABLE_TO_ASSESS"];
const KNOWN_STATUSES: [&str; 4] = ["PASS", "FAIL", "UNABLE_TO_ASSESS", "NOT_APPLICABLE"];
const KNOWN_SEVERITIES: [&str; 2] = ["high", "medium"];

pub fn rule_ids() -> Vec<String> {
    (1..=15).map(|i| format!("R{i:03}")).collect()
}

pub fn input_hash(claim: &Value) -> String {
    digest(claim)
}

fn str_key(v: &Value) -> String {
    // Python's str(row.get('rule_id')): text as is, anything else by its repr; only the order of a damaged result set depends on it
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

fn sorted_rows(results: &Value) -> Vec<&Value> {
    let mut rows: Vec<&Value> = results.as_array().map(|a| a.iter().filter(|r| r.is_object()).collect()).unwrap_or_default();
    rows.sort_by_key(|r| str_key(&r["rule_id"]));
    rows
}

pub fn result_hash(results: &Value) -> String {
    match results.as_array() {
        Some(a) if a.iter().all(Value::is_object) => digest(&Value::Array(sorted_rows(results).into_iter().cloned().collect())),
        _ => digest(results),
    }
}

/// The engine does not expose its facts blob; the evidence and affected lines it derived from the facts are what a later replay compares.
pub fn facts_hash(results: &Value) -> String {
    digest(&Value::Array(sorted_rows(results).into_iter().map(|r| json!([r.get("rule_id"), r.get("affected_line_ids"), r.get("evidence")])).collect()))
}

pub fn degraded(results: &Value) -> bool {
    let Some(rows) = results.as_array() else { return true };
    if rows.len() != 15 {
        return true;
    }
    let mut seen = vec![];
    for r in rows {
        let ok = r.is_object() && r["status"].as_str().is_some_and(|s| KNOWN_STATUSES.contains(&s)) && r["severity"].as_str().is_some_and(|s| KNOWN_SEVERITIES.contains(&s));
        if !ok {
            return true;
        }
        seen.push(str_key(&r["rule_id"]));
    }
    seen.sort();
    seen != rule_ids()
}

pub fn flagged(results: &Value) -> Vec<&Value> {
    results.as_array().map(|a| a.iter().filter(|r| r.is_object() && r["status"].as_str().is_some_and(|s| FLAGGED.contains(&s))).collect()).unwrap_or_default()
}

fn severity(row: &Value) -> &str {
    row["severity"].as_str().filter(|s| KNOWN_SEVERITIES.contains(s)).unwrap_or("high") // unknown counts as the most serious
}

pub fn score(results: &Value, cfg: &RoutingConfig) -> i64 {
    flagged(results).into_iter().map(|r| cfg.points.get(r["status"].as_str().unwrap_or("FAIL"), severity(r))).sum()
}

pub fn lane(results: &Value, cfg: &RoutingConfig) -> &'static str {
    if degraded(results) {
        return "B";
    }
    let rows = flagged(results);
    if rows.is_empty() {
        return "green";
    }
    if rows.len() as i64 >= cfg.lane_b_flagged || score(results, cfg) >= cfg.lane_b_score {
        "B"
    } else {
        "A"
    }
}

pub fn eligibility(results: &Value) -> &'static str {
    if degraded(results) || flagged(results).into_iter().any(|r| severity(r) == "high") {
        "decide_high"
    } else {
        "decide"
    }
}

pub fn make_receipt(claim: &Value, results: &Value, cfg: &RoutingConfig, rule_pack_hash: &str, engine_version: &str, now: f64) -> Value {
    let mut statuses = Map::new();
    for r in sorted_rows(results) {
        if let Some(id) = r["rule_id"].as_str() {
            statuses.insert(id.to_string(), r.get("status").cloned().unwrap_or(Value::Null));
        }
    }
    json!({
        "claim_id": claim.get("claim_id"),
        "input_hash": input_hash(claim),
        "rule_pack_hash": rule_pack_hash,
        "engine_version": engine_version,
        "facts_hash": facts_hash(results),
        "result_hash": result_hash(results),
        "statuses": statuses,
        "score": score(results, cfg),
        "lane": lane(results, cfg),
        "eligibility": eligibility(results),
        "config_version": cfg.version,
        "degraded": degraded(results),
        "created_at": now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(flags: &[(&str, &str, &str)]) -> Value {
        let mut rows: Vec<Value> = rule_ids().into_iter().map(|id| json!({"rule_id": id, "status": "PASS", "severity": "medium", "affected_line_ids": [], "evidence": []})).collect();
        for (id, status, sev) in flags {
            let i = rule_ids().iter().position(|r| r == id).unwrap();
            rows[i]["status"] = json!(status);
            rows[i]["severity"] = json!(sev);
        }
        Value::Array(rows)
    }

    #[test]
    fn lanes_follow_the_published_formula() {
        let cfg = RoutingConfig::default();
        assert_eq!(lane(&results(&[]), &cfg), "green");
        assert_eq!(lane(&results(&[("R001", "FAIL", "medium")]), &cfg), "A");
        assert_eq!(score(&results(&[("R001", "FAIL", "high"), ("R002", "UNABLE_TO_ASSESS", "medium")]), &cfg), 5);
        assert_eq!(lane(&results(&[("R001", "FAIL", "medium"), ("R002", "FAIL", "medium"), ("R003", "FAIL", "medium"), ("R004", "FAIL", "medium")]), &cfg), "B", "four flagged");
        assert_eq!(lane(&results(&[("R001", "FAIL", "high"), ("R002", "FAIL", "high"), ("R003", "FAIL", "medium")]), &cfg), "B", "score 10");
    }

    #[test]
    fn eligibility_needs_a_senior_for_any_high_finding() {
        assert_eq!(eligibility(&results(&[("R001", "FAIL", "medium")])), "decide");
        assert_eq!(eligibility(&results(&[("R001", "UNABLE_TO_ASSESS", "high")])), "decide_high");
        assert_eq!(eligibility(&results(&[])), "decide");
    }

    #[test]
    fn a_damaged_result_set_fails_closed() {
        let cfg = RoutingConfig::default();
        for bad in [json!([]), json!("x"), json!(null), json!({"a": 1})] {
            assert!(degraded(&bad));
            assert_eq!((lane(&bad, &cfg), eligibility(&bad)), ("B", "decide_high"));
        }
        let mut short = results(&[]);
        short.as_array_mut().unwrap().pop();
        assert!(degraded(&short));
        let mut dup = results(&[]);
        dup[14]["rule_id"] = json!("R001");
        assert!(degraded(&dup));
        let mut unknown = results(&[]);
        unknown[3]["status"] = json!("MAYBE");
        assert!(degraded(&unknown));
        let mut sev = results(&[]);
        sev[3]["severity"] = json!("low");
        assert!(degraded(&sev));
        assert!(!degraded(&results(&[])));
    }

    #[test]
    fn an_unknown_severity_counts_as_high() {
        let cfg = RoutingConfig::default();
        let mut r = results(&[("R001", "FAIL", "medium")]);
        r[0]["severity"] = json!("bogus");
        assert_eq!(score(&r, &cfg), 4);
    }

    #[test]
    fn the_receipt_names_everything_needed_to_replay_the_decision() {
        let claim = json!({"claim_id": "C1", "x": 1});
        let rec = make_receipt(&claim, &results(&[("R001", "FAIL", "high")]), &RoutingConfig::default(), "pack", "eng", 5.0);
        assert_eq!((rec["lane"].as_str(), rec["score"].as_i64(), rec["eligibility"].as_str(), rec["config_version"].as_i64(), rec["degraded"].as_bool()), (Some("A"), Some(4), Some("decide_high"), Some(1), Some(false)));
        assert_eq!(rec["statuses"]["R001"], "FAIL");
        assert_eq!(rec["input_hash"], digest(&claim));
        let rec2 = make_receipt(&claim, &results(&[("R001", "FAIL", "high")]), &RoutingConfig::default(), "pack", "eng", 9.0);
        assert_eq!((&rec["result_hash"], &rec["facts_hash"]), (&rec2["result_hash"], &rec2["facts_hash"]), "hashes do not depend on the time");
    }
}
