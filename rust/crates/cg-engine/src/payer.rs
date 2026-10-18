//! Payer rules as data an administrator can change: the policies table (`rules/policies.json`: per policy the currency, the submission window, the allowed
//! providers, the services that need authorization, the documents each service requires, the price and quantity limits).
//!
//! What can change and what cannot. The fifteen rules themselves (the YARA pack) are code reviewed and hashed with the build and are NOT editable here. What a payer
//! contract says, which numbers and lists the rules compare a claim with, is data, and data is what changes when a contract does. Every change is checked
//! (shape, ranges, every service code must exist in the catalogue), described as a list of differences, and written as a new numbered VERSION to an append-only,
//! hash-chained log (`payer_rules.jsonl`, with the actor, the approving administrator, the reason and the full new table), so the history of what the rules
//! were at any time is as tamper-evident as the audit logs. The engine switches to the new table without a restart; the rulebook hash recorded in every triage
//! receipt covers the table, so a claim re-submitted after a change is judged again and the receipt says under which version.
//! Version 0 is the table shipped in `rules/policies.json`. A rollback is a new version with an old table.

use crate::Engine;
use cg_audit::AuditLog;
use cg_core::config::Config;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const MAX_POLICIES: usize = 50;
pub const EVENT: &str = "payer_rules_change";

#[derive(Debug, Default, Clone)]
pub struct Validation {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub changes: Vec<String>,
}

impl Validation {
    pub fn to_json(&self) -> Value {
        json!({"ok": self.errors.is_empty(), "errors": self.errors, "warnings": self.warnings, "changes": self.changes})
    }
}

struct State {
    version: u64,
    policies: Value,
    rows_seen: u64,
}

pub struct PayerRules {
    engine: Arc<Engine>,
    log_path: PathBuf,
    shipped: Value,
    services: BTreeSet<String>,
    rules_json: Value,
    services_json: Value,
    state: Mutex<State>,
    /// the question index, rebuilt when the table version changes
    index: Mutex<Option<(u64, Arc<crate::rag::Index>)>>,
    /// meaning-based search (docs/52): the embedder, its refusal threshold, and the passage vectors per table version. Off unless `set_embedder` is called.
    dense: Mutex<Dense>,
}

/// How long to leave the embedding sidecar alone after it failed, so a dead sidecar costs one failed call in this many seconds, not one per question.
const EMBEDDER_BACKOFF_SECS: u64 = 20;

#[derive(Default)]
struct Dense {
    cfg: Option<(Arc<dyn crate::rag::Embedder>, f64)>,
    /// passage vectors by the hash of the embedded text, so a policy edit re-embeds only the passages that changed
    cache: std::collections::HashMap<String, Vec<f32>>,
    failed_at: Option<std::time::Instant>,
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| format!("{}: {e}", path.display()))
}

fn leaves(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) => m.iter().for_each(|(k, c)| leaves(&if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, c, out)),
        Value::Array(a) => {
            let mut items: Vec<String> = a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect();
            items.sort();
            out.insert(prefix.to_string(), format!("[{}]", items.join(", ")));
        }
        other => {
            out.insert(prefix.to_string(), other.as_str().map(str::to_string).unwrap_or_else(|| other.to_string()));
        }
    }
}

/// A readable list of what differs between two tables, capped.
pub fn diff(old: &Value, new: &Value) -> Vec<String> {
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    leaves("", old, &mut a);
    leaves("", new, &mut b);
    let old_ids: BTreeSet<&String> = old.as_object().map(|m| m.keys().collect()).unwrap_or_default();
    let new_ids: BTreeSet<&String> = new.as_object().map(|m| m.keys().collect()).unwrap_or_default();
    let mut out: Vec<String> = vec![];
    for id in new_ids.difference(&old_ids) {
        out.push(format!("added policy {id}"));
    }
    for id in old_ids.difference(&new_ids) {
        out.push(format!("removed policy {id}"));
    }
    let skip = |path: &str| -> bool {
        let id = path.split('.').next().unwrap_or("");
        !(old_ids.contains(&id.to_string()) && new_ids.contains(&id.to_string()))
    };
    for k in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        if skip(k) {
            continue;
        }
        match (a.get(k), b.get(k)) {
            (Some(x), Some(y)) if x != y => out.push(format!("{k}: {x} -> {y}")),
            (Some(x), None) => out.push(format!("{k}: {x} -> (removed)")),
            (None, Some(y)) => out.push(format!("{k}: (new) -> {y}")),
            _ => {}
        }
    }
    out.truncate(200);
    out
}

