//! The numbers behind the administrator's live board and the daily JSON report: what came in, what was processed, what is still waiting, which rules
//! were violated and what each operator did. Pure functions over claim documents, so the arithmetic is tested without a server.
//!
//! Definitions (the report repeats them under `definitions`):
//! * received: a claim whose receipt was written inside the window;
//! * processed: a claim that reached `decided` or `rechecked` inside the window, by a person or cleared by the system;
//! * unprocessed: a claim that is not decided yet, at the moment the numbers are read (the backlog), whenever it came in;
//! * violated: a rule whose result on a received claim is FAIL; UNABLE_TO_ASSESS is counted on its own;
//! * an operator's claims: the distinct claims on which that badge recorded a decision inside the window.

use crate::flow::AUTO_REQUEST;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const HOUR: f64 = 3600.0;
const SERIES_HOURS: usize = 24;
const BACKLOG: [&str; 7] = ["received", "triaged", "explained", "explanation_skipped", "ready", "leased", "awaiting_countersign"];
const DONE: [&str; 2] = ["decided", "rechecked"];

/// A badge currently on shift, with the claims in its inbox that it has not taken yet.
pub struct OnShift {
    pub badge: String,
    pub inbox: i64,
}

#[derive(Default)]
struct Op {
    claims: BTreeSet<String>,
    decisions: i64,
    confirmed: i64,
    dismissed: i64,
    asked: i64,
    first: f64,
    last: f64,
    processing: i64,
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite())
}

fn in_window(t: Option<f64>, since: f64, now: f64) -> bool {
    t.is_some_and(|t| t >= since && t <= now)
}

/// The top of the hour that `t` falls in, for the hourly series.
fn hour_floor(t: f64) -> f64 {
    (t / HOUR).floor() * HOUR
}

