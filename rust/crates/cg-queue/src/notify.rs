//! Notifications for the console: what an officer should hear about without having to look.
//!
//! Three things are announced: a claim that has just been put in someone's inbox (to that person), a request for information the system made
//! on its own (to every officer, because it never reaches an inbox), and a claim whose lease ran out and went back to the pool (to the person
//! who held it). A notification names a claim id, rule ids and counts and says what happened; it holds no claim value and no patient
//! identifier, so showing one can never show more than the officer could already open. Writing is best effort: a failed notification is
//! dropped and never stops the claim from moving.

use crate::store::QueueStore;
use cg_access::settings::random_token;
use serde_json::{json, Value};

/// How many of a person's notifications one read returns.
pub const LIST_LIMIT: usize = 100;
/// Everyone who may decide claims (the `to` of a broadcast).
pub const EVERYONE: &str = "*";

fn push(store: &dyn QueueStore, to: &str, kind: &str, claim_id: &str, title: String, summary: String, lines: Vec<(&str, String)>, now: f64) {
    let lines: Vec<Value> = lines.into_iter().map(|(k, v)| json!([k, v])).collect();
    let _ = store.append_notification(&json!({"id": random_token(12), "to": to, "kind": kind, "claim_id": claim_id, "title": title, "summary": summary, "lines": lines, "at": now}));
}

fn flagged_rules(doc: &Value) -> Vec<String> {
    doc["results"].as_array().into_iter().flatten().filter(|r| matches!(r["status"].as_str(), Some("FAIL") | Some("UNABLE_TO_ASSESS"))).filter_map(|r| r["rule_id"].as_str().map(str::to_string)).collect()
}

/// A claim was put in `badge`'s inbox.
pub fn claim_arrived(store: &dyn QueueStore, badge: &str, doc: &Value, expires_at: f64, now: f64) {
    let claim_id = doc["claim_id"].as_str().unwrap_or("");
    let flagged = flagged_rules(doc);
    let stage = doc["signoff"]["stage"].as_str();
    let why = match stage {
        Some("countersign") => "It needs your countersignature: another senior already decided its high-severity findings.",
        Some("tiebreak") => "Two seniors disagreed, so it needs your final decision.",
        _ if doc["escalated"] == true => "It was escalated to a senior reviewer.",
        _ => "It was dealt to you from the queue.",
    };
    let minutes = ((expires_at - now) / 60.0).round().max(0.0) as i64;
    let mut lines = vec![("Claim", claim_id.to_string()), ("Lane", doc["receipt"]["lane"].as_str().unwrap_or("-").to_string()), ("Risk score", doc["receipt"]["score"].to_string()),
                         ("Flagged findings", if flagged.is_empty() { "none (a clean claim to verify)".into() } else { flagged.join(", ") }), ("Why you", why.to_string())];
    lines.push(("Hold time", format!("{minutes} minutes; keep the claim open or it goes back to the pool")));
    push(store, badge, "claim_arrived", claim_id, format!("New claim {claim_id}"), format!("{} flagged finding{}. {why}", flagged.len(), if flagged.len() == 1 { "" } else { "s" }), lines, now);
}

/// The system asked for missing information on its own. `rows` are the findings it asked about; `finished` is true when nothing else is left for
/// a person to do (the claim now waits for a corrected submission), false when the rest of the claim still goes to an officer.
pub fn info_requested(store: &dyn QueueStore, claim_id: &str, rows: &[&Value], finished: bool, now: f64) {
    let rules: Vec<String> = rows.iter().filter_map(|r| r["rule_id"].as_str().map(str::to_string)).collect();
    let n = rules.len();
    let mut lines = vec![("Claim", claim_id.to_string()), ("Asked about", rules.join(", "))];
    for r in rows.iter().take(5) {
        let step: String = r["corrective_action"].as_str().unwrap_or("").trim().chars().take(240).collect();
        if !step.is_empty() {
            lines.push(("What was asked", format!("{}: {step}", r["rule_id"].as_str().unwrap_or(""))));
        }
    }
    lines.push(("Done by", "the pipeline, not a person (system:auto-request)".to_string()));
    lines.push(("What happens next", if finished { "Nothing is left for an officer: the claim waits for a corrected submission." } else { "The remaining findings of this claim still go to an officer." }.to_string()));
    push(store, EVERYONE, "info_requested_auto", claim_id, format!("More information requested for {claim_id}"),
         format!("The system asked for missing data on {n} finding{} by itself.", if n == 1 { "" } else { "s" }), lines, now);
}

/// A claim's lease ran out and it went back to the pool.
pub fn lease_expired(store: &dyn QueueStore, badge: &str, claim_id: &str, now: f64) {
    push(store, badge, "lease_expired", claim_id, format!("Claim {claim_id} went back to the pool"), "You did not finish it in time, so someone else may get it.".to_string(),
         vec![("Claim", claim_id.to_string()), ("What happened", "Your hold on the claim ran out (a lease lasts only while you keep it open).".to_string()), ("What to do", "Nothing. Any decisions you already recorded are kept.".to_string())], now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryQueueStore;

    #[test]
    fn personal_ones_are_private_broadcasts_are_cleared_per_person_and_clearing_never_touches_anyone_else() {
        let s = MemoryQueueStore::new();
        lease_expired(&s, "CG-2002", "C1", 10.0);
        lease_expired(&s, "CG-2003", "C2", 11.0);
        info_requested(&s, "C3", &[&json!({"rule_id": "R009", "corrective_action": "send the authorization"})], true, 12.0);
        let ids = |b: &str| s.notifications(b, 50).unwrap().iter().map(|n| n["claim_id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids("CG-2002"), ["C3", "C1"], "newest first, its own plus the broadcast");
        assert_eq!(ids("CG-2003"), ["C3", "C2"]);
        assert_eq!(s.clear_notifications("CG-2002", 20.0).unwrap(), 2);
        assert!(ids("CG-2002").is_empty());
        assert_eq!(ids("CG-2003"), ["C3", "C2"], "someone else's clearing changes nothing for me");
        info_requested(&s, "C4", &[], false, 21.0);
        assert_eq!(ids("CG-2002"), ["C4"], "a broadcast after the clearing shows again");
        assert_eq!(s.erase_claim("C4").unwrap(), 0);
        assert!(ids("CG-2002").is_empty() && !ids("CG-2003").contains(&"C4".to_string()), "erasing a claim erases what names it");
        assert!(s.append_notification(&json!({"to": "x"})).is_err(), "a malformed notification is refused");
    }
}