fn is_id(s: &str) -> bool {
    (2..=40).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
}

impl PayerRules {
    pub fn open(root: &Path, data_dir: &Path, engine: Arc<Engine>) -> Result<PayerRules, String> {
        let shipped = read_json(&root.join("rules/policies.json"))?;
        let services_json = read_json(&root.join("rules/services.json"))?;
        let rules_json = read_json(&root.join("rules/rules.json"))?;
        let services = services_json.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let p = PayerRules { engine, log_path: data_dir.join("payer_rules.jsonl"), shipped: shipped.clone(), services, rules_json, services_json, state: Mutex::new(State { version: 0, policies: shipped, rows_seen: 0 }), index: Mutex::new(None), dense: Mutex::new(Dense::default()) };
        p.refresh()?;
        Ok(p)
    }

    fn events(&self) -> Result<Vec<Value>, String> {
        Ok(cg_audit::read_rows(&self.log_path).map_err(|e| e.to_string())?.iter().filter_map(|r| serde_json::from_str::<Value>(r).ok()).filter(|r| r["event"]["event_type"] == EVENT).collect())
    }

    /// Reads the log again (another process, the API, may have added a version) and switches the engine to the newest table. True when it changed.
    pub fn refresh(&self) -> Result<bool, String> {
        let rows = cg_audit::read_rows(&self.log_path).map_err(|e| e.to_string())?.len() as u64;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if rows == st.rows_seen && (st.version > 0 || rows == 0) {
            return Ok(false);
        }
        let events = self.events()?;
        let latest = events.last();
        let (version, policies) = match latest {
            Some(e) => (e["event"]["version"].as_u64().unwrap_or(0), e["event"]["policies"].clone()),
            None => (0, self.shipped.clone()),
        };
        st.rows_seen = rows;
        if version == st.version && st.policies == policies {
            return Ok(false);
        }
        let cfg = Config::from_values(&self.rules_json, &policies, &self.services_json).map_err(|e| format!("the stored payer rules do not load: {e}"))?;
        self.engine.set_config(cfg, if policies == self.shipped { None } else { Some(&policies) }); // the shipped table, whatever the version number, is the shipped rulebook
        st.version = version;
        st.policies = policies;
        Ok(true)
    }

