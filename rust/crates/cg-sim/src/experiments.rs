//! The experiments behind docs/48. Every number comes from simulated reviewers (see `personas`): they show how the mechanism behaves, not
//! how accurate it would be with real people. All functions are seeded, return plain JSON and write nothing.

use crate::generate::{load_corpus, Corpus, Lab};
use crate::personas::truth;
use cg_queue::dispatcher::{Agent, Dispatcher};
use cg_queue::reliability::{self, Params};
use cg_queue::routing_config::{self as rc, RoutingConfig};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use cg_queue::triage;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

const NOTE: &str = "simulated reviewers; shows the mechanism, not real-world accuracy";

fn params() -> Params {
    Params::from_config(&RoutingConfig::default())
}

fn snapshot_from(store: &dyn QueueStore, p: &Params) -> Value {
    reliability::make_snapshot(&reliability::counted_decisions(&store.by_state("decided", 1_000_000).unwrap_or_default()), p, 1, 0.0, 1)
}

fn counted_total(store: &dyn QueueStore) -> usize {
    reliability::counted_decisions(&store.by_state("decided", 1_000_000).unwrap_or_default()).len()
}

/// Rules whose hidden truth says findings are almost always right, yet the snapshot calls them doubtful.
fn wrongly_doubtful(snap: &Value) -> BTreeSet<String> {
    let t = truth();
    reliability::doubtful_rules(snap).into_iter().filter(|r| t.get(r.as_str()).is_some_and(|x| *x >= 0.9)).collect()
}

fn row_view(snap: &Value, rule: &str) -> Value {
    let r = &snap["rules"][rule];
    if r.is_null() {
        return json!({"status": "unseen"});
    }
    json!({"n": r["n"], "rate": r["rate"], "lower": r["lower"], "upper": r["upper"], "status": r["status"]})
}

/// Decisions pile up in one store, 100 at a time; after each batch the snapshot is recomputed.
pub fn convergence(root: &Path, seed: u64) -> Result<Value, String> {
    let corpus = load_corpus(root)?;
    let store = MemoryQueueStore::new();
    let lab = Lab::new(seed, false);
    let (mut claims, mut curve, mut next) = (0usize, vec![], 0usize);
    let checkpoints = [100usize, 200, 500, 1000, 2000, 4000];
    let (mut first, mut wrong) = (Value::Null, BTreeSet::new());
    while counted_total(&store) < 4000 {
        let s = lab.run_from(&store, &corpus, claims, 100, &RoutingConfig::default(), None)?;
        claims += s.claims;
        let total = counted_total(&store);
        let snap = snapshot_from(&store, &params());
        if first.is_null() && snap["rules"]["R006"]["status"] == "doubtful" {
            first = json!(total);
        }
        wrong.extend(wrongly_doubtful(&snap));
        while next < checkpoints.len() && total >= checkpoints[next] {
            curve.push(json!({"decisions": total, "R001": row_view(&snap, "R001"), "R006": row_view(&snap, "R006"), "R013": row_view(&snap, "R013")}));
            next += 1;
        }
    }
    Ok(json!({"curve": curve, "r006_first_doubtful_at_decisions": first, "rules_wrongly_doubtful": wrong, "hidden_truth": truth(), "note": NOTE}))
}

fn senior_share(store: &dyn QueueStore) -> (f64, usize, BTreeMap<String, String>) {
    let docs = store.by_state("decided", 1_000_000).unwrap_or_default();
    let senior = docs.iter().filter(|d| d["receipt"]["eligibility"] == "decide_high").count();
    let hashes = docs.iter().map(|d| (d["claim_id"].as_str().unwrap_or("").to_string(), triage::result_hash(&d["results"]))).collect();
    (if docs.is_empty() { 0.0 } else { senior as f64 / docs.len() as f64 }, senior, hashes)
}

