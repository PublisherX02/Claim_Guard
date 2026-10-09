//! The queue store: an interface, the validation both implementations share, and the in-memory twin (`src/workqueue/store.py`).
//!
//! One claim version is one document, so intake is one atomic write and every state move is one conditional update that also appends its
//! event. The MongoDB implementation (`cg-mongo`) must pass the same contract (`contract::run`), which is how this twin is checked against
//! the real database. Identifiers are plain strings, so an operator object such as `{"$ne": null}` cannot reach a database query.
//!
//! Document: `{claim_id, version, input_hash, claim, results, receipt, state, state_at, enqueue_pending, lease, events, decided_by,
//! shadow, explanation, decisions, escalated, signoff}`. Unique on `(claim_id, version)` and on `(claim_id, input_hash,
//! receipt.rule_pack_hash)`: the same claim body under a new rule pack is the next version, never a duplicate.

use crate::states;
use cg_access::store::StoreError;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

pub type Doc = Value;
pub type Result<T> = std::result::Result<T, StoreError>;

pub const SETTABLE: [&str; 7] = ["lease", "decided_by", "shadow", "explanation", "enqueue_pending", "escalated", "signoff"];
pub const DEALABLE: [&str; 2] = ["ready", "awaiting_countersign"];
const HISTORY_FIELDS: [&str; 9] = ["claim_id", "patient_id", "provider_id", "submission_date", "lines", "authorizations", "notes", "diagnosis_code", "attachments"];

pub fn invalid<T>(m: impl Into<String>) -> Result<T> {
    Err(StoreError::Invalid(m.into()))
}

pub fn check_version(v: i64) -> Result<()> {
    if v >= 1 { Ok(()) } else { invalid("version must be a whole number of 1 or more") }
}

pub fn check_number(v: f64, what: &str) -> Result<()> {
    if v.is_finite() { Ok(()) } else { invalid(format!("{what} must be a finite number")) }
}

pub fn check_set_fields(f: &Option<Map<String, Value>>) -> Result<Map<String, Value>> {
    match f {
        None => Ok(Map::new()),
        Some(m) if m.keys().all(|k| SETTABLE.contains(&k.as_str())) => Ok(m.clone()),
        Some(_) => invalid(format!("a transition may set only: {}", SETTABLE.join(", "))),
    }
}

