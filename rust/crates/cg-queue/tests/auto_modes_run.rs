//! What the real pipeline does on its own with each automatic step switched on, on the same random batches of the 600 labelled claims:
//! auto-clear only; auto-clear plus auto-request for medium-severity missing data; auto-clear plus auto-request for all severities.
//! Every claim the system cleared and every request it recorded is compared with the organizers' answer key. Writes
//! `outputs/defense/auto_modes_run.json`.
//!
//!     cargo test -p cg-queue --test auto_modes_run -- --nocapture

use cg_engine::Engine;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::explain::{default_guard, ExplainStep, NoModel};
use cg_queue::flow::{engine_fn, process, routing_config, AUTO_CLEAR, AUTO_REQUEST};
use cg_queue::routing_config::{to_doc, RoutingConfig};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use cg_queue::triage;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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

#[derive(Default)]
struct Series {
    cleared: Vec<f64>,
    requested_only: Vec<f64>,
    audited: Vec<f64>,
    for_a_person: Vec<f64>,
    findings_today: Vec<f64>,
    findings_for_a_person: Vec<f64>,
    system_requests: Vec<f64>,
    wrong_clears: Vec<f64>,
    wrong_requests: Vec<f64>,
}

#[test]
fn each_automatic_step_on_the_real_pipeline_and_every_system_decision_matches_the_answer_key() {
    let run_engine = engine_fn(Arc::new(Engine::load(&root()).expect("the rule engine loads")));
    let mut pool: Vec<Value> = vec![];
    let mut key: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
        for r in lines(root().join("data").join(dir).join("expected_results.jsonl")) {
            key.entry(r["claim_id"].as_str().unwrap().to_string()).or_default().insert(r["rule_id"].as_str().unwrap().to_string(), r["status"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(pool.len(), 600);
    // the engine once per claim, shared by every batch and scenario
    let results: Vec<Value> = pool.iter().map(|c| run_engine(c).expect("the engine runs")).collect();

    let clock: cg_access::Clock = Arc::new(|| 1_000_000.0);
    let scenarios = [("auto-clear only", "off"), ("auto-clear + auto-request (medium severity)", "medium"), ("auto-clear + auto-request (all severities)", "all")];
    let mut out_rows = vec![];
    for (name, scope) in scenarios {
        let mut s = Series::default();
        for b in 0..BATCHES {
            let store = Arc::new(MemoryQueueStore::new());
            let cfg = RoutingConfig { auto_clear_green: true, audit_percent: AUDIT, auto_request_info: scope.into(), ..RoutingConfig::default() };
            assert!(store.put_config(&to_doc(&cfg).unwrap(), 0).unwrap());
            let sc = store.clone();
            let explain = Arc::new(ExplainStep::new(store.clone(), Arc::new(NoModel), default_guard(), Arc::new(CircuitBreaker::new(clock.clone())), clock.clone(), Arc::new(move || routing_config(&*sc))));
            let rt = cg_queue::flow::Runtime { store: store.clone(), explain, clock: clock.clone(), max_retries: 3 };
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(20261015 + b); // the same batches in every scenario
            let mut idx: Vec<usize> = (0..pool.len()).collect();
            idx.shuffle(&mut rng);
            let (mut cleared, mut requested_only, mut audited, mut for_person) = (0.0, 0.0, 0.0, 0.0);
            let (mut today, mut left, mut requests, mut wrong_c, mut wrong_r) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for i in idx.into_iter().take(BATCH) {
                let claim = &pool[i];
                let id = claim["claim_id"].as_str().unwrap().to_string();
                let ihash = triage::input_hash(claim);
                let receipt = triage::make_receipt(claim, &results[i], &routing_config(&*store), "pack", "eng", 1_000_000.0);
                let doc = json!({"claim_id": id, "version": 1, "input_hash": ihash, "claim": claim, "results": results[i], "receipt": receipt, "advisory": []});
                assert!(store.put_triaged(&doc).unwrap());
                process(&rt, &id, 1, &ihash);
                let d = store.get(&id, None).unwrap().unwrap();
                let flagged = triage::flagged(&d["results"]).len() as f64;
                today += flagged;
                let was_audit = d["events"].as_array().unwrap().iter().any(|e| e["detail"]["why"] == "green_audit" || e["detail"]["info_audit"] == true);
                audited += f64::from(was_audit as u8);
                let sys_decisions: Vec<&Value> = d["decisions"].as_array().unwrap().iter().filter(|x| x["actor"] == AUTO_REQUEST).collect();
                for x in &sys_decisions {
                    requests += 1.0;
                    if key[&id][x["rule_id"].as_str().unwrap()] != "UNABLE_TO_ASSESS" {
                        wrong_r += 1.0; // the system asked for information on a finding the key says is not a missing-data finding
                    }
                }
                if d["decided_by"] == AUTO_CLEAR {
                    cleared += 1.0;
                    if key[&id].values().any(|st| st == "FAIL" || st == "UNABLE_TO_ASSESS") {
                        wrong_c += 1.0;
                    }
                } else if d["decided_by"] == AUTO_REQUEST {
                    requested_only += 1.0;
                } else {
                    for_person += 1.0;
                    left += flagged - sys_decisions.len() as f64;
                }
            }
            s.cleared.push(cleared);
            s.requested_only.push(requested_only);
            s.audited.push(audited);
            s.for_a_person.push(for_person);
            s.findings_today.push(today);
            s.findings_for_a_person.push(left);
            s.system_requests.push(requests);
            s.wrong_clears.push(wrong_c);
            s.wrong_requests.push(wrong_r);
        }
        assert_eq!(s.wrong_clears.iter().sum::<f64>(), 0.0, "{name}: the system cleared a claim the answer key says has a finding");
        assert_eq!(s.wrong_requests.iter().sum::<f64>(), 0.0, "{name}: the system asked about a finding that is not missing data");
        let no_person: Vec<f64> = s.cleared.iter().zip(&s.requested_only).map(|(a, b)| a + b).collect();
        let no_person_pct: Vec<f64> = no_person.iter().map(|n| 100.0 * n / BATCH as f64).collect();
        out_rows.push(json!({"scenario": name, "auto_request_info": scope, "claims_cleared_by_the_system": stat(&s.cleared), "claims_the_system_requested_information_for": stat(&s.requested_only),
            "claims_decided_without_a_person": stat(&no_person), "percent_of_batch_without_a_person": stat(&no_person_pct), "claims_a_person_still_handles": stat(&s.for_a_person),
            "audit_sample_claims": stat(&s.audited), "findings_in_the_batch": stat(&s.findings_today), "findings_a_person_still_decides": stat(&s.findings_for_a_person),
            "findings_the_system_requested": stat(&s.system_requests), "cleared_but_the_key_has_a_finding": stat(&s.wrong_clears), "requested_but_not_missing_data_by_the_key": stat(&s.wrong_requests)}));
    }
    let out = json!({"what": "the real engine, triage and pipeline on the 600 labelled claims, the same 20 random batches of 500 in each scenario, audit 5%", "batches": BATCHES, "batch_size": BATCH, "audit_percent": AUDIT, "scenarios": out_rows,
                     "note": "real labelled claims (the organizers' synthetic mix); the engine matches the key on every claim; the audit draw is random. A request is the system recording request_information for a missing-data finding; nothing is sent to a provider. No review time is measured."});
    let path = root().join("outputs/defense/auto_modes_run.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