/// The same later claims, reviewed with and without the learned snapshot: who gets them changes, what they say does not.
pub fn routing_effect(root: &Path, seed: u64) -> Result<Value, String> {
    let corpus = load_corpus(root)?;
    let lab = Lab::new(seed, false);
    let base = MemoryQueueStore::new();
    lab.run(&base, &corpus, 2000, &RoutingConfig::default(), None)?;
    let snap = snapshot_from(&base, &params());
    let (off, on) = (MemoryQueueStore::new(), MemoryQueueStore::new());
    lab.run_from(&off, &corpus, 100_000, 1500, &RoutingConfig::default(), None)?;
    lab.run_from(&on, &corpus, 100_000, 1500, &RoutingConfig::default(), Some(&snap))?;
    let ((share_off, q_off, h_off), (share_on, q_on, h_on)) = (senior_share(&off), senior_share(&on));
    let equal = !h_off.is_empty() && h_off == h_on;
    let mut out = json!({"doubtful_rules": reliability::doubtful_rules(&snap), "claims": h_off.len(), "senior_share_off": share_off, "senior_share_on": share_on,
        "senior_queue_off": q_off, "senior_queue_on": q_on, "per_senior_off": q_off as f64 / 4.0, "per_senior_on": q_on as f64 / 4.0,
        "newly_senior": q_on.saturating_sub(q_off), "verdict_hashes_equal": equal, "note": NOTE});
    if q_on > q_off {
        out["load_note"] = json!(format!("The senior queue grows from {q_off} to {q_on} of {} claims when routing uses the snapshot.", h_off.len()));
    }
    Ok(out)
}

fn item_width(snap: &Value, results: &Value) -> f64 {
    triage::flagged(results).iter().filter_map(|r| r["rule_id"].as_str()).map(|id| snap["rules"][id]["width"].as_f64().unwrap_or(1.0)).fold(0.0, f64::max)
}

fn mean_width(snap: &Value) -> f64 {
    let t = truth();
    t.keys().map(|r| snap["rules"][*r]["width"].as_f64().unwrap_or(1.0)).sum::<f64>() / t.len() as f64
}

fn known(snap: &Value, rules: &BTreeSet<String>) -> bool {
    rules.iter().all(|r| matches!(snap["rules"][r.as_str()]["status"].as_str(), Some("measured") | Some("doubtful")))
}

fn run_strategy(corpus: &Corpus, seed: u64, uncertainty_first: bool) -> Result<Value, String> {
    let store = MemoryQueueStore::new();
    let flagged_rules: BTreeSet<String> = corpus.items.iter().flat_map(|(_, r)| triage::flagged(r).into_iter().filter_map(|f| f["rule_id"].as_str().map(str::to_string))).collect();
    let (mut used, mut batch) = (BTreeSet::new(), 0u64);
    let (mut width_600, mut known_at) = (Value::Null, Value::Null);
    let mut snap = snapshot_from(&store, &params());
    while counted_total(&store) < 4000 {
        let mut order: Vec<usize> = (0..corpus.items.len()).filter(|i| !used.contains(i)).collect();
        if order.len() < 20 {
            used.clear();
            order = (0..corpus.items.len()).collect();
        }
        if uncertainty_first {
            order.sort_by(|a, b| item_width(&snap, &corpus.items[*b].1).partial_cmp(&item_width(&snap, &corpus.items[*a].1)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b)));
        }
        let take: Vec<usize> = order.into_iter().take(30).collect();
        used.extend(take.iter().copied());
        let sub = Corpus { items: take.iter().map(|i| corpus.items[*i].clone()).collect() };
        let mut lab = Lab::new(seed + batch, false);
        lab.id_prefix = format!("AL{batch}");
        lab.run(&store, &sub, 50, &RoutingConfig::default(), None)?;
        batch += 1;
        snap = snapshot_from(&store, &params());
        let total = counted_total(&store);
        if width_600.is_null() && total >= 600 {
            width_600 = json!(mean_width(&snap));
        }
        if known_at.is_null() && known(&snap, &flagged_rules) {
            known_at = json!(total);
        }
    }
    Ok(json!({"mean_width_at_600_decisions": width_600, "decisions_until_every_flagged_rule_is_known": known_at}))
}

/// Reviewing the least-known claims first versus in arrival order.
pub fn active_learning(root: &Path, seed: u64) -> Result<Value, String> {
    let corpus = load_corpus(root)?;
    Ok(json!({"arrival": run_strategy(&corpus, seed, false)?, "uncertainty_first": run_strategy(&corpus, seed, true)?,
        "note": "a demonstration on simulated reviewers, not a finding about real ones"}))
}

