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

use crate::cag::AnswerCache;
use crate::kag::{check_generated, Fact, Graph};
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
    providers_json: Value,
    diagnoses_json: Value,
    /// the knowledge graph (docs/60), rebuilt when the table version changes
    graph: Mutex<Option<(u64, Arc<Graph>)>>,
    /// answers already given (docs/60): the fast path
    answers: AnswerCache,
    /// a language model for the questions that need one: proposing a graph query, and writing up an answer from its facts. Off unless `set_llm` is called.
    llm: Mutex<Option<Llm>>,
    /// meaning-based search (docs/52): the embedder, its refusal threshold, and the passage vectors per table version. Off unless `set_embedder` is called.
    dense: Mutex<Dense>,
}

/// A language model behind a plain text interface (the OpenAI-compatible endpoint of vLLM or Ollama, wired in `cg-cli`). Tests use a stub.
pub trait Completer: Send + Sync {
    fn complete(&self, prompt: &str, timeout_secs: f64) -> Result<String, String>;
    fn model(&self) -> String;
}

/// What the model made of a question (docs/68).
enum Rewrite {
    /// the question in the form the search understands
    Question(String),
    /// the model says the question is not about this rulebook
    OutOfScope,
    /// the model's reply was empty or unusable: ask the question as it was
    None,
    /// the model could not be reached: ask the question as it was, and do not remember the answer
    Unavailable,
}

#[derive(Clone)]
struct Llm {
    c: Arc<dyn Completer>,
    /// put the whole rulebook first in the prompt, so a server that caches prompt prefixes computes it once (docs/60)
    prefix: bool,
    timeout: f64,
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
        let providers_json = read_json(&root.join("rules/providers.json")).unwrap_or_else(|_| json!([]));
        let diagnoses_json = read_json(&root.join("rules/diagnoses.json")).unwrap_or_else(|_| json!([]));
        let p = PayerRules { engine, log_path: data_dir.join("payer_rules.jsonl"), shipped: shipped.clone(), services, rules_json, services_json, providers_json, diagnoses_json, graph: Mutex::new(None), answers: AnswerCache::new(512, std::time::Duration::from_secs(24 * 3600)), llm: Mutex::new(None), state: Mutex::new(State { version: 0, policies: shipped, rows_seen: 0 }), index: Mutex::new(None), dense: Mutex::new(Dense::default()) };
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
        let mut topic = None;
        let (hits, mode, fallback) = match self.ask_semantic(&ix, question, k) {
            Some(Ok((hits, t))) => {
                topic = t;
                (hits, "semantic", None)
            }
            Some(Err(why)) => (ix.search(question, k), "keyword", Some(why)),
            None => (ix.search(question, k), "keyword", None),
        };
        let mut out = crate::rag::hits_json(question, &hits);
        out["table_version"] = json!(version);
        out["mode"] = json!(mode);
        if let Some(t) = topic {
            out["out_of_scope"] = json!(t);
            out["note"] = json!(format!("This looks like a question about \"{t}\", which the rulebook does not cover: nothing in the fifteen rules, the payer policies or the field dictionary answers it."));
        }
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

    /// Turn on the language model for "Ask the rulebook" (docs/60). `rulebook_prefix`: put the whole rulebook first in every prompt.
    pub fn set_llm(&self, c: Arc<dyn Completer>, rulebook_prefix: bool, timeout_secs: f64) {
        *self.llm.lock().unwrap_or_else(|e| e.into_inner()) = Some(Llm { c, prefix: rulebook_prefix, timeout: timeout_secs.clamp(1.0, 600.0) });
        self.answers.clear();
    }

