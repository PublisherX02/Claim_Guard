//! "Ask the rulebook": retrieval with citations over the text of the fifteen rules and over the payer policy table AS IT IS NOW (so an approved change is what
//! people read about, and each policy passage names the table version).
//!
//! What it is and is not. It is retrieval: a question goes in, the passages of the rulebook and the policies that best match come out, each with the rule or
//! policy it came from. Nothing is generated, so nothing can be invented: the answer IS the quoted passage and its citation. It never sees a claim: the corpus
//! is the rulebook and the policy table only.
//!
//! Two ways to rank, chosen per request by `PayerRules::ask`:
//! * **semantic** (when an embedding sidecar is configured and reachable; docs/52): the cosine of sentence embeddings (`paraphrase-multilingual-MiniLM-L12-v2`),
//!   refusing below a threshold chosen on a held-out half. It understands "the same procedure appears twice" and French or Arabic questions. Its weakness is
//!   a question close to the domain but not answered by it ("how do I appeal a rejected claim"): the nearest passage comes back, so the answer carries a caveat.
//! * **keyword** (BM25: lexical, no model, no network, deterministic): the fallback, and the only mode when no sidecar is configured. It refuses what matches nothing.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const STOP: &[&str] = &["a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "how", "if", "in", "is", "it", "of", "on", "or", "that", "the", "to", "what", "when", "which", "who", "with", "must", "should", "this", "there", "than", "then"];
const K1: f64 = 1.5;
const B: f64 = 0.75;
/// Whether a field passage is embedded together with its Arabic names (experiments/rag_v2: no gain, so off).
const EMBED_ALIASES: bool = false;

/// Other ways of asking about each rule (Arabic, French) and the out-of-scope topics (docs/55). Used to find passages, never quoted.
const PHRASES_TEXT: &str = include_str!("../data/rule_phrases.json");

/// The kind of a passage about something the rulebook does not cover: a question that lands closest to one is refused, and the topic is named.
pub const OUT_OF_SCOPE: &str = "out of scope";

/// A hit must score at least this much, or the question is refused (a single weak word shared with a passage is not an answer).
const MIN_SCORE: f64 = 1.2;

#[derive(Debug, Clone)]
pub struct Passage {
    pub source: String,
    pub kind: String,
    pub text: String,
    /// Other names the passage is asked about (the Arabic names of a field). Used only for the embedding and for BM25 tokens, never quoted.
    aliases: String,
    tokens: Vec<String>,
}

pub struct Index {
    passages: Vec<Passage>,
    df: HashMap<String, usize>,
    avg_len: f64,
    /// passages that are text of the rulebook (all but the out-of-scope ones), for the keyword statistics
    n_text: usize,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub source: String,
    pub kind: String,
    pub text: String,
    pub score: f64,
}