    pub fn version(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).version
    }

    pub fn current(&self) -> Value {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({"version": st.version, "policies": st.policies, "services": self.services, "rulebook_hash": self.engine.rulebook_hash()})
    }

    /// "Ask the rulebook" (docs/45 section 5): passages of the rules and of the policy table in force that answer a question, quoted with their sources.
    pub fn ask(&self, question: &str, k: usize) -> Value {
        let (version, policies) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.version, st.policies.clone())
        };
        let ix = {
            let mut g = self.index.lock().unwrap_or_else(|e| e.into_inner());
            match &*g {
                Some((v, ix)) if *v == version => ix.clone(),
                _ => {
                    let ix = Arc::new(crate::rag::Index::build(&self.rules_json, &policies, version));
                    *g = Some((version, ix.clone()));
                    ix
                }
            }
        };
        let (hits, mode, fallback) = match self.ask_semantic(&ix, question, k) {
            Some(Ok(hits)) => (hits, "semantic", None),
            Some(Err(why)) => (ix.search(question, k), "keyword", Some(why)),
            None => (ix.search(question, k), "keyword", None),
        };
        let mut out = crate::rag::hits_json(question, &hits);
        out["table_version"] = json!(version);
        out["mode"] = json!(mode);
        if mode == "semantic" {
            if !hits.is_empty() {
                out["note"] = json!("Closest passages by meaning, quoted from the rulebook and the policies in force; nothing here is generated. A passage can be close in topic without answering the question: read it before you rely on it.");
            }
            out["caveat"] = json!("closest passage by meaning; it may not answer your question");
        }
        if let Some(why) = fallback {
            out["fallback"] = json!(format!("The meaning-based search is not available ({why}); this is the keyword search."));
        }
        out
    }

    /// Turn on meaning-based search for "Ask the rulebook". `threshold` is the lowest cosine at which the closest passage still counts as an answer.
    pub fn set_embedder(&self, embedder: Arc<dyn crate::rag::Embedder>, threshold: f64) {
        let mut d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
        d.cfg = Some((embedder, threshold));
        d.failed_at = None;
    }

    /// None: meaning-based search is not configured. Some(Err): configured but unreachable (the caller falls back to keywords). Some(Ok): its answer.
    fn ask_semantic(&self, ix: &crate::rag::Index, question: &str, k: usize) -> Option<Result<Vec<crate::rag::Hit>, String>> {
        use sha2::{Digest, Sha256};
        let (embedder, threshold) = {
            let d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
            let (e, t) = d.cfg.clone()?;
            if d.failed_at.is_some_and(|t| t.elapsed().as_secs() < EMBEDDER_BACKOFF_SECS) {
                return Some(Err("it failed a moment ago".into()));
            }
            (e, t)
        };
        let result = (|| -> Result<Vec<crate::rag::Hit>, String> {
            if question.trim().is_empty() {
                return Ok(vec![]);
            }
            let texts = ix.embed_texts();
            let keys: Vec<String> = texts.iter().map(|t| hex::encode(Sha256::digest(t.as_bytes()))).collect();
            let missing: Vec<usize> = {
                let d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
                (0..texts.len()).filter(|i| !d.cache.contains_key(&keys[*i])).collect()
            };
            for chunk in missing.chunks(64) {
                let batch: Vec<String> = chunk.iter().map(|i| texts[*i].clone()).collect();
                let vecs = embedder.embed(&batch)?;
                if vecs.len() != batch.len() {
                    return Err("the embedder returned the wrong number of vectors".into());
                }
                let mut d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
                for (i, v) in chunk.iter().zip(vecs) {
                    d.cache.insert(keys[*i].clone(), v);
                }
            }
            let vectors: Vec<Vec<f32>> = {
                let d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
                keys.iter().filter_map(|key| d.cache.get(key).cloned()).collect()
            };
            let q = embedder.embed(&[question.to_string()])?.into_iter().next().ok_or("the embedder returned nothing")?;
            if vectors.len() != texts.len() || vectors.iter().any(|v| v.len() != q.len()) || q.iter().any(|x| !x.is_finite()) {
                return Err("the embedder returned vectors of the wrong shape".into());
            }
            let mut hits = ix.named_fields(question);
            for h in ix.search_dense(&vectors, &q, k, threshold) {
                if !hits.iter().any(|x| x.source == h.source && x.kind == h.kind) {
                    hits.push(h);
                }
            }
            hits.truncate(k);
            Ok(hits)
        })();
        let mut d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
        d.failed_at = if result.is_err() { Some(std::time::Instant::now()) } else { None };
        Some(result)
    }

    pub fn history(&self) -> Vec<Value> {
        let mut out = vec![json!({"version": 0, "actor": null, "approver": null, "time": null, "reason": "the table shipped with the build (rules/policies.json)", "changes": []})];
        for e in self.events().unwrap_or_default() {
            let ev = &e["event"];
            out.push(json!({"version": ev["version"], "actor": ev["actor"], "approver": ev["approver"], "time": e["recorded_at"], "reason": ev["reason"], "changes": ev["summary"]}));
        }
        out
    }

    pub fn policies_of(&self, version: u64) -> Option<Value> {
        if version == 0 {
            return Some(self.shipped.clone());
        }
        self.events().ok()?.into_iter().find(|e| e["event"]["version"].as_u64() == Some(version)).map(|e| e["event"]["policies"].clone())
    }

    pub fn validate(&self, candidate: &Value) -> Validation {
        let current = self.state.lock().unwrap_or_else(|e| e.into_inner()).policies.clone();
        self.validate_against(candidate, &current)
    }

    fn validate_against(&self, candidate: &Value, current: &Value) -> Validation {
        let mut v = Validation::default();
        let Some(map) = candidate.as_object() else {
            v.errors.push("the payer rules must be an object with one entry per policy".into());
            return v;
        };
        if map.is_empty() || map.len() > MAX_POLICIES {
            v.errors.push(format!("between 1 and {MAX_POLICIES} policies are allowed"));
        }
        if let Err(e) = Config::from_values(&self.rules_json, candidate, &self.services_json) {
            v.errors.push(format!("the table does not have the shape the engine reads: {e}"));
            return v;
        }
        for (id, p) in map {
            if !is_id(id) {
                v.errors.push(format!("{id}: a policy id is 2 to 40 capital letters, digits or hyphens"));
            }
            let cur = p["currency"].as_str().unwrap_or("");
            if cur.len() != 3 || !cur.bytes().all(|c| c.is_ascii_uppercase()) {
                v.errors.push(format!("{id}.currency: three capital letters, e.g. SAR"));
            }
            match p["submission_window_days"].as_i64() {
                Some(d) if (0..=3650).contains(&d) => {}
                _ => v.errors.push(format!("{id}.submission_window_days: a whole number of days from 0 to 3650")),
            }
            let providers: Vec<&str> = p["allowed_providers"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            if providers.is_empty() {
                v.errors.push(format!("{id}.allowed_providers: at least one provider (an empty list would refuse every claim)"));
            }
            if providers.iter().collect::<BTreeSet<_>>().len() != providers.len() {
                v.errors.push(format!("{id}.allowed_providers: a provider is listed twice"));
            }
            if providers.iter().any(|x| x.is_empty() || x.len() > 40) {
                v.errors.push(format!("{id}.allowed_providers: a provider id is 1 to 40 characters"));
            }
            for key in ["max_unit_price", "max_quantity_per_line"] {
                for (svc, n) in p[key].as_object().into_iter().flatten() {
                    if !self.services.contains(svc) {
                        v.errors.push(format!("{id}.{key}.{svc}: not a service in the catalogue"));
                    }
                    match n.as_f64() {
                        Some(x) if x.is_finite() && x >= 0.0 && x <= 1_000_000_000.0 && (key == "max_unit_price" || x > 0.0) => {}
                        _ => v.errors.push(format!("{id}.{key}.{svc}: a number {} 1,000,000,000", if key == "max_unit_price" { "from 0 up to" } else { "above 0 and up to" })),
                    }
                }
            }
            for svc in p["auth_required_services"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if !self.services.contains(svc) {
                    v.errors.push(format!("{id}.auth_required_services: {svc} is not a service in the catalogue"));
                }
            }
            for (svc, t) in p["required_documents"].as_object().into_iter().flatten() {
                if !self.services.contains(svc) {
                    v.errors.push(format!("{id}.required_documents.{svc}: not a service in the catalogue"));
                }
                let t = t.as_str().unwrap_or("");
                if t.is_empty() || t.len() > 60 || !t.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
                    v.errors.push(format!("{id}.required_documents.{svc}: a document type is lower-case letters, digits and hyphens"));
                }
            }
            if p["payer_id"].as_str().is_none_or(str::is_empty) {
                v.warnings.push(format!("{id}: no payer_id; the payer-consistency checks have nothing to compare with"));
            }
        }
        let old_ids: BTreeSet<&String> = current.as_object().map(|m| m.keys().collect()).unwrap_or_default();
        for gone in old_ids.iter().filter(|id| !map.contains_key(id.as_str())) {
            v.warnings.push(format!("policy {gone} is removed: claims that name it can no longer be checked against it and will be marked unable to assess"));
        }
        v.changes = diff(current, candidate);
        if v.changes.is_empty() {
            v.warnings.push("nothing differs from the table in force".into());
        }
        v
    }

    /// Writes the candidate as the next version and switches the engine to it. `approver` is the second administrator when the change went through
    /// approval, None for a lone administrator (the security log notes that separately).
    pub fn apply(&self, candidate: &Value, actor: &str, approver: Option<&str>, reason: &str) -> Result<u64, String> {
        let reason = reason.trim();
        if reason.chars().count() < 10 || reason.chars().count() > 300 {
            return Err("write a reason of 10 to 300 characters: why does the payer's contract change?".into());
        }
        self.refresh()?;
        let current = self.state.lock().unwrap_or_else(|e| e.into_inner()).policies.clone();
        let v = self.validate_against(candidate, &current);
        if !v.errors.is_empty() {
            return Err(format!("not applied: {}", v.errors.join("; ")));
        }
        if v.changes.is_empty() {
            return Err("nothing differs from the table in force".into());
        }
        let log = AuditLog::open(&self.log_path).map_err(|e| e.to_string())?;
        let version = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.version + 1
        };
        log.append_system_events(&[json!({"event_type": EVENT, "actor": actor, "approver": approver, "version": version, "reason": reason, "policies": candidate, "summary": v.changes})]).map_err(|e| e.to_string())?;
        self.refresh()?;
        Ok(version)
    }
}

