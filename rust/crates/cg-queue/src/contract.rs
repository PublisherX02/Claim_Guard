//! One contract for every `QueueStore`. The in-memory twin and the MongoDB store both pass it, so the twin is checked against the real
//! database's behaviour. Races use real threads: the guarantees (one lease, one transition, one configuration version) must hold there,
//! not just in sequential code. (`tests/queue_store_contract.py` in the Python build.)

use crate::store::QueueStore;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const NOW: f64 = 1000.0;

pub fn make_doc(claim_id: &str, version: i64, input_hash: &str, lane: &str, eligibility: &str, score: i64, patient: &str, created: f64, pack: Option<&str>) -> Value {
    let mut receipt = json!({"claim_id": claim_id, "input_hash": input_hash, "lane": lane, "eligibility": eligibility, "score": score, "config_version": 1, "created_at": created,
                             "statuses": {"R001": "FAIL"}});
    if let Some(p) = pack {
        receipt["rule_pack_hash"] = json!(p);
    }
    json!({"claim_id": claim_id, "version": version, "input_hash": input_hash,
           "claim": {"claim_id": claim_id, "patient_id": patient, "provider_id": "V1", "submission_date": "2026-03-10", "lines": [{"line_id": "L1", "service_code": "SVC-LAB", "quantity": 1}],
                     "authorizations": [], "notes": "n", "diagnosis_code": "DX-EDU-01", "attachments": [], "member_id": "M1"},
           "results": [{"rule_id": "R001", "status": "FAIL", "severity": "high"}], "receipt": receipt})
}

pub fn doc(id: &str) -> Value {
    make_doc(id, 1, &format!("h-{id}"), "A", "decide", 2, "P1", NOW, None)
}

fn ready(s: &dyn QueueStore, id: &str) {
    assert!(s.put_triaged(&doc(id)).unwrap());
    assert!(s.transition(id, 1, "triaged", "explanation_skipped", "system:t", NOW + 1.0, None, None, None).unwrap().is_some());
    assert!(s.transition(id, 1, "explanation_skipped", "ready", "system:t", NOW + 2.0, None, None, None).unwrap().is_some());
}

fn fields(pairs: &[(&str, Value)]) -> Option<Map<String, Value>> {
    Some(pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect())
}