/// Turns texts into vectors of one fixed length (the embedding sidecar, `deploy/embed`). Implemented over HTTP in `cg-cli`; tests use a stub.
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
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
    Passage { source: source.into(), kind: kind.into(), text, aliases: String::new(), tokens }
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
        // Arabic and French ways of asking, attached to the rule's title passage for the embedding only
        let spec: Value = serde_json::from_str(PHRASES_TEXT).expect("rule_phrases.json is valid JSON (a test checks it)");
        for p in ps.iter_mut().filter(|p| p.kind == "title" && p.source.starts_with("rule R")) {
            let id = p.source.trim_start_matches("rule ");
            if let Some(r) = spec["rules"].get(id) {
                let list = |k: &str| r[k].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>();
                p.aliases = format!("{} | {}", list("ar").join("، "), list("fr").join(" ; "));
            }
        }
        // the data dictionary (docs/55): what each claim field means, in English, found by its Arabic names too
        for f in cg_core::localize::field_entries() {
            let mut p = passage(&f.source, "field", f.text);
            p.tokens.extend(tokens(&f.aliases));
            p.aliases = f.aliases;
            ps.push(p);
        }
        // out-of-scope topics: found by meaning only (no keyword tokens), they turn a near-domain question into a refusal that names the topic
        for d in spec["decoys"].as_array().into_iter().flatten() {
            let list = |k: &str| d[k].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>();
            let topic = d["topic"].as_str().unwrap_or("");
            ps.push(Passage { source: format!("out of scope: {topic}"), kind: OUT_OF_SCOPE.into(), text: d["text"].as_str().unwrap_or("").to_string(), aliases: format!("{} | {}", list("ar").join("، "), list("fr").join(" ; ")), tokens: vec![] });
        }
        let n_text = ps.iter().filter(|p| p.kind != OUT_OF_SCOPE).count();
        let mut df: HashMap<String, usize> = HashMap::new();
        for p in &ps {
            let mut seen: Vec<&String> = p.tokens.iter().collect();
            seen.sort();
            seen.dedup();
            for t in seen {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let avg_len = if n_text == 0 { 1.0 } else { ps.iter().map(|p| p.tokens.len()).sum::<usize>() as f64 / n_text as f64 };
        Index { passages: ps, df, avg_len, n_text }
    }

    pub fn len(&self) -> usize {
        self.passages.len()
    }

    /// The text embedded for each passage, in index order. The experiment (docs/52) embedded exactly this: the source and kind in front of the passage.
    pub fn embed_texts(&self) -> Vec<String> {
        self.passages.iter().map(|p| if (EMBED_ALIASES || p.kind != "field") && !p.aliases.is_empty() { format!("{} ({}): {} Also asked as: {}", p.source, p.kind, p.text, p.aliases) } else { format!("{} ({}): {}", p.source, p.kind, p.text) }).collect()
    }

    /// Semantic search: the passages whose vectors are closest to the question's, one per (source, kind), best first. Refused (empty) when even the closest is
    /// below `threshold` (a question that is not about the rulebook has no close passage) or when the closest is an out-of-scope topic, which is then named in the
    /// second value. `vectors` are the passage vectors in index order.
    pub fn search_dense_ex(&self, vectors: &[Vec<f32>], question_vec: &[f32], k: usize, threshold: f64) -> (Vec<Hit>, Option<String>) {
        if vectors.len() != self.passages.len() || question_vec.is_empty() || vectors.iter().any(|v| v.len() != question_vec.len()) {
            return (vec![], None);
        }
        let qn = norm(question_vec);
        if qn == 0.0 {
            return (vec![], None);
        }
        let mut scored: Vec<(f64, usize)> = vectors.iter().enumerate().map(|(i, v)| (dot(v, question_vec) / (norm(v).max(1e-12) * qn), i)).collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        let Some(top) = scored.first().copied() else { return (vec![], None) };
        if self.passages[top.1].kind == OUT_OF_SCOPE {
            return (vec![], Some(self.passages[top.1].source.trim_start_matches("out of scope: ").to_string()));
        }
        if top.0 < threshold {
            return (vec![], None);
        }
        let mut out: Vec<Hit> = vec![];
        for (score, i) in scored {
            let p = &self.passages[i];
            if p.kind == OUT_OF_SCOPE || out.iter().any(|h| h.source == p.source && h.kind == p.kind) {
                continue;
            }
            out.push(Hit { source: p.source.clone(), kind: p.kind.clone(), text: p.text.clone(), score });
            if out.len() >= k {
                break;
            }
        }
        (out, None)
    }

    pub fn search_dense(&self, vectors: &[Vec<f32>], question_vec: &[f32], k: usize, threshold: f64) -> Vec<Hit> {
        self.search_dense_ex(vectors, question_vec, k, threshold).0
    }

    /// The field passages an Arabic question names (`cg_core::localize::fields_named`): a dictionary lookup that does not depend on the embedding.
    pub fn named_fields(&self, question: &str) -> Vec<Hit> {
        cg_core::localize::fields_named(question)
            .iter()
            .filter_map(|src| self.passages.iter().find(|p| p.kind == "field" && &p.source == src))
            .map(|p| Hit { source: p.source.clone(), kind: p.kind.clone(), text: p.text.clone(), score: 1.0 })
            .collect()
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
        let n = self.n_text as f64;
        let mut scored: Vec<(f64, usize)> = vec![];
        let asked = question.to_lowercase();
        let asked_spaced = asked.replace('_', " ");
        for (i, p) in self.passages.iter().enumerate() {
            // keyword search reads a field passage only when the question asks what a field is or means: otherwise the many shared words
            // ("service date", "unit price") would let the dictionary push the rule that answers the question out of first place
            if p.kind == OUT_OF_SCOPE {
                continue;
            }
            if p.kind == "field" {
                let key = p.source.rsplit(['.', ' ']).next().unwrap_or("");
                let defining = ["field", "mean", "used for", "stands for", "defin"].iter().any(|w| asked.contains(w));
                if !(defining && (asked.contains(key) || asked_spaced.contains(&key.replace('_', " ")) || asked.contains("field"))) {
                    continue;
                }
            }
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

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum()
}

fn norm(a: &[f32]) -> f64 {
    dot(a, a).sqrt()
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
            assert!(h.source.starts_with("rule R") || h.source.starts_with("policy ") || h.source.starts_with("field "), "{}", h.source);
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

    #[test]
    fn a_question_closest_to_an_out_of_scope_topic_is_refused_and_the_topic_is_named() {
        let ix = index();
        let n = ix.passages.len();
        // every passage gets its own axis, so the closest passage to a question is the one it is built from
        let vectors: Vec<Vec<f32>> = (0..n).map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect()).collect();
        let at = |i: usize| -> Vec<f32> { (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect() };
        let decoy = ix.passages.iter().position(|p| p.kind == OUT_OF_SCOPE).expect("there are out-of-scope topics");
        let (hits, topic) = ix.search_dense_ex(&vectors, &at(decoy), 4, 0.1);
        assert!(hits.is_empty() && topic.as_deref().is_some_and(|t| !t.is_empty()), "{topic:?}");
        let rule = ix.passages.iter().position(|p| p.source == "rule R007").unwrap();
        let (hits, topic) = ix.search_dense_ex(&vectors, &at(rule), 4, 0.1);
        assert!(topic.is_none() && hits[0].source == "rule R007" && hits.iter().all(|h| h.kind != OUT_OF_SCOPE));
        // vectors of the wrong length are refused, never silently truncated
        assert!(ix.search_dense_ex(&vectors, &[1.0, 0.0], 4, 0.1).0.is_empty());
    }

    #[test]
    fn every_rule_has_other_ways_of_asking_and_decoys_are_never_keyword_hits() {
        let spec: Value = serde_json::from_str(PHRASES_TEXT).unwrap();
        for id in (1..=15).map(|n| format!("R{n:03}")) {
            assert!(spec["rules"][&id]["ar"].as_array().is_some_and(|a| !a.is_empty()) && spec["rules"][&id]["fr"].as_array().is_some_and(|a| !a.is_empty()), "{id}");
        }
        assert!(spec["decoys"].as_array().unwrap().len() >= 10);
        let ix = index();
        assert!(ix.search("appeal a rejected claim deductible premium", 5).iter().all(|h| h.kind != OUT_OF_SCOPE));
        let texts = ix.embed_texts();
        assert!(texts.iter().any(|t| t.starts_with("rule R001 (title)") && t.contains("Also asked as")));
    }
}
