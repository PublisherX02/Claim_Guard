//! Intake, the outbox relay, reconciliation, the pipeline and the worker: how a claim gets from the front door to `ready`
//! (`intake.py`, `relay.py`, `reconcile.py`, `pipeline.py`, `worker.py`, `leases.py` in the Python build).

use crate::explain::ExplainStep;
use crate::routing_config::{self, RoutingConfig};
use crate::states;
use crate::store::{QueueStore, Result};
use crate::{shadow, triage};
use cg_access::securitylog::SecurityLog;
use cg_access::Clock;
use cg_core::transport;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::{Arc, OnceLock};

pub const RETRIES: usize = 3;

pub fn routing_config(store: &dyn QueueStore) -> RoutingConfig {
    match store.latest_config() {
        Ok(Some(doc)) => routing_config::from_doc(&doc).unwrap_or_default(),
        _ => RoutingConfig::default(),
    }
}

// ---- leases ---------------------------------------------------------------------------------------------------------------

/// Returns the leased document to the pool in one conditional move. None if it was no longer leased (someone decided or released first).
pub fn release(store: &dyn QueueStore, doc: &Value, now: f64, actor: &str, reason: &str) -> Result<Option<Value>> {
    let holder = doc["lease"]["badge_id"].as_str().unwrap_or("");
    let mut set = Map::new();
    set.insert("lease".into(), Value::Null);
    store.transition(doc["claim_id"].as_str().unwrap_or(""), doc["version"].as_i64().unwrap_or(0), "leased", "ready", actor, now, Some(json!({"event": reason, "badge_id": holder})), Some(set), None)
}

// ---- intake ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    #[error("claim refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

fn claim_id_ok(id: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").expect("static pattern")).is_match(id)
}

/// The transport check the rest of the system uses, plus a claim id that is safe as a key and a file name.
pub fn check_claim(claim: &Value) -> std::result::Result<(), String> {
    transport::validate(claim).map_err(str::to_string)?;
    if claim["claim_id"].as_str().is_some_and(claim_id_ok) { Ok(()) } else { Err("claim_id must be 1 to 64 letters, digits, underscore or hyphen".into()) }
}

/// engine(claim) -> the fifteen results as a JSON list.
pub type EngineFn = Arc<dyn Fn(&Value) -> std::result::Result<Value, String> + Send + Sync>;

pub fn engine_fn(engine: Arc<cg_engine::Engine>) -> EngineFn {
    Arc::new(move |claim| {
        let mut errors = vec![];
        let results = engine.review(claim, &mut errors).map_err(|e| e.to_string())?;
        serde_json::to_value(results).map_err(|e| e.to_string())
    })
}

pub struct Intake {
    pub store: Arc<dyn QueueStore>,
    pub engine: EngineFn,
    pub log: Arc<SecurityLog>,
    pub clock: Clock,
    pub rule_pack_hash: String,
    pub engine_version: String,
    /// The advisory (extension) results for a claim; None stores `[]`. Never allowed to stop a claim from being triaged.
    pub advisory: Option<AdvisoryFn>,
}

pub type AdvisoryFn = Arc<dyn Fn(&Value) -> Vec<Value> + Send + Sync>;

/// Earlier claims of the same patient read from the queue store (`workqueue/history.py`), for the history-aware extension rules.
pub struct StoreHistory(pub Arc<dyn QueueStore>);

impl cg_engine::extensions::History for StoreHistory {
    fn earlier_claims(&self, claim: &Value) -> std::result::Result<Vec<Value>, String> {
        let Some(pid) = claim.get("patient_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else { return Ok(vec![]) };
        let rows = self.0.claims_for_patient(pid).map_err(|e| e.to_string())?;
        Ok(cg_engine::extensions::earlier_of(claim, rows))
    }
}

/// The advisory function the server uses: the extension rules over the store's history.
pub fn advisory_fn(ext: Arc<cg_engine::extensions::Extensions>, store: Arc<dyn QueueStore>) -> AdvisoryFn {
    Arc::new(move |claim| {
        let history = StoreHistory(store.clone());
        let mut errors = vec![];
        ext.evaluate(claim, Some(&history), &mut errors).unwrap_or_default()
    })
}

impl Intake {
    fn receipt_for(&self, claim_id: &str, ihash: &str) -> Result<Option<Value>> {
        let Some(latest) = self.store.get(claim_id, None)? else { return Ok(None) };
        for version in (1..=latest["version"].as_i64().unwrap_or(0)).rev() {
            let doc = if Some(version) == latest["version"].as_i64() { Some(latest.clone()) } else { self.store.get(claim_id, Some(version))? };
            if let Some(d) = doc {
                if d["input_hash"] == ihash && d["receipt"]["rule_pack_hash"] == self.rule_pack_hash.as_str() {
                    return Ok(Some(d["receipt"].clone()));
                }
            }
        }
        Ok(None)
    }