pub fn build(docs: &[Value], on_shift: &[OnShift], since: f64, now: f64) -> Value {
    let (mut received, mut processed, mut auto_cleared, mut by_person) = (0_i64, 0_i64, 0_i64, 0_i64);
    let mut backlog: BTreeMap<&str, i64> = BACKLOG.iter().map(|s| (*s, 0)).collect();
    let (mut violations, mut unable) = (0_i64, 0_i64);
    let mut rules: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut ops: BTreeMap<String, Op> = BTreeMap::new();
    let top = hour_floor(now);
    let mut series: Vec<(i64, i64, i64)> = vec![(0, 0, 0); SERIES_HOURS];
    let slot = |t: f64| -> Option<usize> {
        let back = ((top - hour_floor(t)) / HOUR).round();
        (back >= 0.0 && (back as usize) < SERIES_HOURS).then(|| SERIES_HOURS - 1 - back as usize)
    };
    for d in docs {
        let state = d["state"].as_str().unwrap_or("");
        if let Some(n) = backlog.get_mut(state) {
            *n += 1;
            if state == "leased" {
                if let Some(b) = d["lease"]["badge_id"].as_str() {
                    ops.entry(b.to_string()).or_default().processing += 1;
                }
            }
        }
        let created = num(&d["receipt"]["created_at"]);
        let fails: Vec<&str> = d["results"].as_array().into_iter().flatten().filter(|r| r["status"] == "FAIL").filter_map(|r| r["rule_id"].as_str()).collect();
        let weak: Vec<&str> = d["results"].as_array().into_iter().flatten().filter(|r| r["status"] == "UNABLE_TO_ASSESS").filter_map(|r| r["rule_id"].as_str()).collect();
        if let Some(i) = created.and_then(slot) {
            series[i].0 += 1;
            series[i].2 += fails.len() as i64;
        }
        if in_window(created, since, now) {
            received += 1;
            violations += fails.len() as i64;
            unable += weak.len() as i64;
            for r in &fails {
                rules.entry((*r).to_string()).or_default().0 += 1;
            }
            for r in &weak {
                rules.entry((*r).to_string()).or_default().1 += 1;
            }
        }
        if DONE.contains(&state) {
            let at = num(&d["state_at"]);
            if let Some(i) = at.and_then(slot) {
                series[i].1 += 1;
            }
            if in_window(at, since, now) {
                processed += 1;
                if d["auto_cleared"] == true || d["decided_by"] == AUTO_REQUEST {
                    auto_cleared += 1;
                } else {
                    by_person += 1;
                }
            }
        }
        let id = d["claim_id"].as_str().unwrap_or("");
        for dec in d["decisions"].as_array().into_iter().flatten() {
            let (actor, at) = (dec["actor"].as_str().unwrap_or(""), num(&dec["at"]));
            if actor.is_empty() || actor == AUTO_REQUEST || !in_window(at, since, now) {
                continue;
            }
            let at = at.unwrap_or(since);
            let o = ops.entry(actor.to_string()).or_default();
            if o.decisions == 0 {
                (o.first, o.last) = (at, at);
            }
            o.first = o.first.min(at);
            o.last = o.last.max(at);
            o.decisions += 1;
            o.claims.insert(id.to_string());
            match dec["action"].as_str().unwrap_or("") {
                "confirm_issue" => o.confirmed += 1,
                "dismiss_with_reason" => o.dismissed += 1,
                "request_information" | "mark_corrected_for_recheck" => o.asked += 1,
                _ => {}
            }
        }
    }
    for s in on_shift {
        ops.entry(s.badge.clone()).or_default();
    }
    let inbox: BTreeMap<&str, i64> = on_shift.iter().map(|s| (s.badge.as_str(), s.inbox)).collect();
    let mut operators: Vec<Value> = ops
        .iter()
        .map(|(badge, o)| {
            let span_hours = ((o.last - o.first) / HOUR).max(1.0);
            let per_hour = if o.claims.is_empty() { 0.0 } else { (o.claims.len() as f64 / span_hours * 100.0).round() / 100.0 };
            json!({"badge_id": badge, "on_shift": inbox.contains_key(badge.as_str()), "processing_now": o.processing, "waiting_in_inbox": inbox.get(badge.as_str()).copied().unwrap_or(0),
                   "claims_processed": o.claims.len(), "decisions": o.decisions, "confirmed": o.confirmed, "dismissed": o.dismissed, "information_requested": o.asked,
                   "first_decision_at": (o.decisions > 0).then_some(o.first), "last_decision_at": (o.decisions > 0).then_some(o.last),
                   "claims_per_hour": per_hour})
        })
        .collect();
    operators.sort_by(|a, b| b["claims_processed"].as_i64().cmp(&a["claims_processed"].as_i64()).then_with(|| a["badge_id"].as_str().cmp(&b["badge_id"].as_str())));
    let worked: Vec<&Value> = operators.iter().filter(|o| o["decisions"].as_i64().unwrap_or(0) > 0).collect();
    let mut rule_rows: Vec<Value> = rules.into_iter().map(|(r, (v, u))| json!({"rule_id": r, "violated": v, "unable_to_assess": u})).collect();
    rule_rows.sort_by(|a, b| b["violated"].as_i64().cmp(&a["violated"].as_i64()).then_with(|| a["rule_id"].as_str().cmp(&b["rule_id"].as_str())));
    let hourly: Vec<Value> = series
        .iter()
        .enumerate()
        .map(|(i, (rx, px, vx))| json!({"hour_start": top - (SERIES_HOURS - 1 - i) as f64 * HOUR, "received": rx, "processed": px, "violations": vx}))
        .collect();
    let unprocessed: i64 = backlog.values().sum();
    let mut b = Map::new();
    for (k, v) in &backlog {
        b.insert((*k).to_string(), json!(v));
    }
    json!({
        "window": {"since": since, "until": now},
        "claims": {"received": received, "processed": processed, "processed_by_person": by_person, "processed_by_system": auto_cleared, "unprocessed": unprocessed, "backlog_by_state": b},
        "rule_violations": {"total": violations, "unable_to_assess": unable, "rules_violated": rule_rows.iter().filter(|r| r["violated"].as_i64().unwrap_or(0) > 0).count(), "by_rule": rule_rows},
        "operators": {"worked": worked.len(), "on_shift": on_shift.len(), "claims_processed_by_operators": by_person, "list": operators},
        "hourly": hourly,
        "definitions": {
            "received": "claims whose receipt was written inside the window",
            "processed": "claims that reached decided or rechecked inside the window",
            "unprocessed": "claims not yet decided at the moment the numbers were read, whenever they arrived",
            "violated": "a rule whose result on a received claim is FAIL (UNABLE_TO_ASSESS is counted separately)",
            "claims_per_hour": "claims the operator decided divided by the hours between their first and last decision, at least one hour"
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half past the hour, so "ten seconds ago" is still in the current hourly bucket.
    const NOW: f64 = 1_800_001_800.0;

    fn claim(id: &str, state: &str, created: f64, state_at: f64, fails: &[&str]) -> Value {
        json!({"claim_id": id, "state": state, "state_at": state_at, "receipt": {"created_at": created},
               "results": fails.iter().map(|r| json!({"rule_id": r, "status": "FAIL"})).chain([json!({"rule_id": "R-OK", "status": "PASS"})]).collect::<Vec<_>>(),
               "decisions": []})
    }

    fn decided(id: &str, who: &str, at: f64, action: &str) -> Value {
        let mut c = claim(id, "decided", at - 600.0, at, &["R-1"]);
        c["decisions"] = json!([{"rule_id": "R-1", "action": action, "actor": who, "at": at}]);
        c
    }

    #[test]
    fn counts_received_processed_and_backlog() {
        let since = NOW - 3.0 * HOUR;
        let docs = vec![
            decided("A", "CG-1", NOW - 1000.0, "confirm_issue"),
            claim("B", "ready", NOW - 2000.0, NOW - 1900.0, &["R-1", "R-2"]),
            claim("C", "leased", NOW - 500.0, NOW - 400.0, &[]),
            claim("OLD", "ready", since - 5.0 * HOUR, since - 4.0 * HOUR, &["R-9"]),
        ];
        let v = build(&docs, &[], since, NOW);
        assert_eq!(v["claims"]["received"], 3, "the old claim came in before the window");
        assert_eq!(v["claims"]["processed"], 1);
        assert_eq!(v["claims"]["unprocessed"], 3, "the backlog counts every undecided claim, old ones too");
        assert_eq!(v["rule_violations"]["total"], 3);
        assert_eq!(v["rule_violations"]["by_rule"][0]["rule_id"], "R-1");
        assert_eq!(v["rule_violations"]["by_rule"][0]["violated"], 2);
        assert!(v["rule_violations"]["by_rule"].as_array().unwrap().iter().all(|r| r["rule_id"] != "R-9"), "violations are counted for claims received in the window");
    }

    #[test]
    fn operators_are_counted_by_their_own_decisions() {
        let since = NOW - 6.0 * HOUR;
        let mut two = decided("B", "CG-1", NOW - 2.0 * HOUR, "dismiss_with_reason");
        two["decisions"].as_array_mut().unwrap().push(json!({"rule_id": "R-2", "action": "confirm_issue", "actor": "CG-1", "at": NOW - 2.0 * HOUR + 5.0}));
        let docs = vec![decided("A", "CG-1", NOW - 4.0 * HOUR, "confirm_issue"), two, decided("C", "CG-2", NOW - 100.0, "confirm_issue"), claim("D", "leased", NOW - 50.0, NOW - 40.0, &[])];
        let mut leased = docs.clone();
        leased[3]["lease"] = json!({"badge_id": "CG-2"});
        let v = build(&leased, &[OnShift { badge: "CG-3".into(), inbox: 4 }], since, NOW);
        assert_eq!(v["operators"]["worked"], 2);
        let list = v["operators"]["list"].as_array().unwrap();
        let find = |b: &str| list.iter().find(|o| o["badge_id"] == b).unwrap();
        assert_eq!(find("CG-1")["claims_processed"], 2);
        assert_eq!(find("CG-1")["decisions"], 3);
        assert_eq!(find("CG-1")["claims_per_hour"], 1.0, "two claims over a two-hour span");
        assert_eq!(find("CG-2")["claims_processed"], 1);
        assert_eq!(find("CG-2")["processing_now"], 1);
        assert_eq!(find("CG-3")["on_shift"], true, "someone on shift who has not decided anything still shows");
        assert_eq!(find("CG-3")["waiting_in_inbox"], 4);
        assert_eq!(find("CG-3")["claims_per_hour"], 0.0);
    }

    #[test]
    fn system_work_is_not_credited_to_a_person() {
        let mut auto = claim("A", "decided", NOW - 900.0, NOW - 800.0, &[]);
        auto["auto_cleared"] = json!(true);
        let mut asked = claim("B", "decided", NOW - 900.0, NOW - 800.0, &["R-1"]);
        asked["decided_by"] = json!(AUTO_REQUEST);
        asked["decisions"] = json!([{"rule_id": "R-1", "action": "request_information", "actor": AUTO_REQUEST, "at": NOW - 800.0}]);
        let v = build(&[auto, asked, decided("C", "CG-1", NOW - 700.0, "confirm_issue")], &[], NOW - HOUR, NOW);
        assert_eq!(v["claims"]["processed"], 3);
        assert_eq!(v["claims"]["processed_by_system"], 2);
        assert_eq!(v["claims"]["processed_by_person"], 1);
        assert_eq!(v["operators"]["list"].as_array().unwrap().len(), 1, "the system is not an operator");
    }

    #[test]
    fn hourly_series_has_24_buckets_ending_now() {
        let docs = vec![claim("A", "ready", NOW - 10.0, NOW - 5.0, &["R-1"]), decided("B", "CG-1", NOW - 2.0 * HOUR, "confirm_issue"), claim("OLD", "ready", NOW - 30.0 * HOUR, NOW - 29.0 * HOUR, &[])];
        let v = build(&docs, &[], NOW - 24.0 * HOUR, NOW);
        let h = v["hourly"].as_array().unwrap();
        assert_eq!(h.len(), 24);
        assert_eq!(h[23]["received"], 1);
        assert_eq!(h[23]["violations"], 1);
        assert_eq!(h.iter().map(|b| b["received"].as_i64().unwrap()).sum::<i64>(), 2, "B was received two hours ago; the 30-hour-old claim falls outside the series");
        assert_eq!(h.iter().map(|b| b["processed"].as_i64().unwrap()).sum::<i64>(), 1);
        assert!(h[0]["hour_start"].as_f64().unwrap() < h[23]["hour_start"].as_f64().unwrap());
    }

    #[test]
    fn empty_day_is_all_zeros() {
        let v = build(&[], &[], NOW - HOUR, NOW);
        assert_eq!(v["claims"]["received"], 0);
        assert_eq!(v["claims"]["unprocessed"], 0);
        assert_eq!(v["operators"]["worked"], 0);
        assert!(v["rule_violations"]["by_rule"].as_array().unwrap().is_empty());
    }
}
