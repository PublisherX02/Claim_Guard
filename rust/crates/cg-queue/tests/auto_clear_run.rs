//! How many claims does the real pipeline clear on its own? Runs the labelled claims (development, validation, stress: 600) through the real rule
//! engine, triage and the pipeline with `auto_clear_green` ON, in random batches of 500, and compares every claim the system cleared with the
//! organizers' answer key. Writes `outputs/defense/auto_clear_run.json`. Run it with:
//!
//!     cargo test -p cg-queue --test auto_clear_run -- --nocapture

use cg_engine::Engine;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::explain::{default_guard, ExplainStep, NoModel};
use cg_queue::flow::{engine_fn, process, routing_config, Runtime};
use cg_queue::routing_config::{to_doc, RoutingConfig};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use cg_queue::triage;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

const BATCHES: u64 = 20;
const BATCH: usize = 500;
const AUDIT: i64 = 5;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn lines(path: PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn stat(v: &[f64]) -> Value {
    json!({"mean": v.iter().sum::<f64>() / v.len().max(1) as f64, "min": v.iter().cloned().fold(f64::MAX, f64::min), "max": v.iter().cloned().fold(f64::MIN, f64::max)})
}

#[test]
fn the_real_pipeline_clears_green_claims_on_its_own_and_every_one_matches_the_answer_key() {
    let engine = Arc::new(Engine::load(&root()).expect("the rule engine loads"));
    let run_engine = engine_fn(engine);
    // the labelled claims and the answer key (the flagged rules each claim should have)
    let mut pool: Vec<Value> = vec![];
    let mut key: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
        for r in lines(root().join("data").join(dir).join("expected_results.jsonl")) {
            let flagged = ["FAIL", "UNABLE_TO_ASSESS"].contains(&r["status"].as_str().unwrap_or(""));
            let set = key.entry(r["claim_id"].as_str().unwrap().to_string()).or_default();
            if flagged {
                set.insert(r["rule_id"].as_str().unwrap().to_string());
            }
        }
    }
    assert_eq!(pool.len(), 600);

    let clock: cg_access::Clock = Arc::new(|| 1_000_000.0);
    let (mut cleared_n, mut audited_n, mut flagged_n, mut other_n, mut wrong_n, mut missed_green_n) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for b in 0..BATCHES {
        let store = Arc::new(MemoryQueueStore::new());
        let cfg = RoutingConfig { auto_clear_green: true, audit_percent: AUTO_AUDIT, ..RoutingConfig::default() };
        assert!(store.put_config(&to_doc(&cfg).unwrap(), 0).unwrap());
        assert!(routing_config(&*store).auto_clear_green);
        let sc = store.clone();
        let explain = Arc::new(ExplainStep::new(store.clone(), Arc::new(NoModel), default_guard(), Arc::new(CircuitBreaker::new(clock.clone())), clock.clone(), Arc::new(move || routing_config(&*sc))));
        let rt = Runtime { store: store.clone(), explain, clock: clock.clone(), max_retries: 3 };

        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(20261015 + b);
        let mut idx: Vec<usize> = (0..pool.len()).collect();
        idx.shuffle(&mut rng);
        let (mut cleared, mut audited, mut flagged, mut other, mut wrong, mut missed) = (0, 0, 0, 0, 0, 0);
        for i in idx.into_iter().take(BATCH) {
            let claim = &pool[i];
            let id = claim["claim_id"].as_str().unwrap().to_string();
            let results = run_engine(claim).expect("the engine runs");
            let ihash = triage::input_hash(claim);
            let receipt = triage::make_receipt(claim, &results, &routing_config(&*store), "pack", "eng", 1_000_000.0);
            let doc = json!({"claim_id": id, "version": 1, "input_hash": ihash, "claim": claim, "results": results, "receipt": receipt, "advisory": []});
            assert!(store.put_triaged(&doc).unwrap());
            process(&rt, &id, 1, &ihash);
            let d = store.get(&id, None).unwrap().unwrap();
            let key_flagged = &key[&id];
            let engine_flagged: BTreeSet<String> = triage::flagged(&d["results"]).iter().map(|r| r["rule_id"].as_str().unwrap().to_string()).collect();
            assert_eq!(&engine_flagged, key_flagged, "the engine and the answer key agree on {id}");
            if d["auto_cleared"] == true {
                cleared += 1;
                if !key_flagged.is_empty() {
                    wrong += 1; // the system cleared a claim the answer key says has a finding
                }
            } else if d["events"].as_array().unwrap().iter().any(|e| e["detail"]["why"] == "green_audit") {
                audited += 1;
            } else if !engine_flagged.is_empty() {
                flagged += 1;
            } else {
                other += 1; // green by the key but not cleared and not audited: should not happen
            }
            if key_flagged.is_empty() && d["auto_cleared"] != true && !d["events"].as_array().unwrap().iter().any(|e| e["detail"]["why"] == "green_audit") {
                missed += 1;
            }
        }
        cleared_n.push(cleared as f64);
        audited_n.push(audited as f64);
        flagged_n.push(flagged as f64);
        other_n.push(other as f64);
        wrong_n.push(wrong as f64);
        missed_green_n.push(missed as f64);
    }
    let total_green: Vec<f64> = cleared_n.iter().zip(&audited_n).map(|(a, b)| a + b).collect();
    let share: Vec<f64> = cleared_n.iter().map(|c| 100.0 * c / BATCH as f64).collect();
    let for_a_person: Vec<f64> = cleared_n.iter().map(|c| BATCH as f64 - c).collect();
    let out = json!({"what": "the real engine, triage and pipeline run on the 600 labelled claims with auto_clear_green on, in random batches",
                     "batches": BATCHES, "batch_size": BATCH, "audit_percent": AUTO_AUDIT, "pool": 600,
                     "cleared_by_the_system": stat(&cleared_n), "cleared_percent_of_batch": stat(&share),
                     "green_claims_sent_to_a_person_as_the_audit_sample": stat(&audited_n), "green_claims_in_the_batch": stat(&total_green),
                     "claims_with_findings_left_for_a_person": stat(&flagged_n), "claims_for_a_person_in_all": stat(&for_a_person),
                     "cleared_but_the_answer_key_has_a_finding": stat(&wrong_n), "green_by_the_key_but_neither_cleared_nor_audited": stat(&other_n),
                     "note": "the claims and the answer key are real labelled claims (the organizers' synthetic mix); the audit draw is random; the engine matches the key on every claim"});
    let path = root().join("outputs/defense/auto_clear_run.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    assert_eq!(wrong_n.iter().sum::<f64>(), 0.0, "the system never cleared a claim the answer key says has a finding");
    assert_eq!(other_n.iter().sum::<f64>(), 0.0);
    assert_eq!(missed_green_n.iter().sum::<f64>(), 0.0);
}

const AUTO_AUDIT: i64 = AUDIT;