    fn advisory_for(&self, claim: &Value) -> Vec<Value> {
        let Some(f) = &self.advisory else { return vec![] };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(claim))).unwrap_or_default()
    }

    /// Validates, runs the engine, writes the triage receipt and stores everything in one atomic write. The same claim twice is stored
    /// once and returns the same receipt; a changed body under an existing claim id becomes the next version.
    pub fn submit(&self, claim: &Value) -> std::result::Result<Value, IntakeError> {
        if !claim.is_object() {
            return Err(IntakeError::Refused("a claim must be an object".into()));
        }
        check_claim(claim).map_err(IntakeError::Refused)?;
        let claim_id = claim["claim_id"].as_str().unwrap_or("").to_string();
        let ihash = triage::input_hash(claim);
        let cfg = routing_config(&*self.store);
        let fail = |e: cg_access::store::StoreError| IntakeError::Failed(e.to_string());
        for _ in 0..RETRIES {
            if let Some(found) = self.receipt_for(&claim_id, &ihash).map_err(fail)? {
                return Ok(found);
            }
            let version = self.store.get(&claim_id, None).map_err(fail)?.map_or(1, |l| l["version"].as_i64().unwrap_or(0) + 1);
            let results = (self.engine)(claim).map_err(IntakeError::Failed)?;
            let receipt = triage::make_receipt(claim, &results, &cfg, &self.rule_pack_hash, &self.engine_version, (self.clock)());
            let doc = json!({"claim_id": claim_id, "version": version, "input_hash": ihash, "claim": claim, "results": results, "receipt": receipt, "advisory": self.advisory_for(claim)});
            if self.store.put_triaged(&doc).map_err(fail)? {
                // shadow mode must never affect a claim
                let _ = shadow::record(&*self.store, &claim_id, version, shadow::predict_clear(&receipt), receipt["created_at"].as_f64().unwrap_or(0.0));
                let _ = self.log.record(
                    "triage_receipt",
                    json!({"claim_id": claim_id, "input_hash": ihash, "result_hash": receipt["result_hash"], "lane": receipt["lane"], "score": receipt["score"], "config_version": receipt["config_version"]}),
                );
                return Ok(receipt);
            }
        }
        Err(IntakeError::Failed("the claim could not be stored: concurrent submissions kept taking its version".into()))
    }
}

// ---- relay ----------------------------------------------------------------------------------------------------------------

pub const RELAY_BATCH: usize = 500;

/// Publishes the claims whose task has not been handed to the broker yet. The marker is cleared only after the publish returned, so a
/// broker outage leaves it for the next sweep; a claim can be published twice, and the consuming task is idempotent. Returns the counts.
pub fn sweep(store: &dyn QueueStore, publish: &dyn Fn(&str, i64, &str) -> std::result::Result<(), String>, now: f64, max_age: f64) -> Value {
    let (mut published, mut failed, mut stale) = (0, 0, 0);
    for doc in store.pending_outbox(RELAY_BATCH).unwrap_or_default() {
        if now - doc["receipt"]["created_at"].as_f64().unwrap_or(0.0) > max_age {
            stale += 1;
        }
        let (id, version) = (doc["claim_id"].as_str().unwrap_or(""), doc["version"].as_i64().unwrap_or(0));
        if publish(id, version, doc["input_hash"].as_str().unwrap_or("")).is_err() {
            failed += 1;
            continue;
        }
        let _ = store.clear_outbox(id, version);
        published += 1;
    }
    json!({"published": published, "failed": failed, "stale": stale})
}