    pub fn llm_model(&self) -> Option<String> {
        self.llm.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|l| l.c.model())
    }

    /// (entries, hits, misses) of the answer cache.
    pub fn cache_stats(&self) -> (usize, u64, u64) {
        self.answers.stats()
    }

    /// The knowledge graph for the table in force.
    pub fn graph(&self) -> Arc<Graph> {
        let (version, policies) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.version, st.policies.clone())
        };
        self.graph_for(version, &policies)
    }

    fn graph_for(&self, version: u64, policies: &Value) -> Arc<Graph> {
        let mut g = self.graph.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some((v, gr)) if *v == version => gr.clone(),
            _ => {
                let gr = Arc::new(Graph::build(&self.rules_json, policies, &self.services_json, &self.providers_json, &self.diagnoses_json, cg_core::localize::glossary_json(), version));
                *g = Some((version, gr.clone()));
                gr
            }
        }
    }

    /// The whole rulebook as one stable text (the front of a prompt).
    pub fn rulebook_context(&self) -> String {
        let (version, policies) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.version, st.policies.clone())
        };
        crate::cag::rulebook_context(&self.rules_json, &policies, &self.services_json, cg_core::localize::glossary_json(), version)
    }

    /// Turn on meaning-based search for "Ask the rulebook". `threshold` is the lowest cosine at which the closest passage still counts as an answer.
    pub fn set_embedder(&self, embedder: Arc<dyn crate::rag::Embedder>, threshold: f64) {
        let mut d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
        d.cfg = Some((embedder, threshold));
        d.failed_at = None;
    }

    /// None: meaning-based search is not configured. Some(Err): configured but unreachable (the caller falls back to keywords). Some(Ok): its answer.
    fn ask_semantic(&self, ix: &crate::rag::Index, question: &str, k: usize) -> Option<Result<(Vec<crate::rag::Hit>, Option<String>), String>> {
        use sha2::{Digest, Sha256};
        let (embedder, threshold) = {
            let d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
            let (e, t) = d.cfg.clone()?;
            if d.failed_at.is_some_and(|t| t.elapsed().as_secs() < EMBEDDER_BACKOFF_SECS) {
                return Some(Err("it failed a moment ago".into()));
            }
            (e, t)
        };
        let result = (|| -> Result<(Vec<crate::rag::Hit>, Option<String>), String> {
            if question.trim().is_empty() {
                return Ok((vec![], None));
            }
            let texts = ix.embed_texts();
            let keys: Vec<String> = texts.iter().map(|t| hex::encode(Sha256::digest(t.as_bytes()))).collect();
            let missing: Vec<usize> = {
                let d = self.dense.lock().unwrap_or_else(|e| e.into_inner());
                (0..texts.len()).filter(|i| !d.cache.contains_key(&keys[*i])).collect()
            };
            for chunk in missing.chunks(16) {
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
            let (dense, topic) = ix.search_dense_ex(&vectors, &q, k, threshold);
            for h in dense {
                if !hits.iter().any(|x| x.source == h.source && x.kind == h.kind) {
                    hits.push(h);
                }
            }
            hits.truncate(k);
            let topic = if hits.is_empty() { topic } else { None };
            Ok((hits, topic))
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

/// Questions worth a model's help in turning into a graph query: they name something of the rulebook, or are not written in English.
fn domain_hint(q: &str) -> bool {
    let l = q.to_lowercase();
    cg_core::localize::has_arabic_script(q)
        || ["rule", "polic", "service", "provider", "authoriz", "authoris", "network", "price", "quantity", "document", "window", "deadline", "currency", "severity", "field", "svc-", "edu-", "règle", "regle", "politique", "autorisation", "prestataire", "délai", "delai", "plafond"].iter().any(|w| l.contains(w))
}

fn first_json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

impl PayerRules {
    fn plan_with_model(&self, graph: &Graph, llm: &Llm, question: &str) -> Option<crate::kag::Query> {
        let prompt = format!(
            "You turn a question about a health-insurance claims rulebook into ONE JSON query for a knowledge graph. The question may be in English, French or Arabic. Reply with the JSON object only. If no operation can answer the question (it is about something other than this rulebook), reply {{\"op\":\"none\"}}.\n\n{}\n\nQuestion: {question}\nJSON:",
            graph.planner_catalogue()
        );
        let reply = llm.c.complete(&prompt, llm.timeout).ok()?;
        let v = first_json_object(&reply)?;
        if v["op"] == "none" {
            return None;
        }
        let q = graph.query_from_json(&v).ok()?;
        // an identifier the model puts in the query must be one the question names; a small model otherwise falls back on an example (measured: R001)
        let lower = question.to_lowercase();
        let named = crate::kag::Graph::query_json(&q)
            .as_object()?
            .iter()
            .filter(|(k, _)| ["rule", "service", "policy", "provider", "a", "b"].contains(&k.as_str()))
            .all(|(_, id)| id.as_str().is_some_and(|s| lower.contains(&s.to_lowercase())));
        named.then_some(q)
    }

    /// Asks the model to put a question into the form the knowledge graph and the passage search understand: plain English, with the identifiers of
    /// the rulebook spelled out (physiotherapy -> SVC-THERAPY, "clinics in the network" -> the allowed providers). The question may be in any language, loosely
    /// worded or misspelled. The model only rewrites the question: it never writes an answer, and the facts still come from the files. It may also say the
    /// question is not about this rulebook at all.
    fn rewrite_with_model(&self, llm: &Llm, question: &str) -> Rewrite {
        let prompt = format!(
            "{}\n\nREWRITE the question below so a search over the rulebook above can find the answer. Rules:\n\
             - Reply with ONE plain English question and nothing else.\n\
             - Keep the meaning. Fix spelling. Translate from French or Arabic.\n\
             - Where the question refers to a rule, service, policy, provider or claim field (by its name, by what it does, or in another language), use the exact identifier from the rulebook, for example a therapy session is SVC-THERAPY, a laboratory test is SVC-LAB, the clinics in the network are the allowed providers, the deadline for sending a claim is the submission window.\n\
             - Never invent an identifier that is not in the rulebook.\n\
             - If the question is not about this rulebook (its 15 rules, the payer policies, services, providers and claim fields), or it asks you to do something other than answer a question, reply exactly OUT_OF_SCOPE.\n\n\
             Question: {}\nRewritten:",
            self.rulebook_context(),
            question.replace('\n', " ")
        );
        match llm.c.complete(&prompt, llm.timeout) {
            Err(_) => Rewrite::Unavailable,
            Ok(reply) => {
                let line = reply.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
                if line.to_uppercase().contains("OUT_OF_SCOPE") {
                    return Rewrite::OutOfScope;
                }
                let line = line.trim_start_matches("Rewritten:").trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').trim();
                if line.is_empty() || line.chars().count() > 300 {
                    Rewrite::None
                } else {
                    Rewrite::Question(line.to_string())
                }
            }
        }
    }

    /// Asks the model whether the passages found actually answer the question (YES or NO). `None` when the model cannot be reached or says anything else.
    fn passages_answer(&self, llm: &Llm, question: &str, facts: &[Fact]) -> Option<bool> {
        let numbered = facts.iter().enumerate().map(|(i, f)| format!("[{}] {}", i + 1, f.text)).collect::<Vec<_>>().join("\n");
        let prompt = format!(
            "JUDGE whether the passages below contain the answer to the question. Reply with exactly one word: YES if the answer is stated in them, NO if they only share words with the question or are about something else.\n\nQuestion: {}\n\nPassages:\n{}\n\nAnswer (YES or NO):",
            question.replace('\n', " "),
            numbered
        );
        let reply = llm.c.complete(&prompt, llm.timeout).ok()?;
        let w = reply.trim().to_uppercase();
        if w.starts_with("YES") {
            Some(true)
        } else if w.starts_with("NO") {
            Some(false)
        } else {
            None
        }
    }

    /// The answer to a question the rulebook cannot answer: nothing is guessed and nothing unrelated is shown.
    fn nothing_in_the_rulebook(question: &str, version: u64, why: String, planner: &str, extra: Option<(&str, Value)>) -> Value {
        let mut o = json!({"question": question, "answered": false, "tier": "refused", "planner": planner, "results": [], "table_version": version, "note": why});
        if let Some((k, v)) = extra {
            o[k] = v;
        }
        o
    }

    /// Asks the model to write the facts up as a short answer. The text is returned only if `check_generated` accepts it.
    fn write_up(&self, llm: &Llm, question: &str, facts: &[Fact]) -> Result<String, String> {
        let numbered = facts.iter().enumerate().map(|(i, f)| format!("[{}] {} (source: {})", i + 1, f.text, f.source)).collect::<Vec<_>>().join("\n");
        let prefix = if llm.prefix { format!("{}\n\n", self.rulebook_context()) } else { String::new() };
        let prompt = format!(
            "{prefix}You answer a question about the rulebook using ONLY the numbered facts below.\nRules: write two to five short sentences. End every sentence with the number of the fact it comes from in square brackets, like [2]. Do not state any rule id, service code, amount or number that is not in the facts. If the facts do not answer the question, reply exactly: NOT ANSWERED.\n\nFacts:\n{numbered}\n\nQuestion: {question}\nAnswer:"
        );
        let text = llm.c.complete(&prompt, llm.timeout)?;
        let text = text.trim().to_string();
        if text.to_uppercase().contains("NOT ANSWERED") {
            return Err("the model says the facts do not answer the question".into());
        }
        check_generated(&text, facts)?;
        Ok(text)
    }

    /// "Ask the rulebook" with three tiers (docs/60): a question asked before is answered from the cache; a question about how the rulebook's parts
    /// connect is answered from the knowledge graph (facts computed from the rule, policy and field files, each cited); anything else goes to passage
    /// search as before. With `generate` and a model configured, the facts or passages are also written up as a short answer, which is shown only if
    /// every sentence cites a fact and every identifier and number in it occurs in the facts; the quoted facts are always returned beside it.
    pub fn ask_routed(&self, question: &str, k: usize, generate: bool) -> Value {
        let t0 = std::time::Instant::now();
        let ms = |t: std::time::Instant| (t.elapsed().as_secs_f64() * 1000.0).round() as u64;
        let (version, policies) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.version, st.policies.clone())
        };
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let writer = if generate { llm.clone() } else { None };
        let options = match &writer {
            Some(l) => format!("generate:{}:{}", l.c.model(), l.prefix),
            None => String::new(),
        };
        // a model that rewrites questions changes what is answered, so it is part of the cache key
        let options = match &llm {
            Some(l) => format!("{options}|rewrite:{}", l.c.model()),
            None => options,
        };
        let key = AnswerCache::key(question, &options, version, &self.engine.rulebook_hash());
        if let Some(mut hit) = self.answers.get(&key) {
            hit["cached"] = json!(true);
            hit["latency_ms"] = json!(ms(t0));
            return hit;
        }
        let graph = self.graph_for(version, &policies);
        // a question about an identifier the rulebook does not have (R099, SVC-XYZ, policy GOLD-PLUS) is refused, not answered with something that shares a word
        let unknown = graph.unknown_ids(question);
        if !unknown.is_empty() {
            let mut out = Self::nothing_in_the_rulebook(question, version, format!("Not in the rulebook: {} is not a rule, service, provider or policy in the documents in force.", unknown.join(", ")), "rules", Some(("unknown_ids", json!(unknown))));
            out["cached"] = json!(false);
            out["latency_ms"] = json!(ms(t0));
            self.answers.put(key, out.clone());
            return out;
        }
        let mut timing = Map::new();
        let tp = std::time::Instant::now();
        let mut cacheable = true;
        let mut planner = "rules";
        let mut query = graph.plan(question);
        let mut rewritten: Option<String> = None;
        if query.is_none() {
            if let Some(l) = &llm {
                match self.rewrite_with_model(l, question) {
                    Rewrite::OutOfScope => {
                        let mut out = Self::nothing_in_the_rulebook(question, version, "This is not a question about the rulebook (its rules, the payer policies, services, providers and claim fields), so there is nothing here to answer it from.".into(), "model", None);
                        out["cached"] = json!(false);
                        out["latency_ms"] = json!(ms(t0));
                        self.answers.put(key, out.clone());
                        return out;
                    }
                    Rewrite::Question(r) => {
                        // an identifier the model made up is not trusted: the question is asked as it was
                        if graph.unknown_ids(&r).is_empty() && !r.eq_ignore_ascii_case(question.trim()) {
                            query = graph.plan(&r);
                            if query.is_some() {
                                planner = "model-rewrite";
                            }
                            rewritten = Some(r);
                        }
                    }
                    Rewrite::None => {}
                    Rewrite::Unavailable => cacheable = false,
                }
                if query.is_none() && domain_hint(rewritten.as_deref().unwrap_or(question)) {
                    query = self.plan_with_model(&graph, l, rewritten.as_deref().unwrap_or(question));
                    if query.is_some() {
                        planner = "model";
                    }
                }
            }
        }
        timing.insert("plan".into(), json!(ms(tp)));
        let tq = std::time::Instant::now();
        let mut graph_facts = query.as_ref().map(|q| graph.run(q)).unwrap_or_default();
        // What the model's rewrite is judged against: the question as the person asked it. A rewrite can drift ("penalty for fraud" -> "FAIL outcome of R006")
        // and then be judged against itself. Only a question that is not plain ASCII (French, Arabic) is judged in its English rewrite.
        let judge_q: String = if question.is_ascii() { question.to_string() } else { rewritten.clone().unwrap_or_else(|| question.to_string()) };
        if planner != "rules" && !graph_facts.is_empty() {
            if let Some(l) = &llm {
                if self.passages_answer(l, &judge_q, &graph_facts) == Some(false) {
                    // the facts answer the rewrite, not the question: refuse instead of showing them as the answer
                    query = None;
                    graph_facts.clear();
                }
            }
        }
        let (mut out, facts): (Value, Vec<Fact>) = if let (Some(q), false) = (&query, graph_facts.is_empty()) {
            timing.insert("graph".into(), json!(ms(tq)));
            let mut o = json!({"question": question, "answered": true, "tier": "graph", "planner": planner, "understood_as": Graph::describe(q), "query": Graph::query_json(q),
                "facts": graph_facts.iter().map(Fact::to_json).collect::<Vec<_>>(), "answer": Graph::answer_text(&graph_facts), "table_version": version,
                "note": "Computed from the rulebook files by walking the connections between rules, fields and policies; every line cites where it comes from. Nothing here is generated."});
            if let Some(r) = &rewritten {
                o["rewritten_as"] = json!(r);
            }
            if planner == "model" {
                o["note"] = json!("A language model chose which question to put to the knowledge graph (it cannot change the facts); the facts are computed from the rulebook files and each cites where it comes from.");
            }
            (o, graph_facts)
        } else {
            let mut o = self.ask(rewritten.as_deref().unwrap_or(question), k);
            timing.insert("search".into(), json!(ms(tq)));
            o["question"] = json!(question);
            if let Some(r) = &rewritten {
                o["rewritten_as"] = json!(r);
                o["planner"] = json!("model-rewrite");
            }
            o["tier"] = json!("vector");
            if o.get("fallback").is_some() {
                cacheable = false; // the meaning-based search was down: ask again next time
            }
            let mut return_facts_empty = false;
            let facts: Vec<Fact> = o["results"].as_array().into_iter().flatten().map(|r| Fact::new(r["text"].as_str().unwrap_or(""), r["source"].as_str().unwrap_or(""))).collect();
            // passages that only share words with the question are not an answer: with a model, ask whether they contain it, and show them as "nearest" if not
            if let (Some(l), false, true) = (&llm, facts.is_empty(), o["answered"] == true) {
                if self.passages_answer(l, &judge_q, &facts) == Some(false) {
                    o["nearest"] = o["results"].take();
                    o["results"] = json!([]);
                    o["answered"] = json!(false);
                    o["note"] = json!("Not in the rulebook: the passages closest to this question only share words with it and do not contain an answer. They are listed under \"nearest\" so a person can look.");
                    return_facts_empty = true;
                }
            }
            (o, if return_facts_empty { vec![] } else { facts })
        };
        if let (Some(l), false) = (&writer, facts.is_empty()) {
            let tw = std::time::Instant::now();
            match self.write_up(l, question, &facts) {
                Ok(text) => {
                    out["written"] = json!(text);
                    out["written_by"] = json!(l.c.model());
                    out["note"] = json!(format!("{} The short answer under \"written\" was written by {} from the numbered facts and checked: every sentence cites a fact and every identifier and number in it is in the facts. The quoted facts are the answer of record.", out["note"].as_str().unwrap_or(""), l.c.model()));
                }
                Err(why) => {
                    out["written_refused"] = json!(why);
                    // a refusal by the check is a result; a model that could not be reached is not, and is retried
                    if why.contains("timed out") || why.contains("Transport") || why.contains("HTTP") || why.contains("connect") {
                        cacheable = false;
                    }
                }
            }
            timing.insert("write".into(), json!(ms(tw)));
        }
        out["cached"] = json!(false);
        out["timing_ms"] = Value::Object(timing);
        out["latency_ms"] = json!(ms(t0));
        if cacheable {
            self.answers.put(key, out.clone());
        }
        out
    }
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

    /// A model that answers from a script, and counts how often it is called.
    struct Script {
        plan: Mutex<String>,
        write: Mutex<String>,
        /// what the model replies to "REWRITE ..." (empty: no usable rewrite) and to "JUDGE ..." (YES or NO)
        rewrite: Mutex<String>,
        judge: Mutex<String>,
        calls: std::sync::atomic::AtomicUsize,
        prompts: Mutex<Vec<String>>,
    }

    impl Script {
        fn new(plan: &str, write: &str) -> Arc<Script> {
            Arc::new(Script { plan: Mutex::new(plan.into()), write: Mutex::new(write.into()), rewrite: Mutex::new(String::new()), judge: Mutex::new("YES".into()), calls: Default::default(), prompts: Mutex::new(vec![]) })
        }
    }

    impl Completer for Script {
        fn complete(&self, prompt: &str, _: f64) -> Result<String, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.prompts.lock().unwrap().push(prompt.to_string());
            Ok(if prompt.contains("REWRITE the question") {
                self.rewrite.lock().unwrap().clone()
            } else if prompt.starts_with("JUDGE whether") {
                self.judge.lock().unwrap().clone()
            } else if prompt.contains("JSON:") {
                self.plan.lock().unwrap().clone()
            } else {
                self.write.lock().unwrap().clone()
            })
        }
        fn model(&self) -> String {
            "script".into()
        }
    }

    #[test]
    fn a_connection_question_is_answered_from_the_graph_without_any_model_and_then_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let a = p.ask_routed("Which rules can fire for SVC-IMAGE under EDU-BASIC?", 4, false);
        assert_eq!((a["tier"].as_str(), a["planner"].as_str(), a["cached"].as_bool()), (Some("graph"), Some("rules"), Some(false)));
        let text = a["answer"].as_str().unwrap();
        assert!(text.contains("R008") && text.contains("R009") && text.contains("[policy EDU-BASIC (table version 0)]"), "{text}");
        assert!(a.get("written").is_none(), "nothing is written without a model");
        let b = p.ask_routed("which rules can fire for svc-image under edu-basic", 4, false);
        assert_eq!((b["cached"].as_bool(), b["answer"].clone()), (Some(true), a["answer"].clone()), "the same question in other clothes is served from memory");
        let (entries, hits, _) = p.cache_stats();
        assert_eq!((entries, hits), (1, 1));
        // approving a new table makes the old answer unreachable
        let mut pol = p.current()["policies"].clone();
        pol["EDU-BASIC"]["max_unit_price"]["SVC-IMAGE"] = json!(2500);
        p.apply(&pol, "CG-3001", None, "raise the imaging ceiling after the amendment").unwrap();
        let c = p.ask_routed("which rules can fire for SVC-IMAGE under EDU-BASIC", 4, false);
        assert_eq!(c["cached"], false);
        assert!(c["answer"].as_str().unwrap().contains("2500") && !c["answer"].as_str().unwrap().contains("2200 "), "the answer follows the table in force");
        // a question the graph does not understand goes to passage search (keyword here: no embedder)
        let d = p.ask_routed("what happens if the invoice number is missing", 4, false);
        assert_eq!((d["tier"].as_str(), d["mode"].as_str()), (Some("vector"), Some("keyword")));
        // off-topic questions that share a word with the rulebook ("clinic", "documents") stay with passage search (found by the held-out regression)
        assert_eq!(p.ask_routed("which documents does a patient need to bring to the clinic", 4, false)["tier"], "vector");
    }

    #[test]
    fn a_model_may_pick_the_query_but_only_from_what_exists_and_it_cannot_change_a_fact() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        // a question the built-in planner does not understand, in French
        let q = "Quelles règles s'appliquent à une prestation d'imagerie SVC-IMAGE avec la politique EDU-PLUS ?";
        assert_eq!(p.ask_routed(q, 4, false)["tier"], "graph", "the identifiers alone are enough here");
        let q2 = "Quelle règle surveille le réseau de prestataires ?";
        let s = Script::new(r#"{"op":"rules_consulting","attr":"allowed_providers"}"#, "");
        p.set_llm(s.clone(), false, 5.0);
        let r = p.ask_routed(q2, 4, false);
        assert_eq!((r["tier"].as_str(), r["planner"].as_str()), (Some("graph"), Some("model")), "{r}");
        assert!(r["answer"].as_str().unwrap().contains("R005"));
        // an invented service is refused and the question falls through to passage search
        *s.plan.lock().unwrap() = r#"{"op":"service_rules","service":"SVC-MADE-UP"}"#.into();
        let r = p.ask_routed("Quelle règle surveille les prestataires inventés ?", 4, false);
        assert_eq!(r["tier"], "vector");
        // a real rule the question never names (the model copying an example) is refused too
        *s.plan.lock().unwrap() = r#"{"op":"rule_info","rule":"R001"}"#.into();
        assert_eq!(p.ask_routed("Quelle règle surveille les écarts de dates ?", 4, false)["tier"], "vector");
        // "none" and rubbish do the same
        for (i, bad) in ["{\"op\":\"none\"}", "I think you want rule R005", "{\"op\":"].iter().enumerate() {
            *s.plan.lock().unwrap() = bad.to_string();
            assert_eq!(p.ask_routed(&format!("Quelle règle surveille le réseau numéro {i} ?"), 4, false)["tier"], "vector", "{bad}");
        }
    }

    #[test]
    fn a_question_naming_something_the_rulebook_does_not_have_is_refused_with_or_without_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        for (q, id) in [("what does rule R099 check", "R099"), ("highest unit price for SVC-XYZ", "SVC-XYZ"), ("what is the limit for policy GOLD-PLUS", "GOLD-PLUS"), ("is EDU-PROV-09 in the network", "EDU-PROV-09")] {
            let r = p.ask_routed(q, 4, false);
            assert_eq!((r["answered"].clone(), r["tier"].clone()), (json!(false), json!("refused")), "{q}: {r}");
            assert!(r["note"].as_str().unwrap().contains(id), "{r}");
            assert_eq!(r["results"], json!([]));
        }
        // real identifiers, and an id-shaped word that is not a rule number, are left alone
        for q in ["what does R008 check", "highest unit price for svc-lab", "which rules can fire for SVC-IMAGE under EDU-PLUS", "what does the payer EDU-PAYER allow"] {
            assert_ne!(p.ask_routed(q, 4, false)["tier"], json!("refused"), "{q}");
        }
    }

    #[test]
    fn a_model_rewrites_a_loosely_worded_question_and_everything_it_says_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let s = Script::new("", "");
        p.set_llm(s.clone(), false, 5.0);
        // loose wording that no keyword matches: the rewrite names the service, so the graph answers, from the files
        *s.rewrite.lock().unwrap() = "What is the highest unit price for SVC-LAB?".into();
        let r = p.ask_routed("what is the most I can charge for a blood panel", 4, false);
        assert_eq!((r["tier"].as_str(), r["planner"].as_str()), (Some("graph"), Some("model-rewrite")), "{r}");
        assert_eq!(r["rewritten_as"], "What is the highest unit price for SVC-LAB?");
        assert!(r["answer"].as_str().unwrap().contains("260"), "the number comes from the policy table: {r}");
        // the model says it is not about the rulebook: refused, and passage search is not even tried
        *s.rewrite.lock().unwrap() = "OUT_OF_SCOPE".into();
        let r = p.ask_routed("what is the capital of France", 4, false);
        assert_eq!((r["answered"].clone(), r["tier"].clone(), r["planner"].clone()), (json!(false), json!("refused"), json!("model")), "{r}");
        // an identifier the model made up is not trusted: the question is asked as it was, and the made-up one never reaches the answer
        *s.rewrite.lock().unwrap() = "What is the highest unit price for SVC-MADE-UP?".into();
        let r = p.ask_routed("what is the most I can charge for a nonexistent thing", 4, false);
        assert!(r.get("rewritten_as").is_none() && !r.to_string().contains("SVC-MADE-UP"), "{r}");
        // passages the model says do not contain the answer are not shown as the answer
        *s.rewrite.lock().unwrap() = "which rule checks the age of the patient".into();
        *s.judge.lock().unwrap() = "NO".into();
        let r = p.ask_routed("which rule looks at how old the patient is", 4, false);
        assert_eq!((r["answered"].clone(), r["results"].clone()), (json!(false), json!([])), "{r}");
        assert!(r["nearest"].as_array().is_some_and(|a| !a.is_empty()), "the nearest passages are kept for a person to look at: {r}");
        // a reply that is not YES or NO changes nothing
        *s.judge.lock().unwrap() = "perhaps".into();
        assert_eq!(p.ask_routed("which rule looks at how old the patient is, please", 4, false)["answered"], true);
    }

    #[test]
    fn a_written_answer_is_shown_only_if_it_passes_the_check_and_the_facts_are_always_there() {
        let dir = tempfile::tempdir().unwrap();
        let (_e, p) = open(dir.path());
        let good = "Policy EDU-BASIC requires an authorization reference for SVC-IMAGE [2]. Rule R008 checks that the reference is present [3].";
        let s = Script::new("", good);
        p.set_llm(s.clone(), true, 5.0);
        let q = "which rules can fire for SVC-IMAGE under EDU-BASIC";
        let r = p.ask_routed(q, 4, true);
        let facts = r["facts"].as_array().unwrap().len();
        assert!(facts >= 4, "{facts}");
        let rr = r["written_refused"].clone();
        assert!(r.get("written").is_some(), "good text is shown: {rr}");
        assert_eq!(r["written_by"], "script");
        assert!(r["answer"].as_str().unwrap().contains("R008"), "the cited facts are returned beside it");
        // with the rulebook prefix switched on the prompt starts with the whole rulebook, the same bytes every time
        let prompts = s.prompts.lock().unwrap().clone();
        assert!(prompts[0].starts_with("RULEBOOK (the only source") && prompts[0].contains("== PAYER POLICIES"));
        // generation is part of the cache key: the plain answer is a different entry
        let calls = s.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(p.ask_routed(q, 4, true)["cached"], true);
        assert_eq!(s.calls.load(std::sync::atomic::Ordering::SeqCst), calls, "a cached written answer does not call the model again");
        assert!(p.ask_routed(q, 4, false).get("written").is_none());
        // text that invents a number, cites nothing or says the facts are not enough is refused, not shown, and the facts remain
        for (bad, why) in [
            ("Policy EDU-BASIC caps SVC-IMAGE at 9999 SAR [2].", "9999"),
            ("Rule R008 checks it, and R012 too, [1].", "R012"),
            ("The rule applies to this service in general terms.", "cites no fact"),
            ("NOT ANSWERED", "do not answer"),
        ] {
            *s.write.lock().unwrap() = bad.into();
            p.set_llm(s.clone(), false, 5.0);
            let r = p.ask_routed(q, 4, true);
            assert!(r.get("written").is_none(), "{bad}");
            assert!(r["written_refused"].as_str().unwrap().contains(why), "{bad}: {}", r["written_refused"]);
            assert!(r["answer"].as_str().unwrap().contains("R008"), "the facts are still there");
        }
    }
}
