//! The explanation step under load, with a real model server: the 600 public claims go through `ExplainStep` from several workers, first
//! with an empty template cache (cold) and then again (warm). It counts cache hits, model calls and outcomes, and writes
//! `outputs/defense/explain_load.json`. Skipped unless a model is configured:
//!
//!     QUEUE_AI_PROVIDER=ollama OLLAMA_MODEL=gemma3:4b EXPLAIN_LOAD_WORKERS=8 cargo test -p cg-queue --test explain_load --release -- --nocapture
//!
//! EXPLAIN_LOAD_RATE sets the routing configuration's ai_per_minute for the run (default 600; the product default is 30).

use cg_access::Clock;
use cg_engine::Engine;
use cg_queue::breaker::{CircuitBreaker, ModelError};
use cg_queue::explain::{default_guard, ExplainStep, Model};
use cg_queue::routing_config::RoutingConfig;
use cg_queue::store::{MemoryQueueStore, QueueStore};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn lines(path: PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// Wraps the real model to count and time its calls.
struct Counting {
    inner: Arc<dyn Model>,
    calls: AtomicUsize,
    failures: AtomicUsize,
    seconds: Mutex<Vec<f64>>,
    answers: Mutex<BTreeMap<String, String>>,
}

impl Model for Counting {
    fn ask(&self, request: &Value, deadline: f64) -> Result<String, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let t = Instant::now();
        let out = self.inner.ask(request, deadline);
        self.seconds.lock().unwrap().push(t.elapsed().as_secs_f64());
        if out.is_err() {
            self.failures.fetch_add(1, Ordering::SeqCst);
        }
        if let Ok(text) = &out {
            self.answers.lock().unwrap().insert(format!("{}|{}", request["rule_id"].as_str().unwrap_or(""), request["failure_shape"].as_str().unwrap_or("")), text.clone());
        }
        out
    }
    fn name(&self) -> String {
        self.inner.name()
    }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[((v.len() as f64 - 1.0) * p) as usize] * 100.0).round() / 100.0
}

fn pass(label: &str, step: &Arc<ExplainStep>, counting: &Arc<Counting>, store: &Arc<MemoryQueueStore>, docs: &[(String, Value)], workers: usize) -> Value {
    for (id, d) in docs {
        let mut d = d.clone();
        d["claim_id"] = json!(format!("{label}-{id}"));
        d["input_hash"] = json!(format!("h-{label}-{id}"));
        d["claim"]["claim_id"] = d["claim_id"].clone();
        d["receipt"]["claim_id"] = d["claim_id"].clone();
        assert!(store.put_triaged(&d).unwrap());
    }
    let (calls0, fail0) = (counting.calls.load(Ordering::SeqCst), counting.failures.load(Ordering::SeqCst));
    counting.seconds.lock().unwrap().clear();
    let next = AtomicUsize::new(0);
    let outcomes: Mutex<BTreeMap<String, i64>> = Mutex::new(BTreeMap::new());
    let per_claim: Mutex<Vec<f64>> = Mutex::new(vec![]);
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= docs.len() {
                    break;
                }
                let id = format!("{label}-{}", docs[i].0);
                let t = Instant::now();
                let moved = step.run(&id, 1);
                per_claim.lock().unwrap().push(t.elapsed().as_secs_f64());
                let d = store.get(&id, None).unwrap().unwrap();
                let key = format!("{moved}/{}", d["explanation"]["outcome"].as_str().unwrap_or("-"));
                *outcomes.lock().unwrap().entry(key).or_default() += 1;
            });
        }
    });
    let wall = t0.elapsed().as_secs_f64();
    // per-finding sources
    let mut sources: BTreeMap<String, i64> = BTreeMap::new();
    for (id, _) in docs {
        let d = store.get(&format!("{label}-{id}"), None).unwrap().unwrap();
        if let Some(f) = d["explanation"]["findings"].as_object() {
            for v in f.values() {
                *sources.entry(v["source"].as_str().unwrap_or("?").to_string()).or_default() += 1;
            }
        }
    }
    let findings: i64 = sources.values().sum();
    let calls = counting.calls.load(Ordering::SeqCst) - calls0;
    let mut secs = counting.seconds.lock().unwrap().clone();
    let mut pc = per_claim.lock().unwrap().clone();
    let row = json!({
        "pass": label, "claims": docs.len(), "workers": workers, "seconds": (wall * 10.0).round() / 10.0,
        "claims_per_minute": ((docs.len() as f64 / wall * 60.0) * 10.0).round() / 10.0,
        "findings": findings, "model_calls": calls, "model_call_failures": counting.failures.load(Ordering::SeqCst) - fail0,
        "findings_by_source": sources, "outcomes": outcomes.into_inner().unwrap(),
        "findings_served_without_a_model_call_percent": if findings > 0 { ((1.0 - calls as f64 / findings as f64) * 1000.0).round() / 10.0 } else { 0.0 },
        "model_call_seconds_p50": percentile(&mut secs, 0.5), "model_call_seconds_p95": percentile(&mut secs, 0.95),
        "claim_seconds_p50": percentile(&mut pc, 0.5), "claim_seconds_p95": percentile(&mut pc, 0.95), "claim_seconds_max": percentile(&mut pc, 1.0),
    });
    println!("{}", serde_json::to_string(&row).unwrap());
    row
}