/// Validates a document handed to `put_triaged` and returns the full stored form (state triaged, outbox marker set).
pub fn prepare_new_doc(doc: &Value) -> Result<Value> {
    let obj = doc.as_object().filter(|o| {
        ["claim_id", "version", "input_hash", "claim", "results", "receipt"].iter().all(|k| o.contains_key(*k)) && o.keys().all(|k| ["claim_id", "version", "input_hash", "claim", "results", "receipt", "advisory"].contains(&k.as_str()))
    });
    let Some(obj) = obj else { return invalid("a new claim document must have: claim, claim_id, input_hash, receipt, results, version (and may have advisory)") };
    if obj.get("advisory").is_some_and(|a| !a.is_array()) {
        return invalid("advisory must be a list");
    }
    let text = |k: &str| obj[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    if text("claim_id").is_none() || text("input_hash").is_none() {
        return invalid("claim_id and input_hash must not be empty");
    }
    let version = obj["version"].as_i64().filter(|v| obj["version"].is_i64() && *v >= 1);
    if version.is_none() {
        return invalid("version must be a whole number of 1 or more");
    }
    if !obj["claim"].is_object() || !obj["receipt"].is_object() || !obj["results"].is_array() {
        return invalid("claim and receipt must be objects and results a list");
    }
    let receipt = &obj["receipt"];
    let now = receipt["created_at"].as_f64().filter(|f| f.is_finite());
    let Some(now) = now else { return invalid("receipt created_at must be a finite number") };
    for key in ["lane", "eligibility"] {
        if !receipt[key].is_string() {
            return invalid(format!("{key} must be a string"));
        }
    }
    let mut full = obj.clone();
    full.entry("advisory").or_insert_with(|| json!([]));
    let event = states::make_event("received", "triaged", "system:intake", now, Some(json!({"lane": receipt["lane"], "score": receipt.get("score").cloned().unwrap_or(json!(0))}))).map_err(StoreError::Invalid)?;
    for (k, v) in [("state", json!("triaged")), ("state_at", json!(now)), ("enqueue_pending", json!(true)), ("lease", Value::Null), ("decided_by", Value::Null), ("shadow", Value::Null),
                   ("explanation", Value::Null), ("decisions", json!([])), ("escalated", json!(false)), ("signoff", Value::Null), ("events", json!([event]))] {
        full.insert(k.into(), v);
    }
    Ok(Value::Object(full))
}

pub fn check_decision(d: &Value) -> Result<()> {
    if !d.as_object().is_some_and(|o| !o.is_empty()) {
        return invalid("a decision must be a non-empty object");
    }
    states::check_detail(d).map_err(StoreError::Invalid)
}

pub fn check_config_doc(doc: &Value, expected_version: i64) -> Result<()> {
    let Some(v) = doc.get("version").filter(|v| v.is_i64()).and_then(Value::as_i64) else { return invalid("a configuration document needs a whole-number version") };
    if expected_version < 0 || v != expected_version + 1 {
        return invalid("a configuration document must be exactly the next version");
    }
    Ok(())
}

pub fn check_id_doc(doc: &Value, key: &str) -> Result<()> {
    if doc.get(key).and_then(Value::as_str).is_some_and(|s| !s.is_empty()) { Ok(()) } else { invalid(format!("the document needs a text {key}")) }
}

pub fn project_history(claim: &Value) -> Value {
    Value::Object(HISTORY_FIELDS.iter().map(|k| ((*k).to_string(), claim.get(*k).cloned().unwrap_or(Value::Null))).collect())
}

pub fn lane_key(doc: &Value) -> String {
    format!("{}|{}|{}", doc["state"].as_str().unwrap_or(""), doc["receipt"]["lane"].as_str().unwrap_or(""), doc["receipt"]["eligibility"].as_str().unwrap_or(""))
}

/// The job record both stores keep per scheduled job.
pub fn job_record(old: Option<&Value>, name: &str, now: f64, ok: bool, detail: &str) -> Value {
    let runs = old.and_then(|o| o["runs"].as_i64()).unwrap_or(0) + 1;
    let failures = old.and_then(|o| o["failures"].as_i64()).unwrap_or(0) + if ok { 0 } else { 1 };
    let last_ok = if ok { json!(now) } else { old.map(|o| o["last_ok_at"].clone()).unwrap_or(Value::Null) };
    json!({"name": name, "at": now, "ok": ok, "detail": detail, "runs": runs, "failures": failures, "last_ok_at": last_ok})
}

pub fn check_job(name: &str, now: f64, detail: &str) -> Result<()> {
    check_number(now, "now")?;
    if name.is_empty() || detail.chars().count() > 200 {
        return invalid("ok must be a boolean and detail short text");
    }
    Ok(())
}

pub trait QueueStore: Send + Sync {
    // intake and reads
    fn put_triaged(&self, doc: &Value) -> Result<bool>;
    fn get(&self, claim_id: &str, version: Option<i64>) -> Result<Option<Doc>>;
    #[allow(clippy::too_many_arguments)]
    fn transition(&self, claim_id: &str, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>, set_fields: Option<Map<String, Value>>, holder: Option<&str>)
        -> Result<Option<Doc>>;
    fn add_decision(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, decision: &Value) -> Result<Option<Doc>>;
    fn set_shadow(&self, claim_id: &str, version: i64, shadow: &Value) -> Result<bool>;
    // light queries
    fn latest_versions(&self, claim_ids: &[String]) -> Result<HashMap<String, i64>>;
    fn leased_summary(&self) -> Result<Vec<Value>>;
    fn inbox_load(&self, badge_id: &str) -> Result<(i64, i64)>;
    // outbox
    fn pending_outbox(&self, limit: usize) -> Result<Vec<Doc>>;
    fn clear_outbox(&self, claim_id: &str, version: i64) -> Result<bool>;
    // queues and leases
    fn by_state(&self, state: &str, limit: usize) -> Result<Vec<Doc>>;
    fn counts(&self) -> Result<BTreeMap<String, i64>>;
    fn lease(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, expires_at: f64) -> Result<Option<Doc>>;
    fn inbox(&self, badge_id: &str) -> Result<Vec<Doc>>;
    fn heartbeat(&self, badge_id: &str, now: f64, expires_at: f64) -> Result<i64>;
    fn expired(&self, now: f64) -> Result<Vec<Doc>>;
    fn claims_for_patient(&self, patient_id: &str) -> Result<Vec<Value>>;
    // configuration
    fn put_config(&self, doc: &Value, expected_version: i64) -> Result<bool>;
    fn latest_config(&self) -> Result<Option<Value>>;
    fn config_history(&self) -> Result<Vec<Value>>;
    // deals, dead letters, cache, counters, jobs
    fn append_deal(&self, doc: &Value) -> Result<()>;
    fn get_deal(&self, deal_id: &str) -> Result<Option<Value>>;
    fn deals(&self, limit: usize) -> Result<Vec<Value>>;
    fn add_dead_letter(&self, doc: &Value) -> Result<()>;
    fn dead_letters(&self, limit: usize) -> Result<Vec<Value>>;
    fn pop_dead_letter(&self, dead_id: &str) -> Result<Option<Value>>;
    fn cache_get(&self, key: &str) -> Result<Option<String>>;
    fn cache_put(&self, key: &str, text: &str) -> Result<()>;
    fn bump(&self, counter: &str, window_key: &str, limit: i64) -> Result<bool>;
    fn record_job(&self, name: &str, now: f64, ok: bool, detail: &str) -> Result<()>;
    fn jobs(&self) -> Result<Vec<Value>>;
    fn ping(&self) -> bool;
}

#[derive(Default)]
struct Inner {
    docs: BTreeMap<(String, i64), Value>,
    configs: Vec<Value>,
    deals: Vec<Value>,
    dead: Vec<(String, Value)>,
    jobs: BTreeMap<String, Value>,
    cache: HashMap<String, String>,
    counters: HashMap<(String, String), i64>,
}

/// Every method takes one lock, so each behaves like the single atomic database operation it stands in for.
#[derive(Default)]
pub struct MemoryQueueStore {
    inner: Mutex<Inner>,
}

impl MemoryQueueStore {
    pub fn new() -> MemoryQueueStore {
        MemoryQueueStore::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn f(v: &Value, path: &[&str]) -> f64 {
    path.iter().fold(v, |acc, k| &acc[*k]).as_f64().unwrap_or(0.0)
}

fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    path.iter().fold(v, |acc, k| &acc[*k]).as_str().unwrap_or("")
}

fn sort_by_time(rows: &mut [&Value], time: impl Fn(&Value) -> f64) {
    rows.sort_by(|a, b| time(a).partial_cmp(&time(b)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| s(a, &["claim_id"]).cmp(s(b, &["claim_id"]))));
}

impl QueueStore for MemoryQueueStore {
    fn put_triaged(&self, doc: &Value) -> Result<bool> {
        let full = prepare_new_doc(doc)?;
        let mut g = self.lock();
        let (cid, ver) = (s(&full, &["claim_id"]).to_string(), full["version"].as_i64().unwrap_or(0));
        let clash = g.docs.values().any(|d| {
            s(d, &["claim_id"]) == cid && (d["version"].as_i64() == Some(ver) || (s(d, &["input_hash"]) == s(&full, &["input_hash"]) && d["receipt"]["rule_pack_hash"] == full["receipt"]["rule_pack_hash"]))
        });
        if clash {
            return Ok(false);
        }
        g.docs.insert((cid, ver), full);
        Ok(true)
    }

    fn get(&self, claim_id: &str, version: Option<i64>) -> Result<Option<Doc>> {
        let g = self.lock();
        let version = match version {
            Some(v) => {
                check_version(v)?;
                v
            }
            None => match g.docs.keys().filter(|(c, _)| c == claim_id).map(|(_, v)| *v).max() {
                Some(v) => v,
                None => return Ok(None),
            },
        };
        Ok(g.docs.get(&(claim_id.to_string(), version)).cloned())
    }

    fn transition(&self, claim_id: &str, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>, set_fields: Option<Map<String, Value>>, holder: Option<&str>)
        -> Result<Option<Doc>> {
        check_version(version)?;
        check_number(now, "now")?;
        let event = states::make_event(from, to, actor, now, detail).map_err(StoreError::Invalid)?;
        let fields = check_set_fields(&set_fields)?;
        let mut g = self.lock();
        let Some(doc) = g.docs.get_mut(&(claim_id.to_string(), version)) else { return Ok(None) };
        if doc["state"] != from {
            return Ok(None);
        }
        if let Some(h) = holder {
            if doc["lease"].is_null() || doc["lease"]["badge_id"] != h {
                return Ok(None);
            }
        }
        doc["state"] = json!(to);
        doc["state_at"] = json!(now);
        if let Some(events) = doc["events"].as_array_mut() {
            events.push(event);
        }
        for (k, v) in fields {
            doc[k.as_str()] = v;
        }
        Ok(Some(doc.clone()))
    }

    fn add_decision(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, decision: &Value) -> Result<Option<Doc>> {
        check_version(version)?;
        check_number(now, "now")?;
        check_decision(decision)?;
        let mut g = self.lock();
        let Some(doc) = g.docs.get_mut(&(claim_id.to_string(), version)) else { return Ok(None) };
        let ok = doc["state"] == "leased" && !doc["lease"].is_null() && doc["lease"]["badge_id"] == badge_id && f(doc, &["lease", "expires_at"]) > now;
        if !ok {
            return Ok(None);
        }
        if !doc["decisions"].is_array() {
            doc["decisions"] = json!([]);
        }
        doc["decisions"].as_array_mut().map(|a| a.push(decision.clone()));
        Ok(Some(doc.clone()))
    }

    fn set_shadow(&self, claim_id: &str, version: i64, shadow: &Value) -> Result<bool> {
        check_version(version)?;
        check_decision(shadow)?;
        let mut g = self.lock();
        match g.docs.get_mut(&(claim_id.to_string(), version)) {
            Some(doc) if doc["shadow"].is_null() => {
                doc["shadow"] = shadow.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn latest_versions(&self, claim_ids: &[String]) -> Result<HashMap<String, i64>> {
        let g = self.lock();
        let mut out = HashMap::new();
        for (cid, ver) in g.docs.keys() {
            if claim_ids.contains(cid) && *ver > out.get(cid).copied().unwrap_or(0) {
                out.insert(cid.clone(), *ver);
            }
        }
        Ok(out)
    }

    fn leased_summary(&self) -> Result<Vec<Value>> {
        let g = self.lock();
        Ok(g.docs
            .values()
            .filter(|d| d["state"] == "leased" && !d["lease"].is_null())
            .map(|d| {
                json!({"claim_id": d["claim_id"], "version": d["version"], "badge_id": d["lease"]["badge_id"], "eligibility": d["receipt"]["eligibility"],
                       "escalated": d["escalated"].as_bool().unwrap_or(false), "signoff": !d["signoff"].is_null()})
            })
            .collect())
    }

    fn inbox_load(&self, badge_id: &str) -> Result<(i64, i64)> {
        let g = self.lock();
        let held: Vec<&Value> = g.docs.values().filter(|d| d["state"] == "leased" && !d["lease"].is_null() && d["lease"]["badge_id"] == badge_id).collect();
        Ok((held.len() as i64, held.iter().map(|d| d["receipt"]["score"].as_i64().unwrap_or(0)).sum()))
    }

    fn pending_outbox(&self, limit: usize) -> Result<Vec<Doc>> {
        let g = self.lock();
        let mut rows: Vec<&Value> = g.docs.values().filter(|d| d["enqueue_pending"] == true).collect();
        sort_by_time(&mut rows, |d| f(d, &["receipt", "created_at"]));
        Ok(rows.into_iter().take(limit).cloned().collect())
    }

    fn clear_outbox(&self, claim_id: &str, version: i64) -> Result<bool> {
        check_version(version)?;
        let mut g = self.lock();
        match g.docs.get_mut(&(claim_id.to_string(), version)) {
            Some(doc) if doc["enqueue_pending"] == true => {
                doc["enqueue_pending"] = json!(false);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn by_state(&self, state: &str, limit: usize) -> Result<Vec<Doc>> {
        let g = self.lock();
        let mut rows: Vec<&Value> = g.docs.values().filter(|d| d["state"] == state).collect();
        sort_by_time(&mut rows, |d| f(d, &["state_at"]));
        Ok(rows.into_iter().take(limit).cloned().collect())
    }

    fn counts(&self) -> Result<BTreeMap<String, i64>> {
        let g = self.lock();
        let mut out = BTreeMap::new();
        for d in g.docs.values() {
            *out.entry(lane_key(d)).or_insert(0) += 1;
        }
        Ok(out)
    }

    fn lease(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, expires_at: f64) -> Result<Option<Doc>> {
        check_version(version)?;
        check_number(now, "now")?;
        check_number(expires_at, "expires_at")?;
        let mut g = self.lock();
        let Some(doc) = g.docs.get_mut(&(claim_id.to_string(), version)) else { return Ok(None) };
        let state = doc["state"].as_str().unwrap_or("").to_string();
        if !DEALABLE.contains(&state.as_str()) {
            return Ok(None);
        }
        let event = states::make_event(&state, "leased", badge_id, now, Some(json!({"expires_at": expires_at}))).map_err(StoreError::Invalid)?;
        doc["state"] = json!("leased");
        doc["state_at"] = json!(now);
        doc["lease"] = json!({"badge_id": badge_id, "leased_at": now, "expires_at": expires_at, "heartbeat_at": now});
        doc["events"].as_array_mut().map(|a| a.push(event));
        Ok(Some(doc.clone()))
    }

    fn inbox(&self, badge_id: &str) -> Result<Vec<Doc>> {
        let g = self.lock();
        let mut rows: Vec<&Value> = g.docs.values().filter(|d| d["state"] == "leased" && d["lease"]["badge_id"] == badge_id).collect();
        sort_by_time(&mut rows, |d| f(d, &["lease", "leased_at"]));
        Ok(rows.into_iter().cloned().collect())
    }

    fn heartbeat(&self, badge_id: &str, now: f64, expires_at: f64) -> Result<i64> {
        check_number(now, "now")?;
        check_number(expires_at, "expires_at")?;
        let mut g = self.lock();
        let mut count = 0;
        for d in g.docs.values_mut().filter(|d| d["state"] == "leased" && d["lease"]["badge_id"] == badge_id) {
            d["lease"]["expires_at"] = json!(expires_at);
            d["lease"]["heartbeat_at"] = json!(now);
            count += 1;
        }
        Ok(count)
    }

    fn expired(&self, now: f64) -> Result<Vec<Doc>> {
        check_number(now, "now")?;
        let g = self.lock();
        let mut rows: Vec<&Value> = g.docs.values().filter(|d| d["state"] == "leased" && f(d, &["lease", "expires_at"]) <= now).collect();
        sort_by_time(&mut rows, |d| f(d, &["lease", "expires_at"]));
        Ok(rows.into_iter().cloned().collect())
    }

    fn claims_for_patient(&self, patient_id: &str) -> Result<Vec<Value>> {
        let g = self.lock();
        let mut latest: BTreeMap<&str, &Value> = BTreeMap::new();
        for ((cid, _), d) in g.docs.iter() {
            let newer = latest.get(cid.as_str()).map_or(true, |cur| d["version"].as_i64() > cur["version"].as_i64());
            if newer {
                latest.insert(cid.as_str(), d);
            }
        }
        Ok(latest.values().filter(|d| d["claim"]["patient_id"] == patient_id).map(|d| project_history(&d["claim"])).collect())
    }

    fn put_config(&self, doc: &Value, expected_version: i64) -> Result<bool> {
        check_config_doc(doc, expected_version)?;
        let mut g = self.lock();
        let current = g.configs.last().and_then(|c| c["version"].as_i64()).unwrap_or(0);
        if current != expected_version {
            return Ok(false);
        }
        g.configs.push(doc.clone());
        Ok(true)
    }

    fn latest_config(&self) -> Result<Option<Value>> {
        Ok(self.lock().configs.last().cloned())
    }

    fn config_history(&self) -> Result<Vec<Value>> {
        Ok(self.lock().configs.clone())
    }

    fn append_deal(&self, doc: &Value) -> Result<()> {
        check_id_doc(doc, "deal_id")?;
        self.lock().deals.push(doc.clone());
        Ok(())
    }

    fn get_deal(&self, deal_id: &str) -> Result<Option<Value>> {
        Ok(self.lock().deals.iter().find(|d| d["deal_id"] == deal_id).cloned())
    }

    fn deals(&self, limit: usize) -> Result<Vec<Value>> {
        Ok(self.lock().deals.iter().rev().take(limit).cloned().collect())
    }

    fn add_dead_letter(&self, doc: &Value) -> Result<()> {
        check_id_doc(doc, "dead_id")?;
        let id = s(doc, &["dead_id"]).to_string();
        let mut g = self.lock();
        match g.dead.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = doc.clone(),
            None => g.dead.push((id, doc.clone())),
        }
        Ok(())
    }

    fn dead_letters(&self, limit: usize) -> Result<Vec<Value>> {
        Ok(self.lock().dead.iter().take(limit).map(|(_, d)| d.clone()).collect())
    }

    fn pop_dead_letter(&self, dead_id: &str) -> Result<Option<Value>> {
        let mut g = self.lock();
        Ok(g.dead.iter().position(|(k, _)| k == dead_id).map(|i| g.dead.remove(i).1))
    }

    fn cache_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.lock().cache.get(key).cloned())
    }

    fn cache_put(&self, key: &str, text: &str) -> Result<()> {
        self.lock().cache.insert(key.to_string(), text.to_string());
        Ok(())
    }

    fn bump(&self, counter: &str, window_key: &str, limit: i64) -> Result<bool> {
        if limit < 0 {
            return invalid("limit must be a whole number of 0 or more");
        }
        let mut g = self.lock();
        let used = g.counters.entry((counter.to_string(), window_key.to_string())).or_insert(0);
        if *used >= limit {
            return Ok(false);
        }
        *used += 1;
        Ok(true)
    }

    fn record_job(&self, name: &str, now: f64, ok: bool, detail: &str) -> Result<()> {
        check_job(name, now, detail)?;
        let mut g = self.lock();
        let rec = job_record(g.jobs.get(name), name, now, ok, detail);
        g.jobs.insert(name.to_string(), rec);
        Ok(())
    }

    fn jobs(&self) -> Result<Vec<Value>> {
        Ok(self.lock().jobs.values().cloned().collect())
    }

    fn ping(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_store_meets_the_contract() {
        crate::contract::run(&|| Box::new(MemoryQueueStore::new()));
    }
}
