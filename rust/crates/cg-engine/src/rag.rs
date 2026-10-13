//! "Ask the rulebook": retrieval with citations over the text of the fifteen rules and over the payer policy table AS IT IS NOW (so an approved change is what
//! people read about, and each policy passage names the table version).
//!
//! What it is and is not. It is retrieval: a question goes in, the passages of the rulebook and the policies that best match come out, each with the rule or
//! policy it came from, ranked by BM25 (lexical, no model, no network, deterministic). Nothing is generated, so nothing can be invented: the answer IS the
//! quoted passage and its citation. A question that matches nothing is refused ("not in the rulebook documents") instead of answered with the nearest guess.
//! It never sees a claim: the corpus is the rulebook and the policy table only. A dense (embedding) retriever and a model that phrases an answer are possible
//! later; they would have to beat this on `tests` below before replacing it.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const STOP: &[&str] = &["a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "how", "if", "in", "is", "it", "of", "on", "or", "that", "the", "to", "what", "when", "which", "who", "with", "must", "should", "this", "there", "than", "then"];
const K1: f64 = 1.5;
const B: f64 = 0.75;
/// A hit must score at least this much, or the question is refused (a single weak word shared with a passage is not an answer).
const MIN_SCORE: f64 = 1.2;

#[derive(Debug, Clone)]
pub struct Passage {
    pub source: String,
    pub kind: String,
    pub text: String,
    tokens: Vec<String>,
}

pub struct Index {
    passages: Vec<Passage>,
    df: HashMap<String, usize>,
    avg_len: f64,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub source: String,
    pub kind: String,
    pub text: String,
    pub score: f64,
}

fn stem(w: &str) -> String {
    let w = w.to_lowercase();
    for suf in ["ing", "ed", "es", "s"] {
        if w.len() > suf.len() + 3 && w.ends_with(suf) {
            return w[..w.len() - suf.len()].to_string();
        }
    }
    w
}

