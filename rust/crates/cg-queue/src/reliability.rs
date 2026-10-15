//! Measured rule reliability: the share of a rule's flagged findings that reviewers uphold, with an exact interval, learned from the
//! decisions people already make (docs/superpowers/specs/2026-10-14-hitl-confidence-and-learning-design.md).
//!
//! Counts and intervals only; no trained model. A rule is `doubtful` when we are 95 % sure that fewer than `doubtful_below` of its findings
//! are upheld, which sends later findings of that rule to a senior. Nothing here changes a verdict.

use crate::routing_config::RoutingConfig;
use crate::service::RESOLVING;
use crate::shadow::clopper_pearson;
use crate::store::QueueStore;
use crate::triage;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub const CONFIDENCE: f64 = 0.95;
const TOLERANCE: f64 = 1e-9;

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub min_decisions: i64,
    pub min_reviewers: i64,
    pub reviewer_cap: f64,
    pub doubtful_below: f64,
}

impl Params {
    pub fn from_config(c: &RoutingConfig) -> Params {
        Params { min_decisions: c.reliability_min_decisions, min_reviewers: c.reliability_min_reviewers, reviewer_cap: c.reliability_reviewer_cap, doubtful_below: c.doubtful_below }
    }

    pub fn to_doc(&self) -> Value {
        json!({"min_decisions": self.min_decisions, "min_reviewers": self.min_reviewers, "reviewer_cap": self.reviewer_cap, "doubtful_below": self.doubtful_below, "confidence": CONFIDENCE})
    }

    pub fn from_doc(v: &Value) -> Result<Params, String> {
        let int = |k: &str, low: i64, high: i64| v[k].as_i64().filter(|x| v[k].is_i64() && (low..=high).contains(x)).ok_or(format!("parameter {k} is invalid"));
        let float = |k: &str, low: f64, high: f64| v[k].as_f64().filter(|x| x.is_finite() && (low..=high).contains(x)).ok_or(format!("parameter {k} is invalid"));
        if v["confidence"].as_f64() != Some(CONFIDENCE) {
            return Err("parameter confidence is invalid".into());
        }
        Ok(Params {
            min_decisions: int("min_decisions", 5, 1000)?,
            min_reviewers: int("min_reviewers", 1, 50)?,
            reviewer_cap: float("reviewer_cap", 0.1, 1.0)?,
            doubtful_below: float("doubtful_below", 0.05, 0.95)?,
        })
    }
}

/// One person's final answer on one flagged rule of one claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Counted {
    pub rule_id: String,
    pub claim_id: String,
    pub version: i64,
    pub actor: String,
    pub at: f64,
    pub upheld: bool,
}

/// For every decided dossier and flagged rule, the last resolving action (confirm = upheld, dismiss = overridden). Information requests and
/// decisions about rules that were not flagged are ignored. Sorted, so the result does not depend on the order of the documents.
pub fn counted_decisions(docs: &[Value]) -> Vec<Counted> {
    let mut out = vec![];
    for doc in docs {
        let flagged: BTreeSet<&str> = triage::flagged(&doc["results"]).into_iter().filter_map(|r| r["rule_id"].as_str()).collect();
        let mut last: BTreeMap<&str, &Value> = BTreeMap::new();
        for d in doc["decisions"].as_array().into_iter().flatten() {
            let (rule, action) = (d["rule_id"].as_str().unwrap_or(""), d["action"].as_str().unwrap_or(""));
            // a bulk confirm is a lighter look, not evidence about the rule: counting it would keep a rule eligible by its own rubber stamp
            if flagged.contains(rule) && RESOLVING.contains(&action) && d["reason"].as_str() != Some(crate::service::BULK_REASON) {
                last.insert(rule, d);
            }
        }
        for (rule, d) in last {
            out.push(Counted {
                rule_id: rule.into(),
                claim_id: doc["claim_id"].as_str().unwrap_or("").into(),
                version: doc["version"].as_i64().unwrap_or(0),
                actor: d["actor"].as_str().unwrap_or("").into(),
                at: d["at"].as_f64().filter(|t| t.is_finite()).unwrap_or(0.0),
                upheld: d["action"] == "confirm_issue",
            });
        }
    }
    out.sort_by(|a, b| (&a.rule_id, &a.claim_id, a.version).cmp(&(&b.rule_id, &b.claim_id, b.version)));
    out
}

