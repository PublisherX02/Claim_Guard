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
use crate::triage;
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
    let breakdown = time_breakdown(decided, max_hold);
    out["time"]["breakdown"] = breakdown.clone();
    out["planning_per_500_claims"] = planning(decided, &breakdown, &out, PLANNING_AUDIT_PERCENT);
    out["decisions"] = json!(counted.len());
    out
}

/// The audit share assumed by the hours estimate for auto-clear and bulk confirm (the default `audit_percent`).
pub const PLANNING_AUDIT_PERCENT: f64 = 5.0;

fn event_at(d: &Value, to: &str) -> Option<f64> {
    d["events"].as_array().into_iter().flatten().filter(|e| e["to"] == to).filter_map(|e| e["at"].as_f64()).fold(None, |a: Option<f64>, x| Some(a.map_or(x, |m| m.max(x))))
}

fn median(v: &mut Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(quantile(v, 0.5))
}

/// Where a reviewer's time goes, from the dossiers' own timestamps (nothing is invented; a part with no usable timestamps is null):
/// * `green_verify`: lease to decision on a claim with nothing flagged that a person verified clear;
/// * `open_claim`: lease to the first decision on a claim with findings (reading the claim and the first finding);
/// * `per_finding`: the gap between one decision and the next on the same claim, overall and by rule. Bulk decisions are left out (they take no time
///   by construction), and so are gaps longer than `max_hold` (a break is not review time).
pub fn time_breakdown(decided: &[Value], max_hold: f64) -> Value {
    let (mut green, mut open, mut gaps) = (vec![], vec![], vec![]);
    let mut by_rule: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for d in decided {
        if d["auto_cleared"] == true {
            continue;
        }
        let Some(leased) = event_at(d, "leased") else { continue };
        let flagged = triage::flagged(&d["results"]);
        if flagged.is_empty() {
            if let Some(done) = event_at(d, "decided") {
                if done > leased && done - leased <= max_hold && !triage::degraded(&d["results"]) {
                    green.push(done - leased);
                }
            }
            continue;
        }
        let mut decisions: Vec<&Value> = d["decisions"].as_array().into_iter().flatten().filter(|x| x["round"].as_i64().unwrap_or(1) == 1 && x["reason"].as_str() != Some(crate::service::BULK_REASON) && x["actor"].as_str() != Some(crate::flow::AUTO_REQUEST)).filter(|x| x["at"].as_f64().is_some()).collect();
        decisions.sort_by(|a, b| a["at"].as_f64().partial_cmp(&b["at"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        let mut previous = leased;
        for (i, x) in decisions.iter().enumerate() {
            let at = x["at"].as_f64().unwrap_or(0.0);
            let gap = at - previous;
            previous = at;
            if !(0.0..=max_hold).contains(&gap) {
                continue;
            }
            if i == 0 {
                open.push(gap);
            } else {
                gaps.push(gap);
                by_rule.entry(x["rule_id"].as_str().unwrap_or("").to_string()).or_default().push(gap);
            }
        }
    }
    let part = |v: &mut Vec<f64>| match median(v) {
        Some(m) => json!({"n": v.len(), "median_seconds": m}),
        None => Value::Null,
    };
    let rules: serde_json::Map<String, Value> = by_rule.iter_mut().map(|(k, v)| (k.clone(), part(v))).collect();
    json!({"green_verify": part(&mut green), "open_claim": part(&mut open), "per_finding": part(&mut gaps), "per_finding_by_rule": rules,
           "note": "medians of the queue's own timestamps; a part with no usable timestamps is null"})
}

/// What each saving is worth in reviewer hours per 500 claims, from this queue's own mix and measured seconds. Null where a number it needs has not been
/// measured. `look` is the share of the normal time a lighter look takes: it is an input (0, 0.25, 0.5), never a measured figure.
pub fn planning(decided: &[Value], breakdown: &Value, table: &Value, audit_percent: f64) -> Value {
    let people: Vec<&Value> = decided.iter().filter(|d| d["auto_cleared"] != true && d["decided_by"] != crate::flow::AUTO_REQUEST).collect();
    if people.is_empty() {
        return json!({"note": "no decided claims yet"});
    }
    let n = people.len() as f64;
    let mut green_n = 0.0;
    let mut findings = 0.0;
    for d in &people {
        let f = triage::flagged(&d["results"]).len();
        if f == 0 && !triage::degraded(&d["results"]) {
            green_n += 1.0;
        }
        findings += f as f64;
    }
    let flagged_share = people.iter().filter(|d| !triage::flagged(&d["results"]).is_empty()).count() as f64 / n;
    let (green_share, findings_per_claim) = (green_n / n, findings / n);
    let sec = |k: &str| breakdown[k]["median_seconds"].as_f64();
    let (green_s, open_s, find_s) = (sec("green_verify"), sec("open_claim"), sec("per_finding"));
    let hours = |s: f64| s * 500.0 / 3600.0;
    let share_light = table["light_review"]["share_of_all_findings"].as_f64().unwrap_or(0.0);
    let share_retune = table["retune"]["share_of_all_findings"].as_f64().unwrap_or(0.0);
    let keep = 1.0 - audit_percent / 100.0;
    let total = match (green_s, open_s, find_s) {
        (Some(g), Some(o), Some(f)) => Some(hours(green_share * g + flagged_share * o + findings_per_claim * f)),
        _ => None,
    };
    let auto_clear = green_s.map(|g| hours(green_share * g * keep));
    let retune = find_s.map(|f| hours(findings_per_claim * share_retune * f));
    let bulk = |look: f64| find_s.map(|f| hours(findings_per_claim * share_light * f * (1.0 - look) * keep));
    json!({"mix": {"claims": people.len(), "green_share": green_share, "flagged_share": flagged_share, "findings_per_claim": findings_per_claim},
           "hours_per_500_claims_today": total,
           "saved_hours": {"auto_clear_green": auto_clear, "retune_wasted_reviews": retune,
                           "bulk_confirm_if_a_light_look_takes": {"0%": bulk(0.0), "25%": bulk(0.25), "50%": bulk(0.5)}},
           "assumes": {"audit_percent": audit_percent, "note": "bulk confirm and retune savings are independent estimates and may overlap; the look fraction is an input, not a measurement"}})
}

/// The upper end of a two-sided 90% interval is the one-sided 95% upper bound: "with 95% confidence the rate is at most this". It is the natural bound for
/// a miss rate, and with no miss in n audited claims it is 1 - 0.05^(1/n) (0.99% at n = 300), the table in docs/50.
const ONE_SIDED_95: f64 = 0.90;

/// What the audit sample says about each automatic step, from the claims a person handled in full while the system held back.
///
/// * auto-clear: the audited green claims. An escalation means a person saw something the engine did not flag; every escalation counts as a possible
///   miss, so the bound is conservative. `suggestion` says what the evidence supports for `audit_percent` (an administrator decides; nothing changes by itself).
/// * auto-request: the audited claims with missing-data findings. The person either asked for the information too (agreed) or confirmed or dismissed
///   the finding instead (differed): a difference means the system would have asked for something a person would not have.
pub fn audit_report(docs: &[Value]) -> Value {
    let (mut auto_cleared, mut audited, mut clear, mut escalated, mut escalated_cleared, mut pending) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let (mut requested_claims, mut requested_findings) = (0i64, 0i64);
    let (mut info_audited, mut agreed, mut differed, mut info_pending) = (0i64, 0i64, 0i64, 0i64);
    for d in docs {
        if d["auto_cleared"] == true {
            auto_cleared += 1;
            continue;
        }
        requested_findings += d["decisions"].as_array().into_iter().flatten().filter(|x| x["actor"] == crate::flow::AUTO_REQUEST).count() as i64;
        if d["decided_by"] == crate::flow::AUTO_REQUEST {
            requested_claims += 1;
            continue;
        }
        let events = d["events"].as_array().cloned().unwrap_or_default();
        if events.iter().any(|e| e["detail"]["info_audit"] == true) {
            info_audited += 1;
            let missing: Vec<&str> = triage::flagged(&d["results"]).into_iter().filter(|r| r["status"] == "UNABLE_TO_ASSESS").filter_map(|r| r["rule_id"].as_str()).collect();
            let mut last: BTreeMap<&str, &str> = BTreeMap::new();
            for x in d["decisions"].as_array().into_iter().flatten() {
                last.insert(x["rule_id"].as_str().unwrap_or(""), x["action"].as_str().unwrap_or(""));
            }
            if missing.iter().any(|r| !last.contains_key(r)) {
                info_pending += 1;
            } else if missing.iter().all(|r| last[r] == "request_information") {
                agreed += 1;
            } else {
                differed += 1;
            }
        }
        if !events.iter().any(|e| e["to"] == "ready" && e["detail"]["why"] == "green_audit") {
            continue;
        }
        audited += 1;
        let went_up = d["escalated"] == true || events.iter().any(|e| e["detail"]["event"] == "escalated");
        match (went_up, d["state"].as_str()) {
            (true, Some("decided")) => {
                escalated += 1;
                escalated_cleared += 1;
            }
            (true, _) => escalated += 1,
            (false, Some("decided")) => clear += 1,
            _ => pending += 1,
        }
    }
    let resolved = clear + escalated;
    let ci = crate::shadow::clopper_pearson(escalated, resolved, ONE_SIDED_95);
    let upper = ci.map(|c| c.1);
    let suggestion = match upper {
        None => "no audited green claims have been resolved yet: keep audit_percent as it is",
        Some(_) if resolved < 300 => "keep audit_percent as it is: fewer than 300 audited claims have been resolved, too few to bound the miss rate at 1%",
        Some(u) if u <= 0.01 => "the audit supports lowering audit_percent: with 95% confidence at most 1% of cleared claims carry a sign the engine missed something",
        Some(u) if u >= 0.05 => "the escalation rate is high: read the escalated claims and consider switching auto-clear off",
        Some(_) => "keep audit_percent as it is: the miss rate is not yet bounded below 1%",
    };
    let info_resolved = agreed + differed;
    let info_ci = crate::shadow::clopper_pearson(differed, info_resolved, ONE_SIDED_95);
    json!({"auto_cleared": auto_cleared,
           "audited": {"total": audited, "verified_clear": clear, "escalated": escalated, "escalated_then_cleared_by_a_senior": escalated_cleared, "still_open": pending},
           "escalation_rate": if resolved > 0 { json!(escalated as f64 / resolved as f64) } else { Value::Null },
           "escalation_rate_upper_95": upper,
           "auto_cleared_with_a_sign_the_engine_missed_at_most": upper.map(|u| u * auto_cleared as f64),
           "suggestion": suggestion,
           "auto_request": {"claims_the_system_requested_information_for": requested_claims, "findings_the_system_requested": requested_findings,
                            "audited": {"total": info_audited, "agreed": agreed, "differed": differed, "still_open": info_pending},
                            "differed_rate": if info_resolved > 0 { json!(differed as f64 / info_resolved as f64) } else { Value::Null },
                            "differed_rate_upper_95": info_ci.map(|c| c.1)},
           "note": "every escalation of an audited green claim is counted as a possible miss, so the bound is conservative; it needs audited claims to exist, and says nothing before they do. A person who differs on a missing-data finding confirmed or dismissed it where the system would have asked for information"})
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

    fn flagged_doc(rules: &[&str], leased: f64, decisions: &[(&str, f64, &str)]) -> Value {
        let mut results: Vec<Value> = triage::rule_ids().into_iter().map(|r| json!({"rule_id": r, "status": if rules.contains(&r.as_str()) { "FAIL" } else { "PASS" }, "severity": "medium"})).collect();
        results.sort_by_key(|r| r["rule_id"].as_str().unwrap_or("").to_string());
        json!({"results": results, "events": [{"to": "leased", "at": leased}, {"to": "decided", "at": 9999.0}],
               "decisions": decisions.iter().map(|(r, at, reason)| json!({"rule_id": r, "at": at, "round": 1, "action": "confirm_issue", "reason": reason})).collect::<Vec<_>>()})
    }

    #[test]
    fn the_breakdown_separates_green_verification_opening_a_claim_and_each_finding_and_skips_bulk() {
        let green = |lease: f64, done: f64| json!({"results": triage::rule_ids().into_iter().map(|r| json!({"rule_id": r, "status": "PASS", "severity": "medium"})).collect::<Vec<_>>(),
                                                   "events": [{"to": "leased", "at": lease}, {"to": "decided", "at": done}], "decisions": []});
        let docs = vec![
            green(0.0, 20.0), green(0.0, 40.0), green(0.0, 60.0),
            flagged_doc(&["R001", "R002", "R003"], 100.0, &[("R001", 130.0, "x"), ("R002", 150.0, "x"), ("R003", 190.0, "x")]),
            flagged_doc(&["R001", "R002"], 500.0, &[("R001", 540.0, "x"), ("R002", 560.0, crate::service::BULK_REASON)]),
            json!({"auto_cleared": true, "results": [], "events": [{"to": "decided", "at": 1.0}]}),
        ];
        let b = time_breakdown(&docs, 1800.0);
        assert_eq!((b["green_verify"]["n"].as_i64(), b["green_verify"]["median_seconds"].as_f64()), (Some(3), Some(40.0)));
        assert_eq!((b["open_claim"]["n"].as_i64(), b["open_claim"]["median_seconds"].as_f64()), (Some(2), Some(40.0)), "30 and 40 seconds to the first decision: the upper median");
        assert_eq!(b["per_finding"]["n"], 2, "20 and 40 seconds between findings; the bulk decision is not counted");
        assert_eq!(b["per_finding_by_rule"]["R002"]["median_seconds"].as_f64(), Some(20.0));
        assert_eq!(b["per_finding_by_rule"]["R003"]["median_seconds"].as_f64(), Some(40.0));
        assert!(b["per_finding_by_rule"].get("R001").is_none(), "the first decision of a claim is opening it, not a finding");
    }

    #[test]
    fn a_break_is_not_review_time_and_a_part_without_timestamps_is_null() {
        let b = time_breakdown(&[flagged_doc(&["R001"], 0.0, &[("R001", 7200.0, "x")])], 1800.0);
        assert!(b["open_claim"].is_null() && b["green_verify"].is_null() && b["per_finding"].is_null());
    }

    #[test]
    fn planning_turns_measured_seconds_into_hours_per_500_claims_and_is_null_without_them() {
        let green = |lease: f64, done: f64| json!({"results": triage::rule_ids().into_iter().map(|r| json!({"rule_id": r, "status": "PASS", "severity": "medium"})).collect::<Vec<_>>(),
                                                   "events": [{"to": "leased", "at": lease}, {"to": "decided", "at": done}], "decisions": []});
        // 1 green claim of 36 s, 1 flagged claim with two findings (30 s to open, 20 s for the second)
        let docs = vec![green(0.0, 36.0), flagged_doc(&["R001", "R002"], 100.0, &[("R001", 130.0, "x"), ("R002", 150.0, "x")])];
        let b = time_breakdown(&docs, 1800.0);
        let table = json!({"light_review": {"share_of_all_findings": 0.5}, "retune": {"share_of_all_findings": 0.1}});
        let p = planning(&docs, &b, &table, 5.0);
        // per claim: 0.5 green x 36 + 0.5 flagged x 30 + 1.0 findings x 20 = 53 s; x 500 / 3600
        assert!((p["hours_per_500_claims_today"].as_f64().unwrap() - 53.0 * 500.0 / 3600.0).abs() < 1e-9, "{p}");
        assert!((p["saved_hours"]["auto_clear_green"].as_f64().unwrap() - 0.5 * 36.0 * 0.95 * 500.0 / 3600.0).abs() < 1e-9);
        assert!((p["saved_hours"]["retune_wasted_reviews"].as_f64().unwrap() - 1.0 * 0.1 * 20.0 * 500.0 / 3600.0).abs() < 1e-9);
        let bulk0 = p["saved_hours"]["bulk_confirm_if_a_light_look_takes"]["0%"].as_f64().unwrap();
        let bulk50 = p["saved_hours"]["bulk_confirm_if_a_light_look_takes"]["50%"].as_f64().unwrap();
        assert!((bulk50 - bulk0 / 2.0).abs() < 1e-9, "a look that takes half the time saves half as much");
        assert!(planning(&[], &b, &table, 5.0)["note"].is_string());
        let no_times = vec![json!({"results": [], "events": [], "decisions": []})];
        assert!(planning(&no_times, &time_breakdown(&no_times, 1800.0), &table, 5.0)["hours_per_500_claims_today"].is_null());
    }

    #[test]
    fn the_audit_report_counts_what_the_sample_found_and_bounds_the_rest() {
        let audited = |state: &str, escalated: bool| {
            let mut ev = vec![json!({"to": "ready", "detail": {"why": "green_audit"}})];
            if escalated {
                ev.push(json!({"to": "ready", "detail": {"event": "escalated"}}));
            }
            json!({"state": state, "escalated": escalated, "events": ev})
        };
        let mut docs: Vec<Value> = (0..200).map(|_| json!({"auto_cleared": true, "state": "decided"})).collect();
        docs.extend((0..9).map(|_| audited("decided", false)));
        docs.push(audited("decided", true));
        docs.push(audited("ready", true));
        docs.push(audited("ready", false));
        docs.push(json!({"state": "decided", "events": [{"to": "ready", "detail": {"why": "green"}}]}));
        let r = audit_report(&docs);
        assert_eq!(r["auto_cleared"], 200);
        assert_eq!((r["audited"]["total"].as_i64(), r["audited"]["verified_clear"].as_i64(), r["audited"]["escalated"].as_i64(), r["audited"]["still_open"].as_i64()), (Some(12), Some(9), Some(2), Some(1)));
        assert_eq!(r["audited"]["escalated_then_cleared_by_a_senior"], 1);
        let (rate, upper, expected) = (r["escalation_rate"].as_f64().unwrap(), r["escalation_rate_upper_95"].as_f64().unwrap(), r["auto_cleared_with_a_sign_the_engine_missed_at_most"].as_f64().unwrap());
        assert!((rate - 2.0 / 11.0).abs() < 1e-9 && upper > rate && (expected - upper * 200.0).abs() < 1e-9);
        assert!(audit_report(&[])["escalation_rate"].is_null(), "nothing audited, nothing claimed");
        assert!(r["suggestion"].as_str().unwrap().contains("fewer than 300"), "12 audited claims bound nothing");
    }

    fn audited_green(escalated: bool) -> Value {
        let mut ev = vec![json!({"to": "ready", "detail": {"why": "green_audit"}})];
        if escalated {
            ev.push(json!({"to": "ready", "detail": {"event": "escalated"}}));
        }
        json!({"state": "decided", "escalated": escalated, "events": ev})
    }

    #[test]
    fn the_suggestion_to_lower_the_audit_needs_enough_clean_audited_claims_and_a_tight_bound() {
        let mut docs: Vec<Value> = (0..1000).map(|_| json!({"auto_cleared": true, "state": "decided"})).collect();
        docs.extend((0..299).map(|_| audited_green(false)));
        assert!(audit_report(&docs)["suggestion"].as_str().unwrap().contains("fewer than 300"), "299 clean claims are one short");
        docs.push(audited_green(false));
        assert!(audit_report(&docs)["suggestion"].as_str().unwrap().contains("supports lowering"), "300 clean audited claims bound the miss rate under 1%");
        docs.push(audited_green(true));
        assert!(audit_report(&docs)["suggestion"].as_str().unwrap().contains("not yet bounded below 1%"), "one escalation in 301 is no longer under 1%");
        let mut bad: Vec<Value> = (0..300).map(|_| audited_green(false)).collect();
        bad.extend((0..30).map(|_| audited_green(true)));
        assert!(audit_report(&bad)["suggestion"].as_str().unwrap().contains("switching auto-clear off"), "a high escalation rate says to stop");
    }

    #[test]
    fn the_request_audit_counts_where_a_person_asked_too_and_where_they_did_not() {
        let missing = |rule: &str| json!({"rule_id": rule, "status": "UNABLE_TO_ASSESS", "severity": "medium"});
        let doc = |rules: &[&str], actions: &[(&str, &str)]| json!({"state": "leased", "results": rules.iter().map(|r| missing(r)).collect::<Vec<_>>(),
            "events": [{"to": "ready", "detail": {"why": "explained", "info_audit": true}}],
            "decisions": actions.iter().map(|(r, a)| json!({"rule_id": r, "action": a, "actor": "CG-2002"})).collect::<Vec<_>>()});
        let docs = vec![
            doc(&["R009"], &[("R009", "request_information")]),
            doc(&["R009", "R010"], &[("R009", "request_information"), ("R010", "request_information")]),
            doc(&["R009"], &[("R009", "dismiss_with_reason")]),
            doc(&["R009", "R010"], &[("R009", "request_information")]),
            json!({"decided_by": crate::flow::AUTO_REQUEST, "decisions": [{"rule_id": "R009", "actor": crate::flow::AUTO_REQUEST}, {"rule_id": "R010", "actor": crate::flow::AUTO_REQUEST}]}),
        ];
        let r = audit_report(&docs);
        let a = &r["auto_request"];
        assert_eq!((a["audited"]["total"].as_i64(), a["audited"]["agreed"].as_i64(), a["audited"]["differed"].as_i64(), a["audited"]["still_open"].as_i64()), (Some(4), Some(2), Some(1), Some(1)));
        assert_eq!((a["claims_the_system_requested_information_for"].as_i64(), a["findings_the_system_requested"].as_i64()), (Some(1), Some(2)));
        assert!((a["differed_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9 && a["differed_rate_upper_95"].as_f64().unwrap() > 1.0 / 3.0);
    }
}