#[test]
fn the_explanation_step_under_load_with_a_cold_and_a_warm_cache() {
    let env: HashMap<String, String> = std::env::vars().collect();
    let Some(real) = cg_queue::model::model_from_env(&env, cg_queue::model::rulebook_rules(&root()).unwrap()).unwrap() else {
        println!("QUEUE_AI_PROVIDER is not set: nothing to measure");
        return;
    };
    let workers: usize = env.get("EXPLAIN_LOAD_WORKERS").and_then(|s| s.parse().ok()).unwrap_or(8);
    let rate: i64 = env.get("EXPLAIN_LOAD_RATE").and_then(|s| s.parse().ok()).unwrap_or(600);
    let engine = Engine::load(&root()).expect("the rule engine loads");
    let mut pool: Vec<Value> = vec![];
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
    }
    let mut docs: Vec<(String, Value)> = vec![];
    for (i, claim) in pool.iter().enumerate() {
        let mut errors = vec![];
        let results = serde_json::to_value(engine.review(claim, &mut errors).unwrap()).unwrap();
        let any = results.as_array().unwrap().iter().any(|r| ["FAIL", "UNABLE_TO_ASSESS"].contains(&r["status"].as_str().unwrap_or("")));
        if !any {
            continue; // a claim without a finding never reaches the explanation step
        }
        let mut d = cg_queue::contract::make_doc(&format!("X{i}"), 1, "h", "A", "decide", 2, "P1", 1000.0, None);
        d["claim"] = claim.clone();
        d["results"] = results;
        docs.push((format!("{i}"), d));
    }
    println!("{} of {} claims have a finding", docs.len(), pool.len());

    let counting = Arc::new(Counting { inner: real, calls: AtomicUsize::new(0), failures: AtomicUsize::new(0), seconds: Mutex::new(vec![]), answers: Mutex::new(BTreeMap::new()) });
    let store = Arc::new(MemoryQueueStore::new());
    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    let clock: Clock = Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64());
    let breaker = Arc::new(CircuitBreaker::new(clock.clone()));
    let step = Arc::new(ExplainStep::new(store.clone(), counting.clone(), default_guard(), breaker, clock, Arc::new(move || RoutingConfig { ai_per_minute: rate, ai_daily_budget: 100_000, ..RoutingConfig::default() })));
    let _ = start;

    let cold = pass("cold", &step, &counting, &store, &docs, workers);
    // why the guard refused the templates it refused (cold pass): rule|shape -> (findings, reason)
    let mut refusals: BTreeMap<String, (i64, String)> = BTreeMap::new();
    for (id, d) in &docs {
        let stored = store.get(&format!("cold-{id}"), None).unwrap().unwrap();
        for r in cg_queue::triage::flagged(&d["results"]) {
            let rule = r["rule_id"].as_str().unwrap_or("");
            if stored["explanation"]["findings"][rule]["source"] != "template" {
                continue;
            }
            let key = format!("{rule}|{}", cg_queue::explain::failure_shape(r));
            let answers = counting.answers.lock().unwrap();
            let why = match answers.get(&key) {
                Some(t) => cg_queue::guard::check_grounding(&cg_queue::explain::fill(t, r), r, None).err().unwrap_or_else(|| "accepted on this claim".into()),
                None => "no model answer".into(),
            };
            refusals.entry(key).or_insert((0, why)).0 += 1;
        }
    }
    println!("refused templates: {}", serde_json::to_string(&refusals).unwrap());
    let warm = pass("warm", &step, &counting, &store, &docs, workers);
    // what the same findings would cost with no cache: one model call each, at the model's own measured speed
    let findings = cold["findings"].as_i64().unwrap();
    let report = json!({
        "model": counting.name(), "claims_with_findings": docs.len(), "findings": findings, "workers": workers, "ai_per_minute_setting": rate,
        "cold": cold, "warm": warm, "guard_refusals_cold_pass": refusals,
        "note": "Real engine, real template cache (in-memory store), real model server on this machine. Cold = empty cache; warm = the same claims again under new ids. A cache stampede (several workers asking for the same missing template at once) shows as model_calls above the number of distinct templates. Measured on this machine only; the target server is a different card.",
    });
    let out = root().join("outputs").join("defense");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("explain_load.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    // every model call on a warm cache is a retry of a template the guard refused (a refused template is not cached)
    let refused_warm = warm["findings_by_source"]["template"].as_i64().unwrap_or(0);
    assert!(warm["model_calls"].as_i64().unwrap() <= refused_warm + 2, "a warm cache asks the model only again for refused templates");
}