/// One adversarial senior who dismisses everything decides a share of the claims. The same decisions are read with and without the protections
/// (a reviewer's cap on one rule, and a minimum number of different reviewers).
pub fn poisoning(root: &Path, seed: u64) -> Result<Value, String> {
    let corpus = load_corpus(root)?;
    let weak = Params { reviewer_cap: 1.0, min_reviewers: 1, ..params() };
    let mut sweep = vec![];
    for share in [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.8, 1.0] {
        let store = MemoryQueueStore::new();
        let mut lab = Lab::new(seed, true);
        lab.adversary_share = Some(share);
        lab.run(&store, &corpus, 3000, &RoutingConfig::default(), None)?;
        let (a, b) = (wrongly_doubtful(&snapshot_from(&store, &weak)), wrongly_doubtful(&snapshot_from(&store, &params())));
        sweep.push(json!({"adversary_share": share, "wrongly_doubtful_without_protection": a.len(), "wrongly_doubtful_with_protection": b.len()}));
    }
    let captured = sweep.last().cloned().unwrap_or(Value::Null);
    let list = |k: &str| -> Vec<Value> { (0..captured[k].as_u64().unwrap_or(0)).map(|_| json!("rule")).collect() };
    Ok(json!({"sweep": sweep,
        "without_protection": {"wrongly_doubtful": list("wrongly_doubtful_without_protection")},
        "with_cap_and_minimum": {"wrongly_doubtful": list("wrongly_doubtful_with_protection")},
        "reading": "The two cases are the last sweep row (the adversary decides every claim): the minimum of three different reviewers keeps every rule unmeasured. \
A reviewer's cap on one rule limits a minority adversary but cannot out-vote an adversary who decides more than the cap and more than the honest reviewers together; see the sweep.",
        "note": NOTE}))
}

fn agent(b: &str) -> Agent {
    Agent { badge_id: b.into(), permissions: ["claims.decide".to_string()].into_iter().collect(), active: true }
}

/// Properties that must hold for every seed; each value is true or false.
pub fn invariants(root: &Path) -> Result<Value, String> {
    let corpus = load_corpus(root)?;
    let cfg = RoutingConfig::default();
    // 1. with the feature off nothing about a stored receipt differs from a plain receipt, and no reliability field exists
    let store = MemoryQueueStore::new();
    Lab::new(3, false).run(&store, &corpus, 200, &cfg, None)?;
    let docs = store.by_state("decided", 100_000).map_err(|e| e.to_string())?;
    let off_equal = !docs.is_empty()
        && docs.iter().all(|d| {
            let fresh = triage::make_receipt(&d["claim"], &d["results"], &cfg, "sim-pack", "sim-engine", 0.0);
            let r = &d["receipt"];
            r.get("reliability_version").is_none() && r.get("doubtful_rules").is_none() && ["lane", "score", "eligibility", "result_hash"].iter().all(|k| r[*k] == fresh[*k])
        });
    // 2. a snapshot with an altered status is refused
    let snap = snapshot_from(&store, &params());
    let some_rule = snap["rules"].as_object().and_then(|o| o.keys().next().cloned()).ok_or("no rules in the snapshot")?;
    let mut bad = snap.clone();
    bad["rules"][&some_rule]["status"] = json!(if snap["rules"][&some_rule]["status"] == "doubtful" { "measured" } else { "doubtful" });
    let tampered = reliability::verify(&snap).is_ok() && reliability::verify(&bad).is_err();
    // 3. applying a snapshot changes no verdict field
    let mut doubtful = snap.clone();
    doubtful["rules"]["R006"] = reliability::row(2, 40, 5, &params());
    let unchanged = corpus.items.iter().take(200).all(|(c, results)| {
        let before = triage::make_receipt(c, results, &cfg, "p", "e", 1.0);
        let mut after = before.clone();
        reliability::apply(&mut after, results, &doubtful).is_ok() && ["lane", "score", "result_hash", "facts_hash", "statuses", "degraded"].iter().all(|k| before[*k] == after[*k])
    });
    // 4. a deal made with the uncertainty term replays from its stored seed
    let deal_store = Arc::new(MemoryQueueStore::new());
    let mut dcfg = RoutingConfig { version: 1, uncertainty_weight: 3.0, slice_size: 10, low_water: 5, ..RoutingConfig::default() };
    dcfg.on_shift = vec!["A".into(), "B".into()];
    deal_store.put_config(&rc::to_doc(&dcfg)?, 0).map_err(|e| e.to_string())?;
    for i in 0..30 {
        let id = format!("D{i}");
        let mut d = cg_queue::contract::make_doc(&id, 1, &format!("h{id}"), "A", "decide", 2, "P1", 1000.0, None);
        d["receipt"]["uncertainty"] = json!((i % 7) as f64 / 7.0);
        if !deal_store.put_triaged(&d).map_err(|e| e.to_string())? {
            return Err("duplicate".into());
        }
        deal_store.transition(&id, 1, "triaged", "ready", "t", 1_000_000.0, None, None, None).map_err(|e| e.to_string())?.ok_or("not ready")?;
    }
    let dispatcher = Dispatcher { store: deal_store.clone(), agents: Arc::new(|| vec![agent("A"), agent("B")]), clock: Arc::new(|| 1_000_000.0), rng_seed: Some(9), log: None };
    let deal = dispatcher.deal(None, true);
    let replays = !deal.assigned.is_empty() && dispatcher.verify(&deal.deal_id).unwrap_or(false);
    Ok(json!({"feature_off_receipts_equal_baseline": off_equal, "tampered_snapshot_detected": tampered, "verdicts_unchanged_by_apply": unchanged, "replay_of_uncertainty_deal_matches": replays}))
}

