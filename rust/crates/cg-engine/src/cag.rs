//! The fast path for "Ask the rulebook" (docs/60): a question asked before is answered from memory, and the language model is given the rulebook
//! as a fixed prefix so the server computes it once.
//!
//! * **The answer cache.** Most questions people put to a rulebook are the same few. An answer is kept under a key made of the normalized
//!   question, the options that change the answer, the policy table version and the rulebook hash, so approving a change to the payer rules makes
//!   every older answer unreachable at once (no purge, no stale answer). Only answers that were complete are kept: one that fell back because the
//!   embedding service or the model was down is not, so the next ask retries.
//! * **The rulebook as a prefix.** `rulebook_context` is the whole rulebook (fifteen rules, the policy table in force, the service catalogue,
//!   the field dictionary) as one stable text. Put first in a prompt, it is identical for every question, and a server that caches prompt prefixes
//!   (vLLM does by default, llama.cpp/Ollama keep the last prompt's) computes it once. It is built in a fixed order, so the same rulebook always
//!   gives the same bytes, and its hash is part of the cache key.
//!
//! Nothing here sees a claim.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Lowercase, strip accents-free punctuation, collapse white space: "What does R003 check?" and "what does r003 check" are the same question.
pub fn normalize(question: &str) -> String {
    let lowered: String = question.chars().flat_map(char::to_lowercase).map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '[' || c == ']' { c } else { ' ' }).collect();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches('.').to_string()
}

pub struct AnswerCache {
    inner: Mutex<Inner>,
    capacity: usize,
    ttl: Duration,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, (Instant, Value)>,
    order: VecDeque<String>,
    hits: u64,
    misses: u64,
}

impl AnswerCache {
    pub fn new(capacity: usize, ttl: Duration) -> AnswerCache {
        AnswerCache { inner: Mutex::new(Inner::default()), capacity: capacity.max(1), ttl }
    }

    /// The key: the question, the options that change the answer, and what the answer was computed from.
    pub fn key(question: &str, options: &str, table_version: u64, rulebook_hash: &str) -> String {
        hex::encode(Sha256::digest(format!("{}\u{1f}{options}\u{1f}{table_version}\u{1f}{rulebook_hash}", normalize(question)).as_bytes()))
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = g.map.get(key).filter(|(at, _)| at.elapsed() < self.ttl).map(|(_, v)| v.clone());
        match fresh {
            Some(v) => {
                g.hits += 1;
                Some(v)
            }
            None => {
                g.misses += 1;
                if g.map.remove(key).is_some() {
                    g.order.retain(|k| k != key);
                }
                None
            }
        }
    }

    pub fn put(&self, key: String, answer: Value) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.map.insert(key.clone(), (Instant::now(), answer)).is_none() {
            g.order.push_back(key);
        }
        while g.order.len() > self.capacity {
            if let Some(old) = g.order.pop_front() {
                g.map.remove(&old);
            }
        }
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.map.clear();
        g.order.clear();
    }

    /// (entries, hits, misses)
    pub fn stats(&self) -> (usize, u64, u64) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.map.len(), g.hits, g.misses)
    }
}