pub fn status_of(n: i64, reviewers: i64, upper: f64, p: &Params) -> &'static str {
    if n < p.min_decisions || reviewers < p.min_reviewers {
        "unmeasured"
    } else if upper < p.doubtful_below {
        "doubtful"
    } else {
        "measured"
    }
}

/// The figures for one rule. Everything except the counts is derived, which is what `verify` re-derives.
pub fn row(upheld: i64, overridden: i64, reviewers: i64, p: &Params) -> Value {
    let n = upheld + overridden;
    let (lower, upper) = clopper_pearson(upheld, n, CONFIDENCE).unwrap_or((0.0, 1.0));
    let rate = if n > 0 { upheld as f64 / n as f64 } else { 0.0 };
    json!({"upheld": upheld, "overridden": overridden, "n": n, "reviewers": reviewers, "rate": rate, "lower": lower, "upper": upper, "width": upper - lower,
           "status": status_of(n, reviewers, upper, p)})
}

/// One reviewer may supply at most `max(1, floor(cap * total))` of a rule's decisions: their earliest by time, claim id and version.
fn capped<'a>(rows: &[&'a Counted], cap: f64) -> Vec<&'a Counted> {
    let limit = ((cap * rows.len() as f64).floor() as usize).max(1);
    let mut by: BTreeMap<&str, Vec<&Counted>> = BTreeMap::new();
    for r in rows {
        by.entry(r.actor.as_str()).or_default().push(r);
    }
    let mut kept = vec![];
    for (_, mut v) in by {
        v.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(Ordering::Equal).then_with(|| a.claim_id.cmp(&b.claim_id)).then(a.version.cmp(&b.version)));
        kept.extend(v.into_iter().take(limit));
    }
    kept
}

fn inputs_hash(counted: &[Counted]) -> String {
    let mut lines: Vec<String> = counted.iter().map(|c| format!("{}|{}|{}|{}|{}", c.rule_id, c.claim_id, c.version, c.actor, c.upheld)).collect();
    lines.sort();
    hash_text(&lines.join("\n"))
}

pub fn hash_text(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// A new snapshot document (status `proposed`). Holds no claim id, badge or reason: counts, figures, parameters and a hash of the inputs.
pub fn make_snapshot(counted: &[Counted], p: &Params, version: i64, now: f64, config_version: i64) -> Value {
    let mut by_rule: BTreeMap<&str, Vec<&Counted>> = BTreeMap::new();
    for c in counted {
        by_rule.entry(c.rule_id.as_str()).or_default().push(c);
    }
    let mut rules = Map::new();
    for (rule, rows) in by_rule {
        let kept = capped(&rows, p.reviewer_cap);
        let upheld = kept.iter().filter(|c| c.upheld).count() as i64;
        let reviewers = kept.iter().map(|c| c.actor.as_str()).collect::<BTreeSet<_>>().len() as i64;
        rules.insert(rule.to_string(), row(upheld, kept.len() as i64 - upheld, reviewers, p));
    }
    json!({"version": version, "status": "proposed", "created_at": now, "config_version": config_version, "parameters": p.to_doc(), "decisions": counted.len(),
           "inputs_hash": inputs_hash(counted), "rules": rules})
}

fn close(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.is_finite() && y.is_finite() && (x - y).abs() < TOLERANCE,
        _ => a == b,
    }
}

/// Recomputes every derived figure from the stored counts and parameters; any difference is reported as tampering.
pub fn verify(snap: &Value) -> Result<(), String> {
    if !snap["version"].as_i64().is_some_and(|v| snap["version"].is_i64() && v >= 1) {
        return Err("the snapshot has no valid version".into());
    }
    let p = Params::from_doc(&snap["parameters"])?;
    let rules = snap["rules"].as_object().ok_or("the snapshot has no rules table")?;
    for (id, r) in rules {
        let count = |k: &str| r[k].as_i64().filter(|v| r[k].is_i64() && *v >= 0).ok_or(format!("{id}: {k} is not a whole number of 0 or more"));
        let (upheld, overridden, reviewers) = (count("upheld")?, count("overridden")?, count("reviewers")?);
        let want = row(upheld, overridden, reviewers, &p);
        for k in ["n", "rate", "lower", "upper", "width", "status"] {
            if !close(&r[k], &want[k]) {
                return Err(format!("{id}: {k} does not follow from the counts"));
            }
        }
    }
    Ok(())
}

pub fn doubtful_rules(snap: &Value) -> BTreeSet<String> {
    snap["rules"].as_object().into_iter().flatten().filter(|(_, r)| r["status"] == "doubtful").map(|(id, _)| id.clone()).collect()
}