pub fn all(root: &Path, seed: u64) -> Result<Value, String> {
    Ok(json!({"simulated": true, "seed": seed,
        "decisions_note": "Reviewers are simulated with a hidden true upheld rate per rule; claims and findings are the real development set, recycled under new ids. The curves show how the mechanism behaves, not accuracy with real people.",
        "convergence": convergence(root, seed)?, "routing_effect": routing_effect(root, seed)?, "active_learning": active_learning(root, seed)?,
        "poisoning": poisoning(root, seed)?, "invariants": invariants(root)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn r006_becomes_doubtful_after_a_stated_number_of_decisions_and_never_before_the_minimum() {
        let c = convergence(&root(), 1).unwrap();
        let first = c["r006_first_doubtful_at_decisions"].as_i64().expect("R006 becomes doubtful");
        assert!(first >= 20, "never before the minimum number of decisions: {first}");
        assert!(c["rules_wrongly_doubtful"].as_array().unwrap().is_empty(), "no rule with a true rate of 0.97 is ever marked doubtful: {c}");
    }

    #[test]
    fn routing_on_changes_who_reviews_but_never_a_verdict_and_reports_senior_load() {
        let r = routing_effect(&root(), 1).unwrap();
        assert_eq!(r["verdict_hashes_equal"], true);
        assert!(r["senior_share_on"].as_f64().unwrap() >= r["senior_share_off"].as_f64().unwrap());
        assert!(r["senior_share_off"].is_number() && r["senior_queue_on"].is_number());
    }

    #[test]
    fn a_captured_queue_is_stopped_by_the_minimum_reviewers_and_the_protections_never_make_it_worse() {
        let p = poisoning(&root(), 1).unwrap();
        assert_eq!(p["with_cap_and_minimum"]["wrongly_doubtful"].as_array().unwrap().len(), 0, "{p}");
        assert!(!p["without_protection"]["wrongly_doubtful"].as_array().unwrap().is_empty(), "the attack must succeed without the protections, or the demo proves nothing: {p}");
        for row in p["sweep"].as_array().unwrap() {
            assert!(row["wrongly_doubtful_with_protection"].as_u64() <= row["wrongly_doubtful_without_protection"].as_u64(), "{row}");
        }
    }

    #[test]
    fn learning_by_uncertainty_is_reported_for_both_strategies() {
        let a = active_learning(&root(), 1).unwrap();
        assert!(a["arrival"]["mean_width_at_600_decisions"].is_number() && a["uncertainty_first"]["mean_width_at_600_decisions"].is_number(), "{a}");
    }

    #[test]
    fn every_invariant_holds() {
        let i = invariants(&root()).unwrap();
        for (k, v) in i.as_object().unwrap() {
            assert_eq!(v, &json!(true), "{k}");
        }
    }
}