pub fn tokens(text: &str) -> Vec<String> {
    let mut out = vec![];
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
        let raw = raw.trim_matches(|c| c == '-' || c == '_');
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_lowercase();
        if lower.contains('-') || lower.contains('_') {
            out.push(lower.clone());                 // R010, svc-lab, invoice_number stay whole ...
            out.extend(lower.split(|c| c == '-' || c == '_').filter(|p| !p.is_empty() && !STOP.contains(p)).map(stem)); // ... and their parts are searchable too
        } else if !STOP.contains(&lower.as_str()) {
            out.push(stem(&lower));
        }
    }
    out
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for part in text.split_inclusive(". ") {
        cur.push_str(part);
        if cur.chars().count() >= 60 {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn passage(source: &str, kind: &str, text: String) -> Passage {
    let tokens = tokens(&format!("{source} {text}"));
    Passage { source: source.into(), kind: kind.into(), text, tokens }
}

fn num(v: &Value) -> String {
    v.as_f64().map(|f| if f.fract() == 0.0 { format!("{}", f as i64) } else { format!("{f}") }).unwrap_or_else(|| v.to_string())
}

impl Index {
    /// `rules_json`: rules/rules.json. `policies`: the payer table in force, at `version`.
    pub fn build(rules_json: &Value, policies: &Value, version: u64) -> Index {
        let mut ps = vec![];
        for r in rules_json.as_array().into_iter().flatten() {
            let id = r["rule_id"].as_str().unwrap_or("");
            let title = r["title"].as_str().unwrap_or("");
            let src = format!("rule {id}");
            ps.push(passage(&src, "title", format!("{id} {title}. Severity {}.", r["severity"].as_str().unwrap_or(""))));
            for s in sentences(r["logic"].as_str().unwrap_or("")) {
                ps.push(passage(&src, "what it checks", format!("{id} {title}: {s}")));
            }
            if let Some(c) = r["corrective_action"].as_str().filter(|c| !c.is_empty()) {
                ps.push(passage(&src, "what to do", format!("{id} {title}: {c}")));
            }
        }
        for (id, p) in policies.as_object().into_iter().flatten() {
            let src = format!("policy {id} (table version {version})");
            let payer = p["payer_id"].as_str().unwrap_or("");
            ps.push(passage(&src, "policy", format!("Policy {id} of payer {payer}: claims are in {} and must be submitted within {} days of the latest service.", p["currency"].as_str().unwrap_or(""), p["submission_window_days"])));
            let providers: Vec<&str> = p["allowed_providers"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            ps.push(passage(&src, "policy", format!("Policy {id}: the allowed providers (the network) are {}.", providers.join(", "))));
            let auth: Vec<&str> = p["auth_required_services"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            ps.push(passage(&src, "policy", format!("Policy {id}: services that need an authorization reference: {}.", auth.join(", "))));
            for (svc, t) in p["required_documents"].as_object().into_iter().flatten() {
                ps.push(passage(&src, "policy", format!("Policy {id}: service {svc} requires a supporting document of type {}.", t.as_str().unwrap_or(""))));
            }
            for (svc, n) in p["max_unit_price"].as_object().into_iter().flatten() {
                ps.push(passage(&src, "policy", format!("Policy {id}: the highest unit price for {svc} is {} {}.", num(n), p["currency"].as_str().unwrap_or(""))));
            }
            for (svc, n) in p["max_quantity_per_line"].as_object().into_iter().flatten() {
                ps.push(passage(&src, "policy", format!("Policy {id}: the highest quantity on one line for {svc} is {}.", num(n))));
            }
        }
        let mut df: HashMap<String, usize> = HashMap::new();
        for p in &ps {
            let mut seen: Vec<&String> = p.tokens.iter().collect();
            seen.sort();
            seen.dedup();
            for t in seen {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let avg_len = if ps.is_empty() { 1.0 } else { ps.iter().map(|p| p.tokens.len()).sum::<usize>() as f64 / ps.len() as f64 };
        Index { passages: ps, df, avg_len }
    }

    pub fn len(&self) -> usize {
        self.passages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.passages.is_empty()
    }

    /// The best passages for a question (at most `k`, best first, none below the threshold), one per (source, kind) so the answer cites different places.
    pub fn search(&self, question: &str, k: usize) -> Vec<Hit> {
        let q = tokens(question);
        if q.is_empty() {
            return vec![];
        }
        let n = self.passages.len() as f64;
        let mut scored: Vec<(f64, usize)> = vec![];
        for (i, p) in self.passages.iter().enumerate() {
            let mut tf: BTreeMap<&String, usize> = BTreeMap::new();
            for t in &p.tokens {
                *tf.entry(t).or_insert(0) += 1;
            }
            let mut score = 0.0;
            let mut seen_q: Vec<&String> = vec![];
            let mut matched = 0;
            for t in &q {
                if seen_q.contains(&t) {
                    continue;
                }
                seen_q.push(t);
                let Some(f) = tf.get(t) else { continue };
                matched += 1;
                let df = *self.df.get(t).unwrap_or(&1) as f64;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                let f = *f as f64;
                score += idf * f * (K1 + 1.0) / (f + K1 * (1.0 - B + B * p.tokens.len() as f64 / self.avg_len));
            }
            // one shared word is not an answer: a question of two or more distinct terms must match at least two of them
            if score >= MIN_SCORE && matched >= seen_q.len().min(2) {
                scored.push((score, i));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        let mut out: Vec<Hit> = vec![];
        for (score, i) in scored {
            let p = &self.passages[i];
            if out.iter().any(|h| h.source == p.source && h.kind == p.kind) {
                continue;
            }
            out.push(Hit { source: p.source.clone(), kind: p.kind.clone(), text: p.text.clone(), score });
            if out.len() >= k {
                break;
            }
        }
        out
    }
}

pub fn hits_json(question: &str, hits: &[Hit]) -> Value {
    json!({"question": question, "answered": !hits.is_empty(),
           "note": if hits.is_empty() { "Not in the rulebook documents: nothing in the fifteen rules or the payer policies matches this question." } else { "Quoted from the rulebook and the policies in force; nothing here is generated." },
           "results": hits.iter().map(|h| json!({"source": h.source, "kind": h.kind, "text": h.text, "score": (h.score * 100.0).round() / 100.0})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn index() -> Index {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let rules: Value = serde_json::from_str(&std::fs::read_to_string(root.join("rules/rules.json")).unwrap()).unwrap();
        let policies: Value = serde_json::from_str(&std::fs::read_to_string(root.join("rules/policies.json")).unwrap()).unwrap();
        Index::build(&rules, &policies, 0)
    }

    /// (question, sources of which at least one must be in the top three). Written from what a reviewer would ask; not tuned against the index.
    const QUESTIONS: &[(&str, &[&str])] = &[
        ("what happens if the invoice number is missing", &["rule R001"]),
        ("a missing diagnosis code", &["rule R001"]),
        ("claim line without a service date", &["rule R001"]),
        ("service date after the submission date", &["rule R002"]),
        ("is the coverage active on the date of service", &["rule R003"]),
        ("the coverage ended before the service", &["rule R003"]),
        ("patient id does not match the beneficiary on the coverage", &["rule R004"]),
        ("member id must match exactly, case sensitive", &["rule R004"]),
        ("provider is not in the network", &["rule R005"]),
        ("which providers are allowed for EDU-BASIC", &["policy EDU-BASIC (table version 0)", "rule R005"]),
        ("two identical lines on the same day", &["rule R006"]),
        ("what should I do about a possible duplicate", &["rule R006"]),
        ("quantity times unit price does not equal the net amount", &["rule R007"]),
        ("how is the amount rounded", &["rule R007", "rule R012"]),
        ("which services need an authorization reference", &["rule R008", "policy EDU-BASIC (table version 0)", "policy EDU-PLUS (table version 0)"]),
        ("authorization is for another patient", &["rule R009"]),
        ("authorization status approved and its valid dates", &["rule R009"]),
        ("what supporting document is required for imaging", &["rule R010", "policy EDU-BASIC (table version 0)", "policy EDU-PLUS (table version 0)"]),
        ("the attached document is only a draft", &["rule R010"]),
        ("unknown service code", &["rule R011"]),
        ("total amount does not equal the sum of the lines", &["rule R012"]),
        ("highest unit price for SVC-LAB", &["rule R013", "policy EDU-BASIC (table version 0)", "policy EDU-PLUS (table version 0)"]),
        ("quantity must be a positive integer", &["rule R013"]),
        ("how many days after the service can a claim be submitted", &["rule R014", "policy EDU-BASIC (table version 0)", "policy EDU-PLUS (table version 0)"]),
        ("submission window for EDU-PLUS", &["policy EDU-PLUS (table version 0)", "rule R014"]),
        ("claim currency is different from SAR", &["rule R015"]),
    ];

    const OFF_TOPIC: &[&str] = &["what is the weather today", "who won the football match", "recommend a treatment for diabetes", "write me a poem", ""];

    #[test]
    fn retrieval_finds_the_right_rule_or_policy_for_a_reviewers_question() {
        let ix = index();
        assert!(ix.len() > 60);
        let (mut at1, mut at3, mut misses) = (0, 0, vec![]);
        for (q, want) in QUESTIONS {
            let hits = ix.search(q, 3);
            if hits.first().is_some_and(|h| want.contains(&h.source.as_str())) {
                at1 += 1;
            }
            if hits.iter().any(|h| want.contains(&h.source.as_str())) {
                at3 += 1;
            } else {
                misses.push(*q);
            }
        }
        let n = QUESTIONS.len() as f64;
        println!("retrieval over {} questions: hit@1 {:.0}%  hit@3 {:.0}%  misses {:?}", QUESTIONS.len(), 100.0 * at1 as f64 / n, 100.0 * at3 as f64 / n, misses);
        assert!(at3 as f64 / n >= 0.92, "hit@3 too low: misses {misses:?}");
        assert!(at1 as f64 / n >= 0.75, "hit@1 too low");
    }

    #[test]
    fn a_question_the_rulebook_cannot_answer_is_refused_not_guessed() {
        let ix = index();
        for q in OFF_TOPIC {
            assert!(ix.search(q, 3).is_empty(), "'{q}' should be refused");
        }
        let v = hits_json("what is the weather today", &[]);
        assert_eq!(v["answered"], false);
        assert!(v["note"].as_str().unwrap().contains("Not in the rulebook"));
    }

    #[test]
    fn every_answer_is_a_quote_with_its_source() {
        let ix = index();
        let hits = ix.search("highest unit price for SVC-LAB", 3);
        assert!(!hits.is_empty());
        for h in &hits {
            assert!(h.source.starts_with("rule R") || h.source.starts_with("policy "), "{}", h.source);
            assert!(!h.text.is_empty());
        }
        assert!(hits.iter().any(|h| h.text.contains("260")), "the policy passage quotes the actual limit: {:?}", hits.iter().map(|h| h.text.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn the_policy_passages_follow_the_table_in_force() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let rules: Value = serde_json::from_str(&std::fs::read_to_string(root.join("rules/rules.json")).unwrap()).unwrap();
        let mut policies: Value = serde_json::from_str(&std::fs::read_to_string(root.join("rules/policies.json")).unwrap()).unwrap();
        policies["EDU-BASIC"]["max_unit_price"]["SVC-LAB"] = json!(417);
        let ix = Index::build(&rules, &policies, 3);
        let hits = ix.search("highest unit price for SVC-LAB", 5);
        assert!(hits.iter().any(|h| h.text.contains("417") && h.source.contains("table version 3")), "{hits:?}");
    }

    #[test]
    fn it_never_sees_a_claim_and_is_deterministic() {
        let ix = index();
        let a = ix.search("duplicate lines", 5);
        let b = ix.search("duplicate lines", 5);
        assert_eq!(a.iter().map(|h| (h.source.clone(), h.kind.clone())).collect::<Vec<_>>(), b.iter().map(|h| (h.source.clone(), h.kind.clone())).collect::<Vec<_>>());
        assert!(ix.passages.iter().all(|p| !p.text.contains("PAT-") && !p.text.contains("CG-")), "the corpus holds no claim data");
    }
}
