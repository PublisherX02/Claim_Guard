//! Builds the simulated review database. The claims and their findings are real (data/development: the claim and the engine's expected
//! results); only the people are invented. The 400 claims carry 499 flagged findings, so claims are recycled under new ids to reach a few
//! thousand decisions. Recycled claims repeat their findings: that is stated in the evidence, and it is why this shows a mechanism, not accuracy.

use crate::personas::{default_reviewers, truth, Persona};
use cg_queue::reliability;
use cg_queue::routing_config::RoutingConfig;
use cg_queue::store::QueueStore;
use cg_queue::triage;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Corpus {
    /// (claim, 15 result rows) for every claim that has at least one flagged finding.
    pub items: Vec<(Value, Value)>,
}

pub fn load_corpus(root: &Path) -> Result<Corpus, String> {
    let read = |name: &str| -> Result<Vec<Value>, String> {
        std::fs::read_to_string(root.join("data/development").join(name)).map_err(|e| e.to_string())?.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| e.to_string())).collect()
    };
    let (claims, results) = (read("claims.jsonl")?, read("expected_results.jsonl")?);
    let mut by_claim: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for r in results {
        by_claim.entry(r["claim_id"].as_str().unwrap_or("").to_string()).or_default().push(r);
    }
    let mut items = vec![];
    for c in claims {
        let Some(rows) = by_claim.remove(c["claim_id"].as_str().unwrap_or("")) else { continue };
        let rows = Value::Array(rows);
        if !triage::flagged(&rows).is_empty() && !triage::degraded(&rows) {
            items.push((c, rows));
        }
    }
    if items.is_empty() {
        return Err("no flagged claims in data/development".into());
    }
    Ok(Corpus { items })
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub claims: usize,
    pub decisions: usize,
    pub fingerprint: String,
}

pub struct Lab {
    pub seed: u64,
    pub reviewers: Vec<Persona>,
}

impl Lab {
    pub fn new(seed: u64, with_adversary: bool) -> Lab {
        Lab { seed, reviewers: default_reviewers(with_adversary) }
    }

    /// The simulated answer of one person to one finding: the truth (confirm with probability `truth[rule]`), shifted by their bias, or a coin flip.
    fn answer(&self, rng: &mut ChaCha8Rng, who: &Persona, rule: &str) -> bool {
        if who.adversarial {
            return false;
        }
        if rng.gen_bool(who.noise.clamp(0.0, 1.0)) {
            return rng.gen_bool(0.5);
        }
        rng.gen_bool((truth().get(rule).copied().unwrap_or(0.9) + who.bias).clamp(0.0, 1.0))
    }