/// The whole rulebook as one stable text, for the front of a prompt. Same rulebook, same bytes.
pub fn rulebook_context(rules_json: &Value, policies: &Value, services_json: &Value, glossary: &Value, table_version: u64) -> String {
    let mut out = String::from("RULEBOOK (the only source; quote it, do not add to it)\n\n== RULES ==\n");
    for r in rules_json.as_array().into_iter().flatten() {
        out.push_str(&format!("{} {} [{} severity]: {} Corrective action: {}\n", r["rule_id"].as_str().unwrap_or(""), r["title"].as_str().unwrap_or(""), r["severity"].as_str().unwrap_or(""), r["logic"].as_str().unwrap_or(""), r["corrective_action"].as_str().unwrap_or("")));
    }
    out.push_str(&format!("\n== PAYER POLICIES (table version {table_version}) ==\n"));
    let mut ids: Vec<&String> = policies.as_object().map(|m| m.keys().collect()).unwrap_or_default();
    ids.sort();
    for id in ids {
        let p = &policies[id];
        out.push_str(&format!("{id}: {}\n", compact(p)));
    }
    out.push_str("\n== SERVICE CATALOGUE ==\n");
    if let Some(m) = services_json.as_object() {
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort();
        for k in keys {
            out.push_str(&format!("{k}: {}\n", compact(&m[k])));
        }
    }
    out.push_str("\n== CLAIM FIELDS (and the rules that read them) ==\n");
    for scope in ["claim", "coverage", "line", "authorization", "attachment"] {
        if let Some(m) = glossary["scopes"][scope].as_object() {
            for (k, f) in m {
                let used: Vec<&str> = f["used_by"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                out.push_str(&format!("{scope}.{k}: {} Used by: {}\n", f["meaning"].as_str().unwrap_or(""), if used.is_empty() { "no rule".to_string() } else { used.join(", ") }));
            }
        }
    }
    out
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// A rough token count for a text (about 3.5 characters a token for English, the figure the sizing uses before the real tokenizer is asked).
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() as f64 / 3.5).ceil() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_same_question_in_other_clothes_is_the_same_key() {
        let k = |q: &str| AnswerCache::key(q, "", 1, "h");
        assert_eq!(k("What does R003 check?"), k("  what does r003   check "));
        assert_ne!(k("what does r003 check"), k("what does r004 check"));
        assert_ne!(AnswerCache::key("q", "", 1, "h"), AnswerCache::key("q", "", 2, "h"), "a new table version is a new key");
        assert_ne!(AnswerCache::key("q", "", 1, "h"), AnswerCache::key("q", "", 1, "other"), "a new rulebook is a new key");
        assert_ne!(AnswerCache::key("q", "", 1, "h"), AnswerCache::key("q", "generate", 1, "h"), "options that change the answer are part of the key");
        assert_eq!(normalize("ما هي قاعدة التفويض المسبق؟"), "ما هي قاعدة التفويض المسبق");
    }

    #[test]
    fn answers_are_kept_until_they_expire_and_the_oldest_goes_first() {
        let c = AnswerCache::new(2, Duration::from_millis(60));
        assert!(c.get("a").is_none());
        c.put("a".into(), json!(1));
        c.put("b".into(), json!(2));
        assert_eq!(c.get("a"), Some(json!(1)));
        c.put("c".into(), json!(3));
        assert!(c.get("a").is_none() && c.get("b").is_some() && c.get("c").is_some(), "the oldest entry made room");
        std::thread::sleep(Duration::from_millis(80));
        assert!(c.get("b").is_none(), "an expired answer is not served");
        let (entries, hits, misses) = c.stats();
        assert_eq!((hits, misses >= 3), (3, true));
        assert!(entries <= 1);
        c.put("d".into(), json!(4));
        c.clear();
        assert!(c.get("d").is_none());
    }

    #[test]
    fn the_rulebook_prefix_is_stable_and_follows_the_table() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let read = |p: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(root.join(p)).unwrap()).unwrap() };
        let (rules, pol, svc) = (read("rules/rules.json"), read("rules/policies.json"), read("rules/services.json"));
        let a = rulebook_context(&rules, &pol, &svc, cg_core::localize::glossary_json(), 1);
        assert_eq!(a, rulebook_context(&rules, &pol, &svc, cg_core::localize::glossary_json(), 1), "same rulebook, same bytes");
        assert!(a.contains("R008 Required authorization reference") && a.contains("EDU-BASIC") && a.contains("SVC-IMAGE") && a.contains("coverage.status"));
        let mut changed = pol.clone();
        changed["EDU-BASIC"]["submission_window_days"] = json!(45);
        assert_ne!(a, rulebook_context(&rules, &changed, &svc, cg_core::localize::glossary_json(), 2));
        let t = estimate_tokens(&a);
        assert!(t > 1000 && t < 30000, "the rulebook is a few thousand tokens: {t}");
    }
}
