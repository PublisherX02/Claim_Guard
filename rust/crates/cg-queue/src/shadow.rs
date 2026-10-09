//! Shadow mode: record what an automatic clearer would have done, and compare it with what the people decided. It acts on nothing
//! (`src/workqueue/shadow.py`).
//!
//! The baseline predictor is a plain rule: a claim with no flagged findings would be cleared, every other claim would not. It stands in for
//! whatever a later, trained model would be. Whether any trained model may ever sit in the decision path is a question for the mentor, and
//! nothing here lets it: the prediction is stored beside the claim, never used for routing, and written once so it cannot be adjusted after
//! the people have decided.
//!
//! `agreement` reports how often the people's outcome matched the prediction, with an exact (Clopper-Pearson) 95 % interval, so a small
//! sample is not mistaken for a result. The interval comes from the binomial distribution directly; no statistics package is used.

use crate::store::QueueStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const MODEL: &str = "baseline-green-clears";
const BIG: usize = 100_000;

/// 1.0 for a claim with nothing flagged and a sound result set, 0.0 for every other claim.
pub fn predict_clear(receipt: &Value) -> f64 {
    if receipt["lane"] == "green" && receipt["degraded"] != true { 1.0 } else { 0.0 }
}

/// Stores the prediction once. False if one was already stored or the claim is unknown.
pub fn record(store: &dyn QueueStore, claim_id: &str, version: i64, prediction: f64, now: f64) -> bool {
    if !(0.0..=1.0).contains(&prediction) {
        return false;
    }
    store.set_shadow(claim_id, version, &json!({"predicted_clear": prediction, "model": MODEL, "at": now})).unwrap_or(false)
}

/// True when the people decided the claim has nothing wrong: no flagged finding was confirmed (a green claim was verified clear).
pub fn human_cleared(doc: &Value) -> bool {
    let mut latest: BTreeMap<String, String> = BTreeMap::new();
    for d in doc["decisions"].as_array().into_iter().flatten() {
        latest.insert(d["rule_id"].as_str().unwrap_or("").to_string(), d["action"].as_str().unwrap_or("").to_string());
    }
    !latest.values().any(|a| a == "confirm_issue")
}

/// ln Gamma(x) (Lanczos, g = 7), accurate to about 1e-13 for the sizes used here.
fn lgamma(x: f64) -> f64 {
    const C: [f64; 9] = [0.999_999_999_999_809_9, 676.520_368_121_885_1, -1_259.139_216_722_402_8, 771.323_428_777_653_1, -176.615_029_162_140_6, 12.507_343_278_686_905, -0.138_571_095_265_720_12, 9.984_369_578_019_572e-6, 1.505_632_735_149_311_6e-7];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - lgamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + 7.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// P(X <= k) for X ~ Binomial(n, p), summed from log-probabilities so large n does not overflow.
fn cdf(k: i64, n: i64, p: f64) -> f64 {
    if k < 0 {
        return 0.0;
    }
    if k >= n {
        return 1.0;
    }
    if p <= 0.0 {
        return 1.0;
    }
    if p >= 1.0 {
        return 0.0;
    }
    let (lp, lq) = (p.ln(), (-p).ln_1p());
    let total: f64 = (0..=k).map(|i| (lgamma(n as f64 + 1.0) - lgamma(i as f64 + 1.0) - lgamma((n - i) as f64 + 1.0) + i as f64 * lp + (n - i) as f64 * lq).exp()).sum();
    total.min(1.0)
}

fn bisect(f: impl Fn(f64) -> f64, target: f64, increasing: bool) -> f64 {
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..80 {
        let mid = (low + high) / 2.0;
        if (f(mid) < target) == increasing { low = mid } else { high = mid }
    }
    (low + high) / 2.0
}

/// Exact two-sided interval for a proportion k/n, or None when n is 0.
pub fn clopper_pearson(k: i64, n: i64, confidence: f64) -> Option<(f64, f64)> {
    if n == 0 || !(0..=n).contains(&k) {
        return None;
    }
    let alpha = 1.0 - confidence;
    let lower = if k == 0 { 0.0 } else { bisect(|p| 1.0 - cdf(k - 1, n, p), alpha / 2.0, true) };
    let upper = if k == n { 1.0 } else { bisect(|p| cdf(k, n, p), alpha / 2.0, false) };
    Some((lower, upper))
}

/// Over the decided claims that carry a shadow prediction: how often prediction and people agreed.
pub fn agreement(store: &dyn QueueStore, confidence: f64) -> Value {
    let (mut n, mut agree) = (0i64, 0i64);
    for doc in store.by_state("decided", BIG).unwrap_or_default() {
        let Some(p) = doc["shadow"]["predicted_clear"].as_f64() else { continue };
        n += 1;
        agree += i64::from((p >= 0.5) == human_cleared(&doc));
    }
    let ci = clopper_pearson(agree, n, confidence);
    json!({"n": n, "agree": agree, "rate": if n > 0 { json!(agree as f64 / n as f64) } else { Value::Null }, "confidence": confidence,
           "lower": ci.map(|c| c.0), "upper": ci.map(|c| c.1), "model": MODEL})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baseline_clears_only_a_clean_sound_claim() {
        assert_eq!(predict_clear(&json!({"lane": "green", "degraded": false})), 1.0);
        assert_eq!(predict_clear(&json!({"lane": "green", "degraded": true})), 0.0);
        assert_eq!(predict_clear(&json!({"lane": "A"})), 0.0);
    }

    #[test]
    fn humans_clear_a_claim_unless_they_confirmed_an_issue_in_their_latest_decision() {
        assert!(human_cleared(&json!({"decisions": []})));
        assert!(!human_cleared(&json!({"decisions": [{"rule_id": "R001", "action": "confirm_issue"}]})));
        assert!(human_cleared(&json!({"decisions": [{"rule_id": "R001", "action": "confirm_issue"}, {"rule_id": "R001", "action": "dismiss_with_reason"}]})));
    }

    #[test]
    fn the_exact_interval_matches_known_values() {
        let (lo, hi) = clopper_pearson(5, 10, 0.95).unwrap();
        assert!((lo - 0.187_086).abs() < 1e-4 && (hi - 0.812_914).abs() < 1e-4, "{lo} {hi}");
        let (lo, hi) = clopper_pearson(0, 10, 0.95).unwrap();
        assert!(lo == 0.0 && (hi - 0.308_5).abs() < 1e-3, "{hi}");
        let (lo, hi) = clopper_pearson(10, 10, 0.95).unwrap();
        assert!(hi == 1.0 && (lo - 0.691_5).abs() < 1e-3);
        assert!(clopper_pearson(0, 0, 0.95).is_none());
        let (lo, hi) = clopper_pearson(500, 1000, 0.95).unwrap();
        assert!(lo > 0.468 && hi < 0.532, "{lo} {hi}");
    }
}