/// Panics with a message on the first violation. `fresh` must give an empty store each time.
pub fn run(fresh: &dyn Fn() -> Box<dyn QueueStore>) {
    // ---- intake and reads
    let s = fresh();
    assert!(s.ping());
    assert!(s.put_triaged(&doc("C1")).unwrap());
    let d = s.get("C1", None).unwrap().unwrap();
    assert_eq!((d["state"].as_str(), d["state_at"].as_f64(), d["enqueue_pending"].as_bool(), d["lease"].is_null(), d["decided_by"].is_null()), (Some("triaged"), Some(NOW), Some(true), true, true));
    assert_eq!(d["events"].as_array().unwrap().len(), 1);
    assert_eq!((d["events"][0]["from"].as_str(), d["events"][0]["to"].as_str()), (Some("received"), Some("triaged")));
    assert!(!s.put_triaged(&doc("C1")).unwrap(), "the same claim and input hash is stored once");
    assert!(s.put_triaged(&make_doc("C1", 2, "h2", "A", "decide", 2, "P1", NOW, None)).unwrap());
    assert_eq!(s.get("C1", None).unwrap().unwrap()["version"], 2, "get returns the latest");
    assert_eq!(s.get("C1", Some(1)).unwrap().unwrap()["input_hash"], "h-C1");
    assert!(!s.put_triaged(&make_doc("C1", 2, "other", "A", "decide", 2, "P1", NOW, None)).unwrap(), "the same version with a different input is refused");
    assert!(s.get("NOPE", None).unwrap().is_none());
    let s2 = fresh();
    assert!(s2.put_triaged(&make_doc("C1", 1, "h", "A", "decide", 2, "P1", NOW, Some("pack1"))).unwrap());
    assert!(!s2.put_triaged(&make_doc("C1", 2, "h", "A", "decide", 2, "P1", NOW, Some("pack1"))).unwrap(), "same body, same pack: a duplicate");
    assert!(s2.put_triaged(&make_doc("C1", 2, "h", "A", "decide", 2, "P1", NOW, Some("pack2"))).unwrap(), "same body, new pack: the next version");
    // parallel intake of one claim stores it once
    let s3 = fresh();
    let stored = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for _ in 0..20 {
            t.spawn(|| {
                if s3.put_triaged(&doc("RACE")).unwrap() {
                    stored.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(stored.load(Ordering::SeqCst), 1);
    // malformed documents are refused
    for bad in [json!({}), json!({"claim_id": "X"}), {let mut d = doc("X"); d["version"] = json!(0); d}, {let mut d = doc("X"); d["version"] = json!("1"); d}, {let mut d = doc("X"); d["claim_id"] = json!(""); d},
                {let mut d = doc("X"); d["results"] = json!({}); d}, {let mut d = doc("X"); d["extra"] = json!(1); d}, {let mut d = doc("X"); d["receipt"]["created_at"] = json!("now"); d}] {
        assert!(s3.put_triaged(&bad).is_err(), "{bad}");
    }

    // ---- transitions
    let s = fresh();
    assert!(s.put_triaged(&doc("C1")).unwrap());
    let moved = s.transition("C1", 1, "triaged", "explained", "system:t", NOW + 1.0, Some(json!({"why": "x"})), None, None).unwrap().unwrap();
    assert_eq!((moved["state"].as_str(), moved["events"].as_array().unwrap().len()), (Some("explained"), 2));
    assert!(s.transition("C1", 1, "triaged", "ready", "system:t", NOW + 2.0, None, None, None).unwrap().is_none(), "wrong state changes nothing");
    assert_eq!(s.get("C1", None).unwrap().unwrap()["events"].as_array().unwrap().len(), 2);
    assert!(s.transition("C1", 1, "explained", "decided", "a", NOW, None, None, None).is_err(), "an illegal pair is refused");
    assert!(s.transition("NOPE", 1, "triaged", "ready", "a", NOW, None, None, None).unwrap().is_none());
    assert!(s.transition("C1", 1, "explained", "ready", "a", NOW, None, fields(&[("nope", json!(1))]), None).is_err(), "unknown set fields are refused");
    let set = s.transition("C1", 1, "explained", "ready", "a", NOW + 3.0, None, fields(&[("escalated", json!(true)), ("decided_by", json!("CG-1"))]), None).unwrap().unwrap();
    assert_eq!((set["escalated"].as_bool(), set["decided_by"].as_str()), (Some(true), Some("CG-1")));

    // ---- outbox, by_state, counts
    let s = fresh();
    assert!(s.put_triaged(&make_doc("B", 1, "hb", "A", "decide", 2, "P1", NOW + 5.0, None)).unwrap());
    assert!(s.put_triaged(&make_doc("A", 1, "ha", "B", "decide_high", 6, "P1", NOW + 1.0, None)).unwrap());
    assert_eq!(s.pending_outbox(10).unwrap().iter().map(|d| d["claim_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["A", "B"], "oldest first");
    assert!(s.clear_outbox("A", 1).unwrap() && !s.clear_outbox("A", 1).unwrap());
    assert_eq!(s.pending_outbox(10).unwrap().len(), 1);
    ready(&*s, "C");
    assert_eq!(s.by_state("ready", 10).unwrap().len(), 1);
    let counts = s.counts().unwrap();
    assert_eq!((counts["triaged|A|decide"], counts["triaged|B|decide_high"], counts["ready|A|decide"]), (1, 1, 1));

    // ---- leases
    let s = fresh();
    ready(&*s, "C1");
    let leased = s.lease("C1", 1, "CG-2002", NOW + 10.0, NOW + 100.0).unwrap().unwrap();
    assert_eq!((leased["state"].as_str(), leased["lease"]["badge_id"].as_str(), leased["lease"]["expires_at"].as_f64()), (Some("leased"), Some("CG-2002"), Some(NOW + 100.0)));
    assert!(s.lease("C1", 1, "CG-2003", NOW + 11.0, NOW + 100.0).unwrap().is_none(), "not ready any more");
    assert!(s.lease("NOPE", 1, "CG-2003", NOW, NOW + 100.0).unwrap().is_none());
    let s = fresh();
    ready(&*s, "RACE");
    let wins = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for i in 0..50 {
            let s = &s;
            let wins = &wins;
            t.spawn(move || {
                if s.lease("RACE", 1, &format!("CG-{i}"), NOW + 10.0, NOW + 100.0).unwrap().is_some() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(wins.load(Ordering::SeqCst), 1, "fifty agents racing for one claim give exactly one lease");
    assert_eq!(s.get("RACE", None).unwrap().unwrap()["events"].as_array().unwrap().len(), 4, "received, skipped, ready and exactly one lease event");
    let s = fresh();
    for (id, at) in [("L2", 20.0), ("L1", 10.0)] {
        ready(&*s, id);
        s.lease(id, 1, "CG-2002", NOW + at, NOW + at + 1000.0).unwrap().unwrap();
    }
    assert_eq!(s.inbox("CG-2002").unwrap().iter().map(|d| d["claim_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["L1", "L2"], "ordered by lease time");
    ready(&*s, "OTHER");
    s.lease("OTHER", 1, "CG-3003", NOW + 5.0, NOW + 500.0).unwrap().unwrap();
    assert_eq!(s.heartbeat("CG-2002", NOW + 100.0, NOW + 9999.0).unwrap(), 2);
    assert_eq!(s.inbox("CG-3003").unwrap()[0]["lease"]["expires_at"].as_f64(), Some(NOW + 500.0), "only that badge's leases");
    assert_eq!(s.expired(NOW + 499.9).unwrap().len(), 0);
    assert_eq!(s.expired(NOW + 500.0).unwrap().len(), 1, "the boundary is inclusive");
    let (count, points) = s.inbox_load("CG-2002").unwrap();
    assert_eq!((count, points), (2, 4));
    assert_eq!(s.inbox_load("nobody").unwrap(), (0, 0));
    let rows = s.leased_summary().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(s.latest_versions(&["L1".to_string(), "NOPE".to_string()]).unwrap().get("L1"), Some(&1));

    // ---- holder guard and decisions
    let s = fresh();
    ready(&*s, "H");
    s.lease("H", 1, "CG-2002", NOW + 10.0, NOW + 100.0).unwrap().unwrap();
    assert!(s.transition("H", 1, "leased", "ready", "x", NOW + 20.0, None, fields(&[("lease", Value::Null)]), Some("CG-9999")).unwrap().is_none(), "not the holder");
    assert!(s.add_decision("H", 1, "CG-9999", NOW + 20.0, &json!({"rule_id": "R001"})).unwrap().is_none(), "not the holder");
    assert!(s.add_decision("H", 1, "CG-2002", NOW + 100.0, &json!({"rule_id": "R001"})).unwrap().is_none(), "the lease ran out at the boundary");
    assert!(s.add_decision("H", 1, "CG-2002", NOW + 99.0, &json!({})).is_err(), "malformed decision");
    let with = s.add_decision("H", 1, "CG-2002", NOW + 99.0, &json!({"rule_id": "R001", "action": "confirm_issue"})).unwrap().unwrap();
    assert_eq!(with["decisions"].as_array().unwrap().len(), 1);
    assert!(s.transition("H", 1, "leased", "ready", "x", NOW + 20.0, None, fields(&[("lease", Value::Null)]), Some("CG-2002")).unwrap().is_some());
    assert!(s.add_decision("H", 1, "CG-2002", NOW + 21.0, &json!({"rule_id": "R001"})).unwrap().is_none(), "not leased any more");
    // a decision and a hand-back race: one winner, a consistent document
    let s = fresh();
    ready(&*s, "RD");
    s.lease("RD", 1, "CG-2002", NOW + 10.0, NOW + 100.0).unwrap().unwrap();
    std::thread::scope(|t| {
        for i in 0..40 {
            let s = &s;
            t.spawn(move || {
                if i % 2 == 0 {
                    let _ = s.add_decision("RD", 1, "CG-2002", NOW + 20.0, &json!({"rule_id": "R001", "n": i}));
                } else {
                    let _ = s.transition("RD", 1, "leased", "ready", "x", NOW + 20.0, None, fields(&[("lease", Value::Null)]), Some("CG-2002"));
                }
            });
        }
    });
    let d = s.get("RD", None).unwrap().unwrap();
    assert_eq!(d["state"], "ready");
    assert!(d["lease"].is_null());
    assert_eq!(d["events"].as_array().unwrap().iter().filter(|e| e["to"] == "ready").count(), 2, "exactly one hand-back");

    // ---- shadow (once) and the history query
    let s = fresh();
    assert!(s.put_triaged(&doc("S")).unwrap());
    assert!(s.set_shadow("S", 1, &json!({"would": "clear"})).unwrap() && !s.set_shadow("S", 1, &json!({"would": "other"})).unwrap());
    assert!(!s.set_shadow("NOPE", 1, &json!({"would": "x"})).unwrap());
    assert_eq!(s.get("S", None).unwrap().unwrap()["shadow"]["would"], "clear");
    assert!(s.put_triaged(&make_doc("S", 2, "h2", "A", "decide", 2, "P1", NOW, None)).unwrap());
    assert!(s.put_triaged(&make_doc("T", 1, "ht", "A", "decide", 2, "P2", NOW, None)).unwrap());
    let hist = s.claims_for_patient("P1").unwrap();
    assert_eq!(hist.len(), 1, "only the newest version of each claim");
    assert_eq!(hist[0]["claim_id"], "S");
    assert!(hist[0].get("member_id").is_none(), "only the fields the cross-claim rules need");

    // ---- configuration
    let s = fresh();
    assert!(s.latest_config().unwrap().is_none());
    assert!(s.put_config(&json!({"version": 1, "slice_size": 5}), 0).unwrap());
    assert!(!s.put_config(&json!({"version": 1, "slice_size": 9}), 0).unwrap(), "a stale write loses");
    assert!(s.put_config(&json!({"version": 2}), 1).unwrap());
    assert_eq!((s.latest_config().unwrap().unwrap()["version"].as_i64(), s.config_history().unwrap().len()), (Some(2), 2));
    assert!(s.put_config(&json!({"version": 5}), 2).is_err(), "must be exactly the next version");
    assert!(s.put_config(&json!({"version": "3"}), 2).is_err());
    let s = fresh();
    let winners = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for _ in 0..20 {
            t.spawn(|| {
                if s.put_config(&json!({"version": 1}), 0).unwrap() {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!((winners.load(Ordering::SeqCst), s.config_history().unwrap().len()), (1, 1), "twenty writers at one expected version give one winner");

    // ---- deals, dead letters, cache, counters, jobs
    let s = fresh();
    s.append_deal(&json!({"deal_id": "D1", "seed": 1, "at": NOW})).unwrap();
    s.append_deal(&json!({"deal_id": "D2", "seed": 2, "at": NOW + 1.0})).unwrap();
    assert_eq!(s.get_deal("D1").unwrap().unwrap()["seed"], 1);
    assert!(s.get_deal("NOPE").unwrap().is_none());
    assert_eq!(s.deals(10).unwrap().iter().map(|d| d["deal_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["D2", "D1"], "newest first");
    assert!(s.append_deal(&json!({"seed": 3})).is_err());
    s.add_dead_letter(&json!({"dead_id": "X1", "claim_id": "C1", "reason": "boom", "at": NOW})).unwrap();
    assert_eq!(s.dead_letters(10).unwrap().len(), 1);
    assert_eq!(s.pop_dead_letter("X1").unwrap().unwrap()["reason"], "boom");
    assert!(s.pop_dead_letter("X1").unwrap().is_none());
    assert!(s.cache_get("k").unwrap().is_none());
    s.cache_put("k", "text").unwrap();
    assert_eq!(s.cache_get("k").unwrap().as_deref(), Some("text"));
    let allowed = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for _ in 0..30 {
            t.spawn(|| {
                if s.bump("ai", "minute-1", 7).unwrap() {
                    allowed.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(allowed.load(Ordering::SeqCst), 7, "bump allows exactly the limit across threads");
    assert!(s.bump("ai", "minute-2", 1).unwrap(), "windows are independent");
    assert!(s.bump("ai", "x", -1).is_err());
    s.record_job("relay", NOW, true, "published 2").unwrap();
    s.record_job("relay", NOW + 10.0, false, "broker down").unwrap();
    s.record_job("deal", NOW + 5.0, true, "").unwrap();
    let jobs = s.jobs().unwrap();
    let relay = jobs.iter().find(|j| j["name"] == "relay").unwrap();
    assert_eq!((relay["runs"].as_i64(), relay["failures"].as_i64(), relay["ok"].as_bool(), relay["at"].as_f64(), relay["last_ok_at"].as_f64()), (Some(2), Some(1), Some(false), Some(NOW + 10.0), Some(NOW)));
    assert_eq!(jobs.len(), 2);
    assert!(s.record_job("", NOW, true, "").is_err() && s.record_job("x", f64::NAN, true, "").is_err() && s.record_job("x", NOW, true, &"d".repeat(201)).is_err());

    // ---- identifiers are text, never a query
    let s = fresh();
    assert!(s.put_triaged(&doc("C1")).unwrap());
    assert!(s.get("{\"$ne\": null}", None).unwrap().is_none());
    assert!(s.inbox("{\"$ne\": null}").unwrap().is_empty());
    assert!(s.get("C1", Some(0)).is_err() && s.get("C1", Some(-1)).is_err());
}