    /// Feeds claims until at least `target` decisions exist. Each claim gets a receipt (with `snapshot` applied when given, which is how routing
    /// changes who may take it), then is leased to a simulated reviewer allowed to take it, decided finding by finding, and marked decided.
    pub fn run(&self, store: &dyn QueueStore, corpus: &Corpus, target: usize, cfg: &RoutingConfig, snapshot: Option<&Value>) -> Result<Summary, String> {
        let mut rng = ChaCha8Rng::seed_from_u64(self.seed);
        let (mut decisions, mut claims, mut t) = (0usize, 0usize, 1_000.0f64);
        let mut trail = String::new();
        while decisions < target {
            let (base, results) = &corpus.items[claims % corpus.items.len()];
            let id = format!("SIM-{:06}", claims);
            let mut claim = base.clone();
            claim["claim_id"] = json!(id);
            claim["simulated"] = json!(true);
            let mut receipt = triage::make_receipt(&claim, results, cfg, "sim-pack", "sim-engine", t);
            if let Some(s) = snapshot {
                reliability::apply(&mut receipt, results, s)?;
            }
            let doc = json!({"claim_id": id, "version": 1, "input_hash": triage::input_hash(&claim), "claim": claim, "results": results, "receipt": receipt});
            if !store.put_triaged(&doc).map_err(|e| e.to_string())? {
                return Err(format!("{id} was stored before"));
            }
            for (from, to) in [("triaged", "explanation_skipped"), ("explanation_skipped", "ready")] {
                t += 0.001;
                store.transition(&id, 1, from, to, "system:sim", t, None, None, None).map_err(|e| e.to_string())?.ok_or("transition refused")?;
            }
            let senior_needed = receipt["eligibility"] == "decide_high";
            let pool: Vec<&Persona> = self.reviewers.iter().filter(|p| p.senior || !senior_needed).collect();
            let who = pool[rng.gen_range(0..pool.len())];
            t += 0.001;
            store.lease(&id, 1, &who.badge, t, t + 1800.0).map_err(|e| e.to_string())?.ok_or("lease refused")?;
            for f in triage::flagged(results) {
                let rule = f["rule_id"].as_str().unwrap_or("");
                let upheld = self.answer(&mut rng, who, rule);
                t += 0.001;
                let d = json!({"rule_id": rule, "action": if upheld { "confirm_issue" } else { "dismiss_with_reason" }, "actor": who.badge, "reason": if upheld { "" } else { "simulated dismissal" }, "at": t, "round": 1, "simulated": true});
                store.add_decision(&id, 1, &who.badge, t, &d).map_err(|e| e.to_string())?.ok_or("decision refused")?;
                trail.push_str(&format!("{id}|{rule}|{}|{upheld};", who.badge));
                decisions += 1;
            }
            let mut set = serde_json::Map::new();
            set.insert("decided_by".into(), json!(who.badge));
            t += 0.001;
            store.transition(&id, 1, "leased", "decided", &who.badge, t, None, Some(set), Some(&who.badge)).map_err(|e| e.to_string())?.ok_or("finish refused")?;
            claims += 1;
        }
        Ok(Summary { claims, decisions, fingerprint: cg_queue::reliability::hash_text(&trail) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_queue::store::MemoryQueueStore;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn the_generator_is_deterministic_and_every_record_is_marked_simulated() {
        let corpus = load_corpus(&root()).unwrap();
        let run = |seed| {
            let store = MemoryQueueStore::new();
            let s = Lab::new(seed, false).run(&store, &corpus, 600, &RoutingConfig::default(), None).unwrap();
            let decided = store.by_state("decided", 100_000).unwrap();
            (s, decided)
        };
        let (a, da) = run(5);
        let (b, db) = run(5);
        assert_eq!(a.decisions, b.decisions);
        assert_eq!(da.iter().map(|d| d["decisions"].clone()).collect::<Vec<_>>(), db.iter().map(|d| d["decisions"].clone()).collect::<Vec<_>>(), "same seed, same decisions");
        assert!(a.decisions >= 600);
        assert!(da.iter().all(|d| d["claim"]["simulated"] == true && d["decisions"].as_array().unwrap().iter().all(|x| x["simulated"] == true)));
        let (c, _) = run(6);
        assert_ne!(a.fingerprint, c.fingerprint, "a different seed gives different decisions");
    }

    #[test]
    fn reviewers_follow_the_hidden_truth_and_the_learner_recovers_it() {
        let corpus = load_corpus(&root()).unwrap();
        let store = MemoryQueueStore::new();
        Lab::new(1, false).run(&store, &corpus, 4000, &RoutingConfig::default(), None).unwrap();
        let counted = reliability::counted_decisions(&store.by_state("decided", 100_000).unwrap());
        let snap = reliability::make_snapshot(&counted, &reliability::Params::from_config(&RoutingConfig::default()), 1, 0.0, 1);
        let r006 = &snap["rules"]["R006"];
        assert!(r006["lower"].as_f64().unwrap() < 0.35 && 0.35 < r006["upper"].as_f64().unwrap(), "the interval for R006 contains the hidden 0.35: {r006}");
        assert_eq!(r006["status"], "doubtful");
        assert_ne!(snap["rules"]["R001"]["status"], "doubtful");
    }

    #[test]
    fn juniors_never_decide_a_claim_that_needs_a_senior() {
        let corpus = load_corpus(&root()).unwrap();
        let store = MemoryQueueStore::new();
        Lab::new(2, false).run(&store, &corpus, 800, &RoutingConfig::default(), None).unwrap();
        for d in store.by_state("decided", 100_000).unwrap() {
            if d["receipt"]["eligibility"] == "decide_high" {
                assert!(d["decided_by"].as_str().unwrap().starts_with("SIM-3"), "{}", d["claim_id"]);
            }
        }
    }
}