/// Sets the reliability fields on a receipt. The snapshot must verify; a refused snapshot changes nothing.
pub fn apply(receipt: &mut Value, results: &Value, snap: &Value) -> Result<(), String> {
    verify(snap)?;
    let rules = snap["rules"].as_object().ok_or("the snapshot has no rules table")?;
    let mut doubtful: Vec<String> = vec![];
    let mut uncertainty = 0.0f64;
    for r in triage::flagged(results) {
        let Some(id) = r["rule_id"].as_str() else { continue };
        match rules.get(id) {
            Some(row) => {
                uncertainty = uncertainty.max(row["width"].as_f64().unwrap_or(1.0));
                if row["status"] == "doubtful" {
                    doubtful.push(id.to_string());
                }
            }
            None => uncertainty = uncertainty.max(1.0),
        }
    }
    doubtful.sort();
    doubtful.dedup();
    receipt["reliability_version"] = snap["version"].clone();
    receipt["uncertainty"] = json!(uncertainty);
    if !doubtful.is_empty() {
        receipt["eligibility"] = json!("decide_high");
    }
    receipt["doubtful_rules"] = json!(doubtful);
    Ok(())
}

/// The reasons a claim needs explicit approval, in plain words and in a fixed order. Never numbers.
pub fn attention(doc: &Value) -> Vec<&'static str> {
    let flagged = triage::flagged(&doc["results"]);
    let mut out = vec![];
    if flagged.iter().any(|r| r["severity"] != "medium") {
        out.push("high_severity");
    }
    if doc["receipt"]["doubtful_rules"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push("low_confidence");
    }
    if flagged.iter().any(|r| r["status"] == "UNABLE_TO_ASSESS") {
        out.push("could_not_assess");
    }
    if doc["receipt"]["degraded"] == true {
        out.push("degraded");
    }
    if doc["escalated"] == true {
        out.push("disagreement");
    }
    out
}

/// The snapshot routing uses: the highest-numbered one that two administrators approved. None when there is none or the store fails.
pub fn active(store: &dyn QueueStore) -> Option<Value> {
    store.reliability_history().ok()?.into_iter().filter(|d| d["status"] == "approved").max_by_key(|d| d["version"].as_i64().unwrap_or(0))
}

const BIG: usize = 100_000;
pub const REPLAY_LIMIT: usize = 500;

/// What would change if this snapshot were routing: the latest claims, re-routed on their stored results. Reads only; the verdict hash is
/// compared claim by claim to show that no verdict moves.
pub fn replay_report(store: &dyn QueueStore, snap: &Value, limit: usize) -> Value {
    let mut docs: Vec<Value> = ["ready", "awaiting_countersign", "leased", "decided"].iter().flat_map(|s| store.by_state(s, BIG).unwrap_or_default()).collect();
    docs.sort_by(|a, b| b["receipt"]["created_at"].as_f64().partial_cmp(&a["receipt"]["created_at"].as_f64()).unwrap_or(Ordering::Equal).then_with(|| a["claim_id"].as_str().cmp(&b["claim_id"].as_str())));
    docs.truncate(limit);
    let (mut before, mut after, mut changed, mut newly, mut intact, mut unc) = (0i64, 0i64, 0i64, 0i64, true, 0.0f64);
    for d in &docs {
        let was = triage::eligibility(&d["results"]) == "decide_high";
        let mut receipt = d["receipt"].clone();
        receipt["eligibility"] = json!(triage::eligibility(&d["results"]));
        if apply(&mut receipt, &d["results"], snap).is_err() {
            continue;
        }
        let now = receipt["eligibility"] == "decide_high";
        before += i64::from(was);
        after += i64::from(now);
        changed += i64::from(was != now);
        newly += i64::from(!was && now);
        unc += receipt["uncertainty"].as_f64().unwrap_or(0.0);
        intact &= triage::result_hash(&d["results"]) == d["receipt"]["result_hash"].as_str().unwrap_or("");
    }
    let n = docs.len() as i64;
    let mut out = json!({"claims": n, "senior_before": before, "senior_after": after, "newly_senior": newly, "changed": changed,
        "senior_share_before": if n > 0 { before as f64 / n as f64 } else { 0.0 }, "senior_share_after": if n > 0 { after as f64 / n as f64 } else { 0.0 },
        "mean_uncertainty": if n > 0 { unc / n as f64 } else { 0.0 }, "verdicts_unchanged": intact});
    if after > before {
        out["note"] = json!(format!("The senior load rises from {before} to {after} of {n} claims under this snapshot. A lower doubtful_below, or leaving route_by_reliability off, limits this."));
    }
    out
}

