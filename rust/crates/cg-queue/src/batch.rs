//! Batches of claims, the audit sample of a batch, and quarantine (docs/53).
//!
//! A batch is one submission (a file or a request). When `auto_clear_green` is on, the system clears the green claims of a batch except an audit
//! sample that goes to officers. The sample is an EXACT share of the batch's green claims (`sample_size`), chosen without replacement from a seed that
//! is written on the batch, so anyone can check the draw.
//!
//! The moment an officer says one of the sampled claims was wrong (`escalate`, with the rule that should have caught it), the whole batch is
//! quarantined: its cleared claims are held. The administrator sees only the batch id, how many sampled claims were flagged out of how many, and the
//! categories and the officers' short notes (checked for patient identifiers when written; never a claim id, a claim value or a badge), and decides: release the batch, send N of its cleared claims back to officers, or
//! send all of them back.
//!
//! A batch has no mutable record. Its status is the fold of its append-only events (`fold`), the same shape as the review log: nothing is
//! overwritten, so the history of a quarantine can be read afterwards.

use rand::{seq::index::sample, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// What an officer picks when a sampled claim was wrong: the rule that should have flagged it, or "other".
pub const CATEGORIES: [&str; 16] = ["R001", "R002", "R003", "R004", "R005", "R006", "R007", "R008", "R009", "R010", "R011", "R012", "R013", "R014", "R015", "other"];
pub const NOTE_CHARS: usize = 200;
/// The largest batch: the number of claims one submission may carry.
pub const MAX_BATCH: usize = 1000;

pub fn is_category(c: &str) -> bool {
    CATEGORIES.contains(&c)
}

/// `B-` plus the time in base 36 plus 4 random hex digits: readable, sortable and not guessable from the previous one.
pub fn new_batch_id(now: f64) -> String {
    use rand::{rngs::OsRng, RngCore};
    let mut t = now.max(0.0) as u64;
    let mut digits = vec![];
    while t > 0 {
        digits.push(char::from_digit((t % 36) as u32, 36).unwrap_or('0'));
        t /= 36;
    }
    digits.reverse();
    format!("B-{}-{:04X}", digits.into_iter().collect::<String>(), OsRng.next_u32() & 0xFFFF)
}

/// How many of `green` green claims the audit looks at: `percent` of them rounded up, and at least one (a batch with any green claim always has a sample,
/// so a quarantine is always possible).
pub fn sample_size(green: usize, percent: i64) -> usize {
    if green == 0 {
        return 0;
    }
    let p = percent.clamp(1, 100) as usize;
    (green * p).div_ceil(100).clamp(1, green)
}

/// The positions (among the batch's green claims, counting from 0) that go to officers: exactly `sample_size`, no repeats, reproducible from `seed`.
pub fn choose(green: usize, percent: i64, seed: u64) -> BTreeSet<usize> {
    let k = sample_size(green, percent);
    if k == 0 {
        return BTreeSet::new();
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    sample(&mut rng, green, k).into_iter().collect()
}

/// `n` of the given items, no repeats, reproducible from `seed` (the claims an administrator sends back for review).
pub fn pick<T: Clone>(items: &[T], n: usize, seed: u64) -> Vec<T> {
    let n = n.min(items.len());
    if n == 0 {
        return vec![];
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut idx: Vec<usize> = sample(&mut rng, items.len(), n).into_iter().collect();
    idx.sort_unstable();
    idx.into_iter().map(|i| items[i].clone()).collect()
}

#[derive(Debug, Clone, Default)]
pub struct BatchView {
    pub batch_id: String,
    pub created_at: f64,
    pub submitted: i64,
    pub accepted: i64,
    pub green: i64,
    pub sampled: i64,
    pub percent: i64,
    pub auto_clear: bool,
    pub seed: u64,
    /// `open` (nothing wrong found), `quarantined`, `in_review` (the administrator sent claims back) or `released`.
    pub status: String,
    pub quarantined_at: Option<f64>,
    /// Claims (not events): a claim flagged twice counts once, a flagged claim is not also counted verified.
    pub flagged_claims: BTreeSet<String>,
    pub verified_claims: BTreeSet<String>,
    pub categories: BTreeMap<String, i64>,
    /// The officers' notes on flagged claims, one per flagged claim, with no claim id and no badge.
    pub notes: Vec<String>,
    pub decisions: Vec<Value>,
    pub reopened: BTreeSet<String>,
    /// Claim ids of the batch (the administrator's view never shows them).
    pub members: Vec<String>,
}

pub fn fold(events: &[Value]) -> Option<BatchView> {
    let mut v = BatchView::default();
    let mut seen = false;
    for e in events {
        match e["kind"].as_str().unwrap_or("") {
            "created" => {
                seen = true;
                v.batch_id = e["batch_id"].as_str().unwrap_or("").to_string();
                v.created_at = e["at"].as_f64().unwrap_or(0.0);
                v.submitted = e["submitted"].as_i64().unwrap_or(0);
                v.accepted = e["accepted"].as_i64().unwrap_or(0);
                v.green = e["green"].as_i64().unwrap_or(0);
                v.sampled = e["sampled"].as_i64().unwrap_or(0);
                v.percent = e["percent"].as_i64().unwrap_or(0);
                v.auto_clear = e["auto_clear"] == true;
                v.seed = e["seed"].as_u64().unwrap_or(0);
                v.status = "open".into();
                v.members = e["members"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
            }
            "sample_verified" if seen => {
                // a claim already flagged stays flagged: a second officer confirming it afterwards must not count it as confirmed too
                if let Some(c) = e["claim_id"].as_str().filter(|c| !v.flagged_claims.contains(*c)) {
                    v.verified_claims.insert(c.to_string());
                }
            }
            "flagged" if seen => {
                let Some(c) = e["claim_id"].as_str() else { continue };
                if v.flagged_claims.insert(c.to_string()) {
                    *v.categories.entry(e["category"].as_str().unwrap_or("other").to_string()).or_insert(0) += 1;
                    if let Some(n) = e["note"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
                        v.notes.push(n.to_string());
                    }
                }
                v.verified_claims.remove(c);
                if v.status != "in_review" && v.status != "quarantined" {
                    v.status = "quarantined".into();
                    v.quarantined_at = e["at"].as_f64();
                }
            }
            "decision" if seen => {
                v.decisions.push(json!({"action": e["action"], "n": e["n"], "at": e["at"], "by": e["by"]}));
                match e["action"].as_str().unwrap_or("") {
                    "release" => v.status = "released".into(),
                    "review_n" | "review_all" => {
                        v.status = "in_review".into();
                        v.reopened.extend(e["reopened"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(str::to_string)));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    seen.then_some(v)
}

/// A claim is held while its batch is quarantined or under review. (Released and open batches hold nothing.)
pub fn holds(status: &str) -> bool {
    status == "quarantined" || status == "in_review"
}

impl BatchView {
    /// What the administrator sees: the batch id, counts, categories and the officers' notes. No claim id, no claim value, no officer badge.
    /// `pending` is how many claims sent back for review are still with officers (None when it was not computed).
    pub fn to_json(&self, pending: Option<i64>) -> Value {
        let sample_flagged = self.flagged_claims.iter().filter(|c| !self.reopened.contains(*c)).count() as i64;
        let review_flagged = self.flagged_claims.len() as i64 - sample_flagged;
        let sample_verified = self.verified_claims.iter().filter(|c| !self.reopened.contains(*c)).count() as i64;
        let status = if self.status == "in_review" && pending == Some(0) { "review_done" } else { self.status.as_str() };
        json!({
            "batch_id": self.batch_id, "created_at": self.created_at, "claims": self.accepted, "green": self.green, "audit_percent": self.percent, "auto_clear": self.auto_clear,
            "sampled": self.sampled, "sample_flagged": sample_flagged, "sample_verified": sample_verified, "sample_waiting": (self.sampled - sample_flagged - sample_verified).max(0),
            "status": status, "held": holds(&self.status), "quarantined_at": self.quarantined_at, "categories": self.categories, "notes": self.notes,
            "review": {"sent_back": self.reopened.len(), "flagged": review_flagged, "still_with_officers": pending},
            "decisions": self.decisions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_is_an_exact_share_of_the_green_claims_and_never_empty() {
        assert_eq!((sample_size(0, 5), sample_size(1, 5), sample_size(19, 5), sample_size(20, 5), sample_size(21, 5)), (0, 1, 1, 1, 2));
        assert_eq!((sample_size(100, 5), sample_size(1000, 5), sample_size(333, 5), sample_size(10, 100), sample_size(10, 0)), (5, 50, 17, 10, 1));
        for green in [1usize, 7, 100, 333, 1000] {
            let s = choose(green, 5, 42);
            assert_eq!(s.len(), sample_size(green, 5), "exactly this many, no repeats");
            assert!(s.iter().all(|i| *i < green));
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_draw_and_another_seed_another() {
        assert_eq!(choose(1000, 5, 7), choose(1000, 5, 7));
        assert_ne!(choose(1000, 5, 7), choose(1000, 5, 8));
        let items: Vec<u32> = (0..50).collect();
        assert_eq!(pick(&items, 10, 3), pick(&items, 10, 3));
        assert_eq!(pick(&items, 10, 3).len(), 10);
        assert_eq!(pick(&items, 99, 3).len(), 50, "asking for more than there is gives all of them");
        assert!(pick(&items, 0, 3).is_empty());
    }

    #[test]
    fn every_position_can_be_drawn() {
        let mut hit = [false; 20];
        for seed in 0..400u64 {
            for i in choose(20, 5, seed) {
                hit[i] = true;
            }
        }
        assert!(hit.iter().all(|h| *h), "the draw has no favourite positions");
    }

    #[test]
    fn a_batch_id_looks_like_one() {
        let id = new_batch_id(1_760_000_000.0);
        assert!(id.starts_with("B-") && id.matches('-').count() == 2 && id.len() > 8, "{id}");
        assert_ne!(new_batch_id(1.0), new_batch_id(1.0), "random suffix");
    }

    fn created() -> Value {
        json!({"batch_id": "B1", "kind": "created", "at": 10.0, "submitted": 100, "accepted": 100, "green": 40, "sampled": 2, "percent": 5, "auto_clear": true, "seed": 9, "members": ["C1", "C2"]})
    }

    #[test]
    fn the_first_flag_quarantines_and_the_status_is_read_from_the_events_alone() {
        let mut ev = vec![created()];
        assert_eq!(fold(&ev).unwrap().status, "open");
        ev.push(json!({"batch_id": "B1", "kind": "sample_verified", "at": 11.0, "claim_id": "C1"}));
        assert_eq!(fold(&ev).unwrap().status, "open", "a verified sample claim changes nothing");
        ev.push(json!({"batch_id": "B1", "kind": "flagged", "at": 12.0, "claim_id": "C2", "category": "R007"}));
        ev.push(json!({"batch_id": "B1", "kind": "flagged", "at": 13.0, "claim_id": "C2", "category": "R007"}));
        let v = fold(&ev).unwrap();
        assert_eq!((v.status.as_str(), v.quarantined_at, v.flagged_claims.len(), v.categories["R007"]), ("quarantined", Some(12.0), 1, 1), "a claim flagged twice counts once");
        let j = v.to_json(None);
        assert_eq!((j["sampled"].as_i64(), j["sample_flagged"].as_i64(), j["sample_verified"].as_i64(), j["sample_waiting"].as_i64(), j["held"].as_bool()), (Some(2), Some(1), Some(1), Some(0), Some(true)));
        assert!(!j.to_string().contains("C2") && !j.to_string().contains("C1"), "the administrator's view names no claim");
    }

    #[test]
    fn a_decision_releases_or_sends_claims_back_and_a_later_flag_quarantines_a_released_batch_again() {
        let mut ev = vec![created(), json!({"batch_id": "B1", "kind": "flagged", "at": 12.0, "claim_id": "C2", "category": "other"})];
        ev.push(json!({"batch_id": "B1", "kind": "decision", "at": 20.0, "action": "release", "by": "CG-4004"}));
        assert_eq!(fold(&ev).unwrap().status, "released");
        assert!(!holds("released") && holds("quarantined") && holds("in_review") && !holds("open"));
        ev.push(json!({"batch_id": "B1", "kind": "flagged", "at": 21.0, "claim_id": "C9", "category": "R012"}));
        let v = fold(&ev).unwrap();
        assert_eq!((v.status.as_str(), v.quarantined_at), ("quarantined", Some(21.0)), "a new flag after a release holds the batch again");
        ev.push(json!({"batch_id": "B1", "kind": "decision", "at": 30.0, "action": "review_n", "n": 2, "by": "CG-4004", "reopened": ["C5", "C6"]}));
        let v = fold(&ev).unwrap();
        assert_eq!((v.status.as_str(), v.reopened.len()), ("in_review", 2));
        assert_eq!(v.to_json(Some(2))["status"], "in_review");
        assert_eq!(v.to_json(Some(0))["status"], "review_done", "everything sent back has been looked at");
        ev.push(json!({"batch_id": "B1", "kind": "flagged", "at": 31.0, "claim_id": "C5", "category": "R007"}));
        let j = fold(&ev).unwrap().to_json(Some(1));
        assert_eq!((j["review"]["flagged"].as_i64(), j["sample_flagged"].as_i64()), (Some(1), Some(2)), "flags are split into the audit sample and the claims sent back");
    }

    #[test]
    fn a_flagged_claim_that_a_second_officer_then_confirms_is_still_only_flagged_and_notes_carry_no_claim() {
        let ev = vec![
            created(),
            json!({"batch_id": "B1", "kind": "flagged", "at": 12.0, "claim_id": "C2", "category": "R007", "note": "  amount does not match  ", "by": "CG-2002"}),
            json!({"batch_id": "B1", "kind": "sample_verified", "at": 13.0, "claim_id": "C2", "by": "CG-3003"}),
        ];
        let j = fold(&ev).unwrap().to_json(None);
        assert_eq!((j["sample_flagged"].as_i64(), j["sample_verified"].as_i64(), j["sample_waiting"].as_i64()), (Some(1), Some(0), Some(1)), "C2 is flagged, not also confirmed; the other sample claim is still waiting");
        assert_eq!(j["notes"], json!(["amount does not match"]));
        assert!(!j.to_string().contains("C2") && !j.to_string().contains("CG-2002"));
    }

    #[test]
    fn events_of_a_batch_that_was_never_created_fold_to_nothing() {
        assert!(fold(&[json!({"batch_id": "B1", "kind": "flagged", "at": 1.0, "claim_id": "C", "category": "R001"})]).is_none());
        assert!(fold(&[]).is_none());
    }
}