/// Helper for callers that hold a table as text.
pub fn parse(text: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(match v {
        Value::Object(m) => Value::Object(m.into_iter().collect::<Map<_, _>>()),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn open(dir: &Path) -> (Arc<Engine>, PayerRules) {
        let e = Arc::new(Engine::load(&root()).unwrap());
        let p = PayerRules::open(&root(), dir, e.clone()).unwrap();
        (e, p)
    }

    fn claim_over_the_lab_limit() -> Value {
        let text = std::fs::read_to_string(root().join("data/validation/claims.jsonl")).unwrap();
        let mut c: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        c["lines"] = json!([{"line_id": "L1", "service_code": "SVC-LAB", "service_date": c["submission_date"], "modifier": null, "quantity": 1, "unit_price": 300, "net_amount": 300, "authorization_id": null}]);
        c["total_amount"] = json!(300);
        c["authorizations"] = json!([]);
        c
    }

    fn status(e: &Engine, c: &Value, rule: &str) -> String {
        let mut errs = vec![];
        e.review(c, &mut errs).unwrap().into_iter().find(|r| r.rule_id == rule).unwrap().status
    }

    const REASON: &str = "contract amendment: laboratory ceiling raised";

    #[test]
    fn a_change_is_versioned_applied_without_a_restart_and_changes_the_rulebook_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (e, p) = open(dir.path());
        assert_eq!(p.version(), 0);
        let before = e.rulebook_hash();
        assert_eq!(before, e.pack.hash(), "version 0 is the shipped table: the hash is the pack's, as before");
        let claim = claim_over_the_lab_limit();
        let pid = claim["policy_id"].as_str().unwrap().to_string();
        assert_eq!(status(&e, &claim, "R013"), "FAIL", "300 is over the shipped limit of 260");
        let mut table = p.current()["policies"].clone();
        table[&pid]["max_unit_price"]["SVC-LAB"] = json!(400);
        let v = p.validate(&table);
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert!(v.changes.iter().any(|c| c.contains("max_unit_price.SVC-LAB") && c.contains("260") && c.contains("400")), "{:?}", v.changes);
        assert_eq!(p.apply(&table, "CG-4004", Some("CG-4005"), REASON), Ok(1));
        assert_eq!(p.version(), 1);
        assert_ne!(e.rulebook_hash(), before, "a claim sent again is judged again, under the new table");
        assert_ne!(status(&e, &claim, "R013"), "FAIL", "the engine uses the new limit at once");
        let h = p.history();
        assert_eq!(h.len(), 2);
        assert_eq!((h[1]["version"].clone(), h[1]["actor"].clone(), h[1]["approver"].clone()), (json!(1), json!("CG-4004"), json!("CG-4005")));
        let old = p.policies_of(0).unwrap();
        assert_eq!(p.apply(&old, "CG-4004", None, "rollback of version 1: the amendment was withdrawn"), Ok(2), "a rollback is a new version carrying the old table");
        assert_eq!(status(&e, &claim, "R013"), "FAIL");
        assert_eq!(e.rulebook_hash(), before, "the same table gives the same hash again");
    }

    #[test]
    fn nothing_unsafe_is_applied_and_the_reasons_are_given() {
        let dir = tempfile::tempdir().unwrap();
        let (_, p) = open(dir.path());
        let good = p.current()["policies"].clone();
        let pid = good.as_object().unwrap().keys().next().unwrap().clone();
        let bad = |f: &dyn Fn(&mut Value)| {
            let mut t = good.clone();
            f(&mut t[&pid]);
            let v = p.validate(&t);
            assert!(!v.errors.is_empty(), "this should be refused");
            assert_eq!(p.apply(&t, "CG-4004", None, REASON).is_err(), true);
            v.errors.join(" | ")
        };
        assert!(bad(&|t| t["max_unit_price"]["SVC-NOPE"] = json!(5)).contains("not a service in the catalogue"));
        assert!(bad(&|t| t["max_unit_price"]["SVC-LAB"] = json!(-1)).contains("max_unit_price.SVC-LAB"));
        assert!(bad(&|t| t["max_quantity_per_line"]["SVC-LAB"] = json!(0)).contains("max_quantity_per_line"));
        assert!(bad(&|t| t["allowed_providers"] = json!([])).contains("at least one provider"));
        assert!(bad(&|t| t["allowed_providers"] = json!(["EDU-PROV-01", "EDU-PROV-01"])).contains("listed twice"));
        assert!(bad(&|t| t["currency"] = json!("sar")).contains("three capital letters"));
        assert!(bad(&|t| t["submission_window_days"] = json!(-3)).contains("submission_window_days"));
        assert!(bad(&|t| t["required_documents"]["SVC-IMAGE"] = json!("Imaging Report!")).contains("document type"));
        assert!(bad(&|t| t["auth_required_services"] = json!(["SVC-GHOST"])).contains("not a service in the catalogue"));
        assert!(!p.validate(&json!([1, 2])).errors.is_empty());
        assert!(!p.validate(&json!({})).errors.is_empty());
        assert!(p.apply(&good, "CG-4004", None, REASON).is_err(), "no change: refused");
        let mut t = good.clone();
        t[&pid]["max_unit_price"]["SVC-LAB"] = json!(270);
        assert!(p.apply(&t, "CG-4004", None, "short").is_err(), "a reason is required");
        assert_eq!(p.version(), 0, "nothing was written by any refusal");
        let mut removed = good.clone();
        removed.as_object_mut().unwrap().remove(&pid);
        removed[format!("{pid}-B")] = good[&pid].clone();
        let v = p.validate(&removed);
        assert!(v.warnings.iter().any(|w| w.contains("is removed")), "{:?}", v.warnings);
    }

    #[test]
    fn a_second_process_sees_the_change_and_a_restart_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let (_, writer) = open(dir.path());
        let (e2, reader) = open(dir.path());
        let mut table = writer.current()["policies"].clone();
        let pid = table.as_object().unwrap().keys().next().unwrap().clone();
        table[&pid]["max_unit_price"]["SVC-LAB"] = json!(333);
        writer.apply(&table, "CG-4004", None, REASON).unwrap();
        assert_eq!(reader.version(), 0, "the other process has not looked yet");
        assert_eq!(reader.refresh(), Ok(true));
        assert_eq!(reader.version(), 1);
        assert_eq!(reader.refresh(), Ok(false), "and a second look finds nothing new");
        assert_ne!(e2.rulebook_hash(), e2.pack.hash());
        let (e3, again) = open(dir.path());
        assert_eq!(again.version(), 1, "a restart keeps the table");
        assert_eq!(e3.rulebook_hash(), e2.rulebook_hash());
        assert!(cg_audit::verify(&dir.path().join("payer_rules.jsonl")).is_ok(), "the history is a verifying hash chain");
    }

    // ---- meaning-based "Ask the rulebook" (docs/52) ----------------------------------------------------------------------------

    /// A stand-in embedder: hashes words into 128 buckets. Not meaningful, but deterministic, so the plumbing (modes, threshold, cache, fallback) can be tested.
    struct Stub {
        calls: std::sync::atomic::AtomicUsize,
        texts: std::sync::atomic::AtomicUsize,
        down: std::sync::atomic::AtomicBool,
    }

    impl Stub {
        fn new() -> Arc<Stub> {
            Arc::new(Stub { calls: Default::default(), texts: Default::default(), down: Default::default() })
        }
    }

    impl crate::rag::Embedder for Stub {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            use std::sync::atomic::Ordering::SeqCst;
            self.calls.fetch_add(1, SeqCst);
            if self.down.load(SeqCst) {
                return Err("connection refused".into());
            }
            self.texts.fetch_add(texts.len(), SeqCst);
            Ok(texts.iter().map(|t| {
                let mut v = vec![0f32; 128];
                for w in crate::rag::tokens(t) {
                    v[w.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % 128] += 1.0;
                }
                v
            }).collect())
        }
    }

    #[test]
    fn without_an_embedder_the_answer_is_the_keyword_search() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let r = p.ask("highest unit price for SVC-LAB", 4);
        assert_eq!(r["mode"], "keyword");
        assert!(r.get("fallback").is_none() && r.get("caveat").is_none());
        assert_eq!(r["answered"], true);
    }

    #[test]
    fn with_an_embedder_the_answer_is_semantic_and_carries_the_caveat() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        p.set_embedder(Stub::new(), 0.2);
        let r = p.ask("highest unit price for SVC-LAB", 4);
        assert_eq!(r["mode"], "semantic");
        assert_eq!(r["answered"], true);
        assert!(r["caveat"].as_str().unwrap().contains("may not answer"));
        assert!(r["results"].as_array().unwrap().len() <= 4);
        assert!(r["results"][0]["score"].as_f64().unwrap() >= 0.2, "{r}");
    }

    #[test]
    fn below_the_threshold_it_refuses_and_a_blank_question_is_not_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let stub = Stub::new();
        p.set_embedder(stub.clone(), 0.99);
        let r = p.ask("zzqx plugh xyzzy", 4);
        assert_eq!(r["mode"], "semantic");
        assert_eq!(r["answered"], false);
        assert_eq!(r["results"].as_array().unwrap().len(), 0);
        let calls = stub.calls.load(std::sync::atomic::Ordering::SeqCst);
        let r = p.ask("   ", 4);
        assert_eq!(r["answered"], false);
        assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), calls, "a blank question never reaches the embedder");
    }

    #[test]
    fn a_dead_embedder_falls_back_to_keywords_and_is_not_hammered() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let stub = Stub::new();
        stub.down.store(true, SeqCst);
        p.set_embedder(stub.clone(), 0.2);
        let r = p.ask("highest unit price for SVC-LAB", 4);
        assert_eq!(r["mode"], "keyword");
        assert_eq!(r["answered"], true, "the keyword search still answers");
        assert!(r["fallback"].as_str().unwrap().contains("keyword search"));
        let after_first = stub.calls.load(SeqCst);
        for _ in 0..5 {
            assert_eq!(p.ask("highest unit price for SVC-LAB", 4)["mode"], "keyword");
        }
        assert_eq!(stub.calls.load(SeqCst), after_first, "within the back-off the sidecar is left alone");
    }

    #[test]
    fn a_policy_change_re_embeds_the_policy_passages_but_not_the_rules() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let stub = Stub::new();
        p.set_embedder(stub.clone(), 0.2);
        p.ask("highest unit price for SVC-LAB", 4);
        let first = stub.texts.load(SeqCst);
        assert!(first > 60, "the first question embeds the whole corpus ({first})");
        p.ask("submission window", 4);
        assert_eq!(stub.texts.load(SeqCst), first + 1, "the second question embeds only the question");
        let mut pol = p.current()["policies"].clone();
        pol["EDU-BASIC"]["max_unit_price"]["SVC-LAB"] = json!(417);
        p.apply(&pol, "CG-3001", None, "raise the laboratory ceiling after the amendment").unwrap();
        let before = stub.texts.load(SeqCst);
        let r = p.ask("highest unit price for SVC-LAB", 4);
        let used = stub.texts.load(SeqCst) - before;
        // a policy passage names its table version, so the new version's passages are new texts; the rule passages are unchanged and come from the cache
        assert!(used > 1 && used < first, "the new version's policy passages (and the question) are embedded, the rules are not: {used} of {first}");
        assert_eq!(r["mode"], "semantic");
    }
}