/// The repair for a broker that lost messages (Redis restarted without its data, or someone flushed it). A claim the pipeline has not
/// finished, with nothing left in the broker to finish it, is a lost message: publish it again. Consumers are idempotent, so a repeat is
/// harmless. It does nothing while the broker still holds messages (a long backlog is not loss, and republishing it would only pile up
/// copies), when the broker does not answer, or for claims younger than `older_than` seconds.
pub fn republish_lost(store: &dyn QueueStore, broker: &dyn crate::broker::Broker, now: f64, older_than: f64) -> Value {
    match broker.stats() {
        Err(_) => return json!({"republished": 0, "skipped": "the broker does not answer"}),
        Ok(st) if st.length > 0 => return json!({"republished": 0, "skipped": "the broker still holds messages"}),
        Ok(_) => {}
    }
    let (mut republished, mut failed) = (0, 0);
    'all: for state in PIPELINE_STATES {
        for doc in store.by_state(state, RELAY_BATCH).unwrap_or_default() {
            if republished + failed >= RELAY_BATCH {
                break 'all;
            }
            if now - doc["state_at"].as_f64().unwrap_or(now) <= older_than {
                continue;
            }
            match broker.publish(doc["claim_id"].as_str().unwrap_or(""), doc["version"].as_i64().unwrap_or(0), doc["input_hash"].as_str().unwrap_or("")) {
                Ok(()) => republished += 1,
                Err(_) => {
                    failed += 1;
                    break 'all; // the broker went away: the next run tries again
                }
            }
        }
    }
    json!({"republished": republished, "failed": failed})
}

// ---- reconcile ------------------------------------------------------------------------------------------------------------

pub struct Report {
    pub findings: Vec<Value>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.findings.iter().all(|f| f["repaired"] == true)
    }
}

fn add(report: &mut Report, check: &str, doc: Option<&Value>, repaired: bool, extra: Value) {
    let mut row = json!({"check": check, "repaired": repaired});
    if let (Some(o), Some(e)) = (row.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    if let Some(d) = doc {
        row["claim_id"] = d["claim_id"].clone();
        row["version"] = d["version"].clone();
    }
    report.findings.push(row);
}

/// A periodic audit that the queue documents are in sane states, with one safe repair: a lease past its expiry goes back to the pool.
/// Everything else is reported for a person to look at, never guessed at.
pub fn reconcile(store: &dyn QueueStore, now: f64, actor: &str) -> Report {
    const BIG: usize = 100_000;
    let mut report = Report { findings: vec![] };
    let counts = store.counts().unwrap_or_default();
    let total: i64 = counts.values().sum();
    for (key, n) in &counts {
        let state = key.split('|').next().unwrap_or("");
        if !states::STATES.contains(&state) {
            add(&mut report, "unknown_state", None, false, json!({"state": state, "count": n}));
        }
    }
    let (mut listed, mut leased): (i64, std::collections::BTreeMap<String, Vec<Value>>) = (0, Default::default());
    for state in states::STATES {
        let docs = store.by_state(state, BIG).unwrap_or_default();
        listed += docs.len() as i64;
        for d in docs {
            let age = now - d["state_at"].as_f64().unwrap_or(0.0);
            match state {
                "triaged" if age > 60.0 => add(&mut report, "stuck", Some(&d), false, json!({"state": state, "age_seconds": age})),
                "explained" | "explanation_skipped" if age > 60.0 => add(&mut report, "stuck", Some(&d), false, json!({"state": state, "age_seconds": age})),
                "ready" | "awaiting_countersign" if age > 86_400.0 => add(&mut report, "waiting_too_long", Some(&d), false, json!({"state": state, "age_seconds": age})),
                "decided" if d["decided_by"].is_null() || d["decided_by"] == "" => add(&mut report, "decided_without_a_person", Some(&d), false, json!({})),
                "leased" => {
                    if d["lease"].is_null() {
                        add(&mut report, "leased_without_a_lease", Some(&d), false, json!({}));
                        continue;
                    }
                    leased.entry(d["claim_id"].as_str().unwrap_or("").to_string()).or_default().push(d.clone());
                    if d["lease"]["expires_at"].as_f64().unwrap_or(f64::MAX) <= now {
                        let moved = release(store, &d, now, actor, "lease_expired").ok().flatten();
                        add(&mut report, "lease_expired", Some(&d), moved.is_some(), json!({"badge_id": d["lease"]["badge_id"]}));
                    }
                }
                _ => {}
            }
        }
    }
    for docs in leased.values().filter(|d| d.len() > 1) {
        let mut versions: Vec<i64> = docs.iter().filter_map(|d| d["version"].as_i64()).collect();
        versions.sort();
        add(&mut report, "two_live_leases", Some(&docs[0]), false, json!({"versions": versions}));
    }
    if listed != total {
        add(&mut report, "count_mismatch", None, false, json!({"documents": total, "listed": listed}));
    }
    for d in store.pending_outbox(BIG).unwrap_or_default() {
        let age = now - d["receipt"]["created_at"].as_f64().unwrap_or(0.0);
        if age > 60.0 {
            add(&mut report, "unpublished_marker", Some(&d), false, json!({"age_seconds": age}));
        }
    }
    report
}

// ---- pipeline and worker -----------------------------------------------------------------------------------------------------

const PIPELINE_STATES: [&str; 3] = ["triaged", "explained", "explanation_skipped"];

fn make_ready(store: &dyn QueueStore, claim_id: &str, version: i64, from: &str, now: f64, why: &str) -> &'static str {
    match store.transition(claim_id, version, from, "ready", "system:pipeline", now, Some(json!({"why": why})), None, None) {
        Ok(Some(_)) => "ready",
        _ => "noop",
    }
}

