//! Where reviewer time goes, rule by rule, and what could be saved. Read-only: nothing here routes, clears or changes a claim.
//!
//! Two lists come out of the measured reliability of each rule (`reliability`):
//! - **retune**: findings that reviewers mostly override (we are 95% sure fewer than `doubtful_below` are upheld). Each overridden finding is a
//!   review that produced nothing; fixing or retuning the rule removes those reviews for good.
//! - **light review**: findings that reviewers almost always uphold (95% sure that at least `skip_lower` are). These are the candidates for a
//!   lighter look. The unverified wrong ones are counted with their exact bound, because that is the price of the saving.
//!
//! Time is only reported when the queue's own timestamps allow it (lease to last decision, per finding); otherwise counts are all there is.

use crate::reliability::{self, Counted, Params};
use serde_json::{json, Value};

pub const DEFAULT_SKIP_LOWER: f64 = 0.95;

/// The table for a snapshot. `skip_lower` is the least lower bound of the upheld rate for a rule to be a light-review candidate.
pub fn rule_table(snap: &Value, skip_lower: f64) -> Value {
    let mut rows = vec![];
    let (mut total, mut wasted, mut light, mut wrong_est, mut wrong_bound) = (0i64, 0i64, 0i64, 0.0f64, 0.0f64);
    for (id, r) in snap["rules"].as_object().into_iter().flatten() {
        let (n, up, over) = (r["n"].as_i64().unwrap_or(0), r["upheld"].as_i64().unwrap_or(0), r["overridden"].as_i64().unwrap_or(0));
        let (lower, rate) = (r["lower"].as_f64().unwrap_or(0.0), r["rate"].as_f64().unwrap_or(0.0));
        let role = match r["status"].as_str() {
            Some("doubtful") => "retune",
            Some("measured") if lower >= skip_lower => "light_review",
            _ => "full_review",
        };
        total += n;
        match role {
            "retune" => wasted += over,
            "light_review" => {
                light += n;
                wrong_est += n as f64 * (1.0 - rate);
                wrong_bound += n as f64 * (1.0 - lower);
            }
            _ => {}
        }
        rows.push(json!({"rule": id, "n": n, "reviewers": r["reviewers"], "upheld": up, "overridden": over, "rate": rate, "lower": lower, "upper": r["upper"], "status": r["status"], "role": role}));
    }
    rows.sort_by(|a, b| b["n"].as_i64().cmp(&a["n"].as_i64()).then_with(|| a["rule"].as_str().cmp(&b["rule"].as_str())));
    let share = |x: i64| if total > 0 { x as f64 / total as f64 } else { 0.0 };
    json!({"rules": rows, "findings_counted": total,
           "retune": {"wasted_reviews": wasted, "share_of_all_findings": share(wasted)},
           "light_review": {"findings": light, "share_of_all_findings": share(light), "expected_wrong_among_them": wrong_est, "wrong_at_most_95pct": wrong_bound, "skip_lower": skip_lower}})
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// Seconds a reviewer took per finding, from the dossiers' own timestamps: last lease to last decision, divided by the findings decided,
/// leaving out dossiers held longer than `max_hold` seconds (a lunch break is not review time). None when no dossier qualifies.
pub fn seconds_per_finding(decided: &[Value], max_hold: f64) -> Option<Value> {
    let mut per: Vec<f64> = vec![];
    for d in decided {
        let leased = d["events"].as_array().into_iter().flatten().filter(|e| e["to"] == "leased").filter_map(|e| e["at"].as_f64()).fold(f64::MIN, f64::max);
        let decisions: Vec<&Value> = d["decisions"].as_array().into_iter().flatten().filter(|x| x["round"].as_i64().unwrap_or(1) == 1).collect();
        let last = decisions.iter().filter_map(|x| x["at"].as_f64()).fold(f64::MIN, f64::max);
        if leased > f64::MIN && last > leased && !decisions.is_empty() && last - leased <= max_hold {
            per.push((last - leased) / decisions.len() as f64);
        }
    }
    if per.is_empty() {
        return None;
    }
    per.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(json!({"dossiers": per.len(), "median_seconds": quantile(&per, 0.5), "p90_seconds": quantile(&per, 0.9)}))
}

/// The whole report from the decided dossiers of a store.
pub fn report(decided: &[Value], p: &Params, skip_lower: f64, max_hold: f64) -> Value {
    let counted: Vec<Counted> = reliability::counted_decisions(decided);
    let snap = reliability::make_snapshot(&counted, p, 0, 0.0, 0);
    let mut out = rule_table(&snap, skip_lower);
    let timing = seconds_per_finding(decided, max_hold);
    if let Some(t) = &timing {
        let m = t["median_seconds"].as_f64().unwrap_or(0.0);
        out["time"] = json!({"measured": t, "retune_hours": out["retune"]["wasted_reviews"].as_i64().unwrap_or(0) as f64 * m / 3600.0,
            "light_review_hours_if_the_look_took_nothing": out["light_review"]["findings"].as_i64().unwrap_or(0) as f64 * m / 3600.0});
    } else {
        out["time"] = json!({"measured": null, "note": "no dossier has a usable lease-to-decision time, so only counts are reported"});
    }
    out["decisions"] = json!(counted.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params { min_decisions: 20, min_reviewers: 3, reviewer_cap: 1.0, doubtful_below: 0.6 }
    }

    fn snap_of(rows: &[(&str, i64, i64)]) -> Value {
        let p = params();
        let mut rules = serde_json::Map::new();
        for (id, up, over) in rows {
            rules.insert((*id).into(), reliability::row(*up, *over, 5, &p));
        }
        json!({"version": 1, "status": "proposed", "parameters": p.to_doc(), "rules": rules})
    }

    #[test]
    fn rules_are_sorted_into_retune_light_and_full_review_and_the_price_of_skipping_is_counted() {
        let t = rule_table(&snap_of(&[("R006", 8, 52), ("R001", 200, 2), ("R002", 25, 5), ("R003", 6, 0)]), 0.95);
        let role = |id: &str| t["rules"].as_array().unwrap().iter().find(|r| r["rule"] == id).unwrap()["role"].clone();
        assert_eq!((role("R006"), role("R001"), role("R002"), role("R003")), (json!("retune"), json!("light_review"), json!("full_review"), json!("full_review")), "{t}");
        assert_eq!(t["retune"]["wasted_reviews"], 52);
        assert_eq!(t["light_review"]["findings"], 202);
        let (est, bound) = (t["light_review"]["expected_wrong_among_them"].as_f64().unwrap(), t["light_review"]["wrong_at_most_95pct"].as_f64().unwrap());
        assert!(est > 0.0 && bound > est, "the bound is above the estimate: {est} {bound}");
        assert_eq!(t["findings_counted"], 298);
    }

    #[test]
    fn a_rule_with_a_high_rate_but_few_decisions_is_never_a_light_review_candidate() {
        let t = rule_table(&snap_of(&[("R001", 18, 0)]), 0.5);
        assert_eq!(t["rules"][0]["role"], "full_review", "below the minimum number of decisions the rule is unmeasured");
    }

    fn dossier(leased: f64, decided: &[f64]) -> Value {
        json!({"events": [{"to": "ready", "at": 1.0}, {"to": "leased", "at": leased}], "decisions": decided.iter().map(|t| json!({"at": t, "round": 1})).collect::<Vec<_>>()})
    }

    #[test]
    fn time_per_finding_comes_from_the_timestamps_and_ignores_a_dossier_held_all_afternoon() {
        let docs = vec![dossier(100.0, &[160.0, 220.0]), dossier(100.0, &[130.0]), dossier(100.0, &[100.0 + 7200.0]), dossier(100.0, &[])];
        let t = seconds_per_finding(&docs, 1800.0).unwrap();
        assert_eq!(t["dossiers"], 2);
        assert_eq!(t["median_seconds"].as_f64(), Some(60.0), "(120 / 2 = 60) and (30 / 1 = 30): the median of two takes the upper one");
        assert!(seconds_per_finding(&[dossier(5.0, &[])], 1800.0).is_none());
    }
}
