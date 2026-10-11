//! The claim state machine (`src/workqueue/states.py`). Every move is one append-only event, and a move from the wrong state is refused.
//!
//! ```text
//! received -> triaged -> explained | explanation_skipped -> ready -> leased -> decided -> rechecked -> triaged
//! ```
//!
//! Green claims go triaged -> ready directly; leased -> ready is a lease expiring or being handed back; a claim with a high-severity
//! finding is signed by one senior (leased -> awaiting_countersign) and then leased again to a different senior, who countersigns
//! (-> decided) or disagrees (-> ready, escalated, for a third senior); dead_lettered is the side state of a task that failed its
//! retries (an administrator replays it back to triaged).

use serde_json::{json, Value};

pub const STATES: [&str; 10] = [
    "received", "triaged", "explained", "explanation_skipped", "ready", "leased", "awaiting_countersign", "decided", "rechecked", "dead_lettered",
];
pub const MAX_DETAIL_DEPTH: usize = 3;

pub fn allowed_from(state: &str) -> &'static [&'static str] {
    match state {
        "received" => &["triaged", "dead_lettered"],
        "triaged" => &["explained", "explanation_skipped", "ready", "dead_lettered"],
        "explained" => &["ready", "dead_lettered"],
        "explanation_skipped" => &["ready", "dead_lettered"],
        "ready" => &["leased", "dead_lettered"],
        "leased" => &["ready", "decided", "awaiting_countersign", "dead_lettered"],
        "awaiting_countersign" => &["leased", "dead_lettered"],
        "decided" => &["rechecked"],
        "rechecked" => &["triaged"],
        "dead_lettered" => &["triaged"],
        _ => &[],
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct IllegalTransition(pub String);

pub fn check_transition(from: &str, to: &str) -> Result<(), IllegalTransition> {
    if allowed_from(from).contains(&to) {
        Ok(())
    } else {
        Err(IllegalTransition(format!("a claim cannot move from {from:?} to {to:?}")))
    }
}

/// Small plain data only: text, numbers, booleans, lists and objects, nested at most 3 deep.
pub fn check_detail(value: &Value) -> Result<(), String> {
    fn walk(v: &Value, depth: usize) -> Result<(), String> {
        match v {
            Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
            Value::Number(n) => {
                if n.as_f64().is_some_and(|f| !f.is_finite()) {
                    Err("detail numbers must be finite".into())
                } else {
                    Ok(())
                }
            }
            Value::Array(items) => {
                if depth >= MAX_DETAIL_DEPTH {
                    return Err("detail is nested too deeply".into());
                }
                items.iter().try_for_each(|i| walk(i, depth + 1))
            }
            Value::Object(map) => {
                if depth >= MAX_DETAIL_DEPTH {
                    return Err("detail is nested too deeply".into());
                }
                map.values().try_for_each(|i| walk(i, depth + 1))
            }
        }
    }
    walk(value, 0)
}

pub fn make_event(from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Value, String> {
    check_transition(from, to).map_err(|e| e.0)?;
    if actor.is_empty() {
        return Err("the actor must be a badge or a system worker name".into());
    }
    if !now.is_finite() {
        return Err("the time must be a number".into());
    }
    let detail = detail.unwrap_or_else(|| json!({}));
    if !detail.is_object() {
        return Err("detail must be an object".into());
    }
    check_detail(&detail)?;
    Ok(json!({"from": from, "to": to, "actor": actor, "at": now, "detail": detail}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_happy_path_and_the_side_paths_are_legal() {
        for (a, b) in [("received", "triaged"), ("triaged", "ready"), ("triaged", "explained"), ("explained", "ready"), ("ready", "leased"), ("leased", "decided"),
                       ("leased", "awaiting_countersign"), ("awaiting_countersign", "leased"), ("leased", "ready"), ("decided", "rechecked"), ("rechecked", "triaged"),
                       ("dead_lettered", "triaged")] {
            assert!(check_transition(a, b).is_ok(), "{a} -> {b}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for from in STATES {
            for to in STATES {
                let legal = allowed_from(from).contains(&to);
                assert_eq!(check_transition(from, to).is_ok(), legal);
            }
        }
        assert!(check_transition("decided", "leased").is_err() && check_transition("ready", "decided").is_err() && check_transition("nope", "ready").is_err());
        assert!(STATES.iter().all(|s| allowed_from(s).iter().all(|t| STATES.contains(t))), "every target is a known state");
    }

    #[test]
    fn events_validate_their_inputs() {
        assert!(make_event("ready", "leased", "CG-2002", 1.0, Some(json!({"expires_at": 5.0}))).is_ok());
        assert!(make_event("ready", "decided", "CG-2002", 1.0, None).is_err());
        assert!(make_event("ready", "leased", "", 1.0, None).is_err());
        assert!(make_event("ready", "leased", "a", f64::NAN, None).is_err());
        assert!(make_event("ready", "leased", "a", 1.0, Some(json!([1]))).is_err());
        assert!(make_event("ready", "leased", "a", 1.0, Some(json!({"a": {"b": {"c": {"d": 1}}}}))).is_err(), "too deep");
        assert!(make_event("ready", "leased", "a", 1.0, Some(json!({"a": {"b": {"c": 1}}}))).is_ok());
    }
}