/// Runs the claim up to `ready`. Idempotent: safe to run twice, out of order or after a crash. Returns "ready" when this call moved it
/// there, otherwise "noop". Dealing is not a step here: the dispatcher owns the `ready` pool.
pub fn advance(store: &dyn QueueStore, claim_id: &str, version: i64, explain: &ExplainStep, now: f64) -> &'static str {
    let Ok(Some(doc)) = store.get(claim_id, Some(version)) else { return "noop" };
    let state = doc["state"].as_str().unwrap_or("").to_string();
    if !PIPELINE_STATES.contains(&state.as_str()) {
        return "noop";
    }
    let mut state = state;
    if state == "triaged" {
        if doc["receipt"]["lane"] == "green" {
            return make_ready(store, claim_id, version, "triaged", now, "green");
        }
        if explain.run(claim_id, version) == "noop" {
            return "noop"; // another worker is handling it
        }
        match store.get(claim_id, Some(version)) {
            Ok(Some(d)) if ["explained", "explanation_skipped"].contains(&d["state"].as_str().unwrap_or("")) => state = d["state"].as_str().unwrap_or("").to_string(),
            _ => return "noop",
        }
    }
    make_ready(store, claim_id, version, &state, now, if state == "explained" { "explained" } else { "skipped" })
}

pub struct Runtime {
    pub store: Arc<dyn QueueStore>,
    pub explain: Arc<ExplainStep>,
    pub clock: Clock,
    pub max_retries: u32,
}

/// Handles one delivered message: it must still match the stored claim (a stale or forged message is a no-op).
pub fn process(rt: &Runtime, claim_id: &str, version: i64, input_hash: &str) -> &'static str {
    match rt.store.get(claim_id, Some(version)) {
        Ok(Some(d)) if d["input_hash"] == input_hash => advance(&*rt.store, claim_id, version, &rt.explain, (rt.clock)()),
        _ => "noop",
    }
}

/// Parks a claim that is still in the pipeline and records one dead letter. The claim is moved first and the record is written only if this call moved it, so a claim
/// that already finished (or was already parked by an earlier delivery whose acknowledgement failed) gets no record, and a repeat of this
/// call writes nothing. The reason is the error kind only: its message could quote claim text. Returns whether it parked the claim.
pub fn dead_letter(rt: &Runtime, claim_id: &str, version: i64, input_hash: &str, reason: &str, attempts: u32) -> bool {
    let now = (rt.clock)();
    let Ok(Some(doc)) = rt.store.get(claim_id, Some(version)) else { return false };
    let state = doc["state"].as_str().unwrap_or("");
    // only a claim still in the pipeline can be parked here: one that is ready, leased or being countersigned is in a person's hands
    if !PIPELINE_STATES.contains(&state) || !states::allowed_from(state).contains(&"dead_lettered") {
        return false;
    }
    let moved = rt.store.transition(claim_id, version, state, "dead_lettered", "system:worker", now, Some(json!({"reason": reason, "attempts": attempts})), None, None);
    if !matches!(moved, Ok(Some(_))) {
        return false;
    }
    let id = cg_access::settings::random_token(16);
    let _ = rt.store.add_dead_letter(&json!({"dead_id": id, "claim_id": claim_id, "version": version, "input_hash": input_hash, "reason": reason, "attempts": attempts, "at": now}));
    true
}