fn fingerprint(snap: &Value) -> (Value, Value, BTreeMap<String, String>) {
    let statuses = snap["rules"].as_object().into_iter().flatten().map(|(k, r)| (k.clone(), r["status"].as_str().unwrap_or("").to_string())).collect();
    (snap["inputs_hash"].clone(), snap["parameters"].clone(), statuses)
}

/// Computes the next snapshot from the decided dossiers and stores it as a PROPOSAL (with its replay report). Returns None when nothing
/// has been decided, when the newest snapshot (whatever its status) already has the same inputs, parameters and statuses, or when another
/// learner stored the same version first. A proposal changes no routing: see `activate`.
pub fn learn(store: &dyn QueueStore, cfg: &RoutingConfig, now: f64) -> Result<Option<Value>, String> {
    let p = Params::from_config(cfg);
    let decided = store.by_state("decided", BIG).map_err(|e| e.to_string())?;
    let counted = counted_decisions(&decided);
    if counted.is_empty() {
        return Ok(None);
    }
    let history = store.reliability_history().map_err(|e| e.to_string())?;
    let version = history.last().and_then(|d| d["version"].as_i64()).unwrap_or(0) + 1;
    let mut snap = make_snapshot(&counted, &p, version, now, cfg.version);
    if history.last().is_some_and(|last| fingerprint(last) == fingerprint(&snap)) {
        return Ok(None);
    }
    snap["replay"] = replay_report(store, &snap, REPLAY_LIMIT);
    match store.put_reliability(&snap, version - 1) {
        Ok(true) => Ok(Some(snap)),
        Ok(false) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Makes a proposal the one routing uses. The proposal must verify, still be `proposed`, and be newer than the active one; the previous
/// active snapshot is then marked superseded. `approver` is the second administrator (None when a lone administrator acts alone).
pub fn activate(store: &dyn QueueStore, version: i64, requester: &str, approver: Option<&str>, now: f64) -> Result<(), String> {
    let history = store.reliability_history().map_err(|e| e.to_string())?;
    let doc = history.iter().find(|d| d["version"].as_i64() == Some(version)).ok_or("unknown snapshot")?;
    if doc["status"] != "proposed" {
        return Err("only a proposed snapshot can be activated".into());
    }
    verify(doc)?;
    let previous = history.iter().filter(|d| d["status"] == "approved").filter_map(|d| d["version"].as_i64()).max();
    if previous.is_some_and(|v| v > version) {
        return Err("a newer snapshot is already active".into());
    }
    let detail = json!({"requested_by": requester, "approved_by": approver});
    store.set_reliability_status(version, "proposed", "approved", approver.unwrap_or(requester), now, Some(detail)).map_err(|e| e.to_string())?.ok_or("the proposal changed while it was being approved")?;
    // Re-read after our own write and supersede every approved snapshot except the newest. Two activations of different proposals can run
    // at once and each read `previous` before the other wrote; whichever finishes last sees both and leaves exactly one approved.
    let approved: Vec<i64> = store.reliability_history().map_err(|e| e.to_string())?.iter().filter(|d| d["status"] == "approved").filter_map(|d| d["version"].as_i64()).collect();
    let newest = approved.iter().copied().max().unwrap_or(version);
    for old in approved.into_iter().filter(|v| *v != newest) {
        let _ = store.set_reliability_status(old, "approved", "superseded", approver.unwrap_or(requester), now, None); // `active` takes the highest approved version anyway
    }
    Ok(())
}

pub fn reject(store: &dyn QueueStore, version: i64, actor: &str, now: f64, note: &str) -> Result<(), String> {
    store.set_reliability_status(version, "proposed", "rejected", actor, now, Some(json!({"note": note.chars().take(200).collect::<String>()}))).map_err(|e| e.to_string())?.ok_or("only a proposed snapshot can be rejected")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decided(claim: &str, rule: &str, decisions: &[(&str, &str, f64)]) -> Value {
        json!({"claim_id": claim, "version": 1,
               "results": [{"rule_id": rule, "status": "FAIL", "severity": "medium"}],
               "decisions": decisions.iter().map(|(a, who, at)| json!({"rule_id": rule, "action": a, "actor": who, "at": at, "round": 1})).collect::<Vec<_>>()})
    }

    fn params() -> Params {
        Params { min_decisions: 20, min_reviewers: 3, reviewer_cap: 0.4, doubtful_below: 0.6 }
    }

    #[test]
    fn only_the_last_resolving_action_on_a_flagged_rule_counts() {
        let docs = vec![
            decided("C1", "R006", &[("request_information", "A", 1.0), ("dismiss_with_reason", "A", 2.0), ("confirm_issue", "B", 3.0)]),
            decided("C2", "R006", &[("request_information", "A", 1.0)]),
        ];
        let mut stray = decided("C3", "R006", &[("confirm_issue", "A", 1.0)]);
        stray["decisions"].as_array_mut().unwrap().push(json!({"rule_id": "R999", "action": "confirm_issue", "actor": "A", "at": 2.0}));
        let counted = counted_decisions(&[docs[0].clone(), docs[1].clone(), stray]);
        let ids: Vec<(&str, &str, bool)> = counted.iter().map(|c| (c.claim_id.as_str(), c.actor.as_str(), c.upheld)).collect();
        assert_eq!(ids, vec![("C1", "B", true), ("C3", "A", true)], "an information request, an unflagged rule and a second resolution do not add rows");
    }

    #[test]
    fn a_rule_with_few_decisions_or_few_reviewers_is_unmeasured_never_doubtful() {
        let p = params();
        assert_eq!(status_of(19, 5, 0.1, &p), "unmeasured");
        assert_eq!(status_of(40, 2, 0.1, &p), "unmeasured");
        assert_eq!(status_of(40, 3, 0.59, &p), "doubtful");
        assert_eq!(status_of(40, 3, 0.6, &p), "measured", "the upper bound must be strictly below the threshold");
    }

    #[test]
    fn an_overridden_rule_becomes_doubtful_and_a_confirmed_one_does_not() {
        let p = params();
        let weak = row(6, 34, 5, &p); // 15 % upheld of 40
        assert_eq!(weak["status"], "doubtful");
        let strong = row(39, 1, 5, &p);
        assert_eq!(strong["status"], "measured");
        assert!(weak["lower"].as_f64().unwrap() <= weak["rate"].as_f64().unwrap() && weak["rate"].as_f64().unwrap() <= weak["upper"].as_f64().unwrap());
        assert_eq!(row(0, 0, 0, &p)["width"], 1.0, "no decisions: the widest possible interval");
    }

    #[test]
    fn one_reviewer_cannot_swing_a_rule() {
        // 30 dismissals by M, plus 10 confirmations spread over four others: without the cap the rule looks doubtful.
        let mut counted = vec![];
        for i in 0..30 {
            counted.push(Counted { rule_id: "R006".into(), claim_id: format!("M{i}"), version: 1, actor: "M".into(), at: i as f64, upheld: false });
        }
        for (k, who) in ["A", "B", "C", "D"].iter().enumerate() {
            for i in 0..3 {
                counted.push(Counted { rule_id: "R006".into(), claim_id: format!("{who}{i}"), version: 1, actor: (*who).into(), at: (100 + k * 10 + i) as f64, upheld: true });
            }
        }
        let uncapped = make_snapshot(&counted, &Params { reviewer_cap: 1.0, ..params() }, 1, 0.0, 1);
        let capped = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert_eq!(uncapped["rules"]["R006"]["status"], "doubtful");
        // cap: floor(0.4 * 42) = 16 of M's 30 are kept
        assert_eq!(capped["rules"]["R006"]["overridden"], 16);
        assert_eq!(capped["rules"]["R006"]["upheld"], 12);
        assert_ne!(capped["rules"]["R006"]["status"], "doubtful", "16 vs 12 is not 95 % sure that fewer than 60 % are upheld");
    }

    #[test]
    fn a_reviewer_always_keeps_at_least_one_decision_however_small_the_cap_share() {
        // two decisions by two people with a 0.4 cap: floor(0.4 * 2) = 0, but each person's one decision must still count
        let counted: Vec<Counted> = ["A", "B"].iter().enumerate().map(|(i, who)| Counted { rule_id: "R006".into(), claim_id: format!("C{i}"), version: 1, actor: (*who).into(), at: i as f64, upheld: true }).collect();
        let snap = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert_eq!((snap["rules"]["R006"]["n"].as_i64(), snap["rules"]["R006"]["reviewers"].as_i64()), (Some(2), Some(2)));
    }

    #[test]
    fn only_a_proposed_snapshot_can_be_activated_and_the_refusal_says_why() {
        let store = MemoryQueueStore::new();
        fill(&store, "R006", &["U1", "U2", "U3"], 30, 3, 0);
        learn(&store, &RoutingConfig::default(), 1.0).unwrap().unwrap();
        activate(&store, 1, "CG-4004", None, 2.0).unwrap();
        assert_eq!(activate(&store, 1, "CG-4004", None, 3.0).unwrap_err(), "only a proposed snapshot can be activated", "already approved");
        fill(&store, "R006", &["U1", "U2", "U3"], 6, 6, 500);
        learn(&store, &RoutingConfig::default(), 4.0).unwrap().unwrap();
        reject(&store, 2, "CG-4005", 5.0, "no").unwrap();
        assert_eq!(activate(&store, 2, "CG-4004", None, 6.0).unwrap_err(), "only a proposed snapshot can be activated", "rejected");
    }

    #[test]
    fn activating_different_proposals_at_once_leaves_exactly_one_approved() {
        for round in 0..20 {
            let store = MemoryQueueStore::new();
            for v in 1..=4i64 {
                let mut snap = make_snapshot(&[], &params(), v, 0.0, 1);
                snap["status"] = json!("proposed");
                assert!(store.put_reliability(&snap, v - 1).unwrap());
            }
            activate(&store, 1, "A", None, 1.0).unwrap();
            std::thread::scope(|t| {
                for v in 2..=4i64 {
                    let store = &store;
                    t.spawn(move || {
                        let _ = activate(store, v, "A", Some("B"), 2.0); // an older one may lose to a newer one; that is fine
                    });
                }
            });
            let approved: Vec<i64> = store.reliability_history().unwrap().iter().filter(|d| d["status"] == "approved").map(|d| d["version"].as_i64().unwrap()).collect();
            assert_eq!(approved.len(), 1, "round {round}: {approved:?}");
            assert_eq!(active(&store).unwrap()["version"].as_i64(), approved.first().copied());
        }
    }

    #[test]
    fn the_snapshot_does_not_depend_on_the_order_of_the_decisions() {
        let mut counted: Vec<Counted> = (0..50).map(|i| Counted { rule_id: format!("R00{}", i % 3 + 1), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", i % 5), at: i as f64, upheld: i % 4 != 0 }).collect();
        let a = make_snapshot(&counted, &params(), 1, 0.0, 1);
        counted.reverse();
        let b = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert_eq!(a, b);
    }

    #[test]
    fn verify_accepts_a_real_snapshot_and_rejects_every_kind_of_tampering() {
        let counted: Vec<Counted> = (0..30).map(|i| Counted { rule_id: "R006".into(), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", i % 4), at: i as f64, upheld: i % 3 == 0 }).collect();
        let good = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert!(verify(&good).is_ok());
        let tamper = |f: &dyn Fn(&mut Value)| {
            let mut s = good.clone();
            f(&mut s);
            verify(&s)
        };
        assert!(tamper(&|s| s["rules"]["R006"]["status"] = json!("measured")).is_err(), "status that does not follow from the counts");
        assert!(tamper(&|s| s["rules"]["R006"]["upper"] = json!(0.99)).is_err());
        assert!(tamper(&|s| s["rules"]["R006"]["upheld"] = json!(25)).is_err(), "counts changed but figures not");
        assert!(tamper(&|s| s["rules"]["R006"]["upheld"] = json!(-1)).is_err());
        assert!(tamper(&|s| s["rules"]["R006"]["n"] = json!(7)).is_err());
        assert!(tamper(&|s| s["parameters"]["doubtful_below"] = json!(0.99)).is_err());
        assert!(tamper(&|s| s["parameters"]["doubtful_below"] = json!("x")).is_err());
        assert!(tamper(&|s| s["rules"] = json!([])).is_err());
        assert!(tamper(&|s| s["version"] = json!(0)).is_err());
    }

    #[test]
    fn random_snapshots_are_consistent_and_verify() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(7);
        for _ in 0..300 {
            let n = rng.gen_range(0..80);
            let counted: Vec<Counted> = (0..n).map(|i| Counted { rule_id: format!("R00{}", rng.gen_range(1..4)), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", rng.gen_range(0..6)), at: i as f64, upheld: rng.gen_bool(0.5) }).collect();
            let snap = make_snapshot(&counted, &params(), 1, 0.0, 1);
            verify(&snap).unwrap();
            for (_, r) in snap["rules"].as_object().unwrap() {
                let (lo, rate, hi) = (r["lower"].as_f64().unwrap(), r["rate"].as_f64().unwrap(), r["upper"].as_f64().unwrap());
                assert!(lo <= rate + 1e-12 && rate <= hi + 1e-12 && (0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi));
                assert!(r["n"].as_i64().unwrap() <= n as i64);
            }
        }
    }

    fn results_with(flags: &[(&str, &str, &str)]) -> Value {
        let mut rows: Vec<Value> = crate::triage::rule_ids().into_iter().map(|id| json!({"rule_id": id, "status": "PASS", "severity": "medium"})).collect();
        for (id, status, sev) in flags {
            let i = crate::triage::rule_ids().iter().position(|r| r == id).unwrap();
            rows[i]["status"] = json!(status);
            rows[i]["severity"] = json!(sev);
        }
        Value::Array(rows)
    }

    fn snapshot_with(rule: &str, upheld: i64, overridden: i64) -> Value {
        let p = params();
        json!({"version": 3, "status": "approved", "parameters": p.to_doc(), "rules": {rule: row(upheld, overridden, 5, &p)}})
    }

    #[test]
    fn a_doubtful_rule_sends_the_claim_to_a_senior_and_nothing_else_changes() {
        let results = results_with(&[("R006", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        assert_eq!(receipt["eligibility"], "decide");
        let before = receipt.clone();
        apply(&mut receipt, &results, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!((receipt["eligibility"].as_str(), receipt["reliability_version"].as_i64(), receipt["doubtful_rules"].clone()), (Some("decide_high"), Some(3), json!(["R006"])));
        for k in ["lane", "score", "result_hash", "facts_hash", "statuses", "degraded"] {
            assert_eq!(receipt[k], before[k], "{k} must not change");
        }
        assert!(receipt["uncertainty"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn a_measured_rule_leaves_routing_alone_and_a_green_claim_has_no_uncertainty() {
        let results = results_with(&[("R006", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut receipt, &results, &snapshot_with("R006", 39, 1)).unwrap();
        assert_eq!((receipt["eligibility"].as_str(), receipt["doubtful_rules"].clone()), (Some("decide"), json!([])));
        let green = results_with(&[]);
        let mut r2 = crate::triage::make_receipt(&json!({"claim_id": "C2"}), &green, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut r2, &green, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!((r2["uncertainty"].as_f64(), r2["doubtful_rules"].clone(), r2["eligibility"].as_str()), (Some(0.0), json!([]), Some("decide")));
        assert!(attention(&json!({"results": green, "receipt": r2})).is_empty());
    }

    #[test]
    fn an_unknown_rule_is_maximally_uncertain_and_a_tampered_snapshot_is_refused() {
        let results = results_with(&[("R010", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut receipt, &results, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!(receipt["uncertainty"].as_f64(), Some(1.0));
        let mut bad = snapshot_with("R006", 6, 34);
        bad["rules"]["R006"]["status"] = json!("measured");
        let mut r2 = receipt.clone();
        assert!(apply(&mut r2, &results, &bad).is_err());
        assert_eq!(r2, receipt, "a refused snapshot leaves the receipt untouched");
    }

    #[test]
    fn attention_names_the_reasons_in_words() {
        let high = json!({"results": results_with(&[("R001", "FAIL", "high")]), "receipt": {"degraded": false}});
        assert_eq!(attention(&high), vec!["high_severity"]);
        let mixed = json!({"results": results_with(&[("R001", "UNABLE_TO_ASSESS", "high")]), "receipt": {"degraded": false, "doubtful_rules": ["R001"]}, "escalated": true});
        assert_eq!(attention(&mixed), vec!["high_severity", "low_confidence", "could_not_assess", "disagreement"]);
        assert_eq!(attention(&json!({"results": json!([]), "receipt": {"degraded": true}})), vec!["degraded"]);
    }

    use crate::store::{MemoryQueueStore, QueueStore};

    /// A decided dossier: `n` findings of `rule`, decided by `who`, `upheld` of them confirmed.
    fn fill(store: &MemoryQueueStore, rule: &str, who: &[&str], n: usize, upheld: usize, offset: usize) {
        for i in 0..n {
            let id = format!("{rule}-{}", offset + i);
            let mut d = crate::contract::make_doc(&id, 1, &format!("h-{id}"), "A", "decide", 2, "P1", 1000.0, None);
            d["results"] = results_with(&[(rule, "FAIL", "medium")]);
            d["receipt"]["result_hash"] = json!(triage::result_hash(&d["results"]));
            assert!(store.put_triaged(&d).unwrap());
            let actor = who[i % who.len()];
            store.transition(&id, 1, "triaged", "explanation_skipped", "system:t", 1001.0, None, None, None).unwrap().unwrap();
            store.transition(&id, 1, "explanation_skipped", "ready", "system:t", 1002.0, None, None, None).unwrap().unwrap();
            store.lease(&id, 1, actor, 1003.0, 9000.0).unwrap().unwrap();
            let action = if i < upheld { "confirm_issue" } else { "dismiss_with_reason" };
            store.add_decision(&id, 1, actor, 1004.0 + i as f64, &json!({"rule_id": rule, "action": action, "actor": actor, "reason": "x", "at": 1004.0 + i as f64, "round": 1})).unwrap().unwrap();
            let mut set = serde_json::Map::new();
            set.insert("decided_by".into(), json!(actor));
            store.transition(&id, 1, "leased", "decided", actor, 1005.0 + i as f64, None, Some(set), Some(actor)).unwrap().unwrap();
        }
    }

    #[test]
    fn learn_proposes_once_and_only_when_something_changed() {
        let store = MemoryQueueStore::new();
        let cfg = RoutingConfig::default();
        assert!(learn(&store, &cfg, 10.0).unwrap().is_none(), "nothing decided yet");
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 40, 6, 0);
        let first = learn(&store, &cfg, 11.0).unwrap().expect("a proposal");
        assert_eq!((first["version"].as_i64(), first["status"].as_str(), first["rules"]["R006"]["status"].as_str()), (Some(1), Some("proposed"), Some("doubtful")));
        assert!(first["replay"]["claims"].as_i64().unwrap() > 0 && first["replay"]["verdicts_unchanged"] == true);
        assert!(verify(&first).is_ok(), "the replay report is not part of what verify checks");
        assert!(learn(&store, &cfg, 12.0).unwrap().is_none(), "the same data again writes nothing");
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 4, 4, 100);
        let second = learn(&store, &cfg, 13.0).unwrap().expect("new decisions, new proposal");
        assert_eq!(second["version"], 2);
    }

    #[test]
    fn a_proposal_changes_nothing_until_it_is_activated_and_a_stale_one_cannot_be() {
        let store = MemoryQueueStore::new();
        let cfg = RoutingConfig::default();
        fill(&store, "R006", &["U1", "U2", "U3"], 30, 3, 0);
        learn(&store, &cfg, 1.0).unwrap().unwrap();
        assert!(active(&store).is_none(), "proposed is not active");
        activate(&store, 1, "CG-4004", Some("CG-4005"), 2.0).unwrap();
        assert_eq!(active(&store).unwrap()["version"], 1);
        assert!(activate(&store, 1, "CG-4004", Some("CG-4005"), 3.0).is_err(), "only a proposed snapshot can be activated");
        fill(&store, "R006", &["U1", "U2", "U3"], 6, 6, 200);
        learn(&store, &cfg, 4.0).unwrap().unwrap();
        fill(&store, "R006", &["U1", "U2", "U3"], 6, 6, 300);
        learn(&store, &cfg, 5.0).unwrap().unwrap(); // v3
        activate(&store, 3, "CG-4004", Some("CG-4005"), 6.0).unwrap();
        assert_eq!(active(&store).unwrap()["version"], 3);
        assert!(activate(&store, 2, "CG-4004", Some("CG-4005"), 7.0).is_err(), "an older proposal cannot replace a newer active one");
        let h = store.reliability_history().unwrap();
        assert_eq!(h[0]["status"], "superseded", "the previous active snapshot is marked superseded");
        reject(&store, 2, "CG-4005", 8.0, "not needed").unwrap();
        assert_eq!(store.reliability_history().unwrap()[1]["status"], "rejected");
    }

    #[test]
    fn the_replay_report_counts_senior_load_before_and_after_and_says_so_when_it_rises() {
        let store = MemoryQueueStore::new();
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 40, 6, 0);
        let counted = counted_decisions(&store.by_state("decided", 1000).unwrap());
        let snap = make_snapshot(&counted, &params(), 1, 0.0, 1);
        let rep = replay_report(&store, &snap, 500);
        assert_eq!((rep["claims"].as_i64(), rep["senior_before"].as_i64(), rep["senior_after"].as_i64(), rep["newly_senior"].as_i64()), (Some(40), Some(0), Some(40), Some(40)));
        assert!(rep["note"].as_str().unwrap().contains("senior"), "a rise in senior load is stated in words");
        assert_eq!(rep["verdicts_unchanged"], true);
    }
}
