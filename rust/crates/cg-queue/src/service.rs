//! The queue service: what an agent or an administrator may do with the queue. No HTTP in here (`src/workqueue/service.py`).
//!
//! Rules enforced in this file, not in the web layer:
//! * a person decides only claims leased to them, and only while the lease runs; permissions are read from the user store again at the
//!   moment of a decision (what the session token said when the request began is not trusted);
//! * a decision is written to the claim by one conditional database update that checks the lease holder and its expiry, so a decision that
//!   loses a race with an expiry or a re-deal is refused and writes nothing;
//! * nobody assigns a particular claim to a particular person: the administrator changes capacity, shifts and the formula's numbers through
//!   the versioned configuration, and the dispatcher deals;
//! * level 4 holds `queue.view` and `routing.manage` but not `claims.decide`, so whoever runs the queue cannot decide its claims;
//! * a claim with a high-severity finding needs two different seniors: the first resolves every finding and the claim waits for a
//!   countersignature; a second senior (never the first) decides the high-severity findings again without seeing the first's answer.
//!   Agreeing decides the claim; disagreeing sends it, escalated, to a third senior whose decision is final.

use crate::dispatcher::{Agent, Dispatcher};
use crate::flow::Intake;
use crate::routing_config::{self as rc, RoutingConfig};
use crate::store::QueueStore;
use crate::{shadow, triage};
use cg_access::masking;
use cg_access::permissions::{self, Perms};
use cg_access::securitylog::SecurityLog;
use cg_access::service::{AccessService, AuthError, Principal};
use cg_access::Clock;
use cg_audit::AuditLog;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const REVIEWABLE: [&str; 2] = ["FAIL", "UNABLE_TO_ASSESS"];
pub const RESOLVING: [&str; 2] = ["confirm_issue", "dismiss_with_reason"];
pub const GREEN_ACTIONS: [&str; 2] = ["verify_clear", "escalate"];
const ACTIONS: [&str; 4] = ["confirm_issue", "dismiss_with_reason", "request_information", "mark_corrected_for_recheck"];
const LOG_VALUE_CHARS: usize = 400;
pub const MAX_SUBMIT: usize = 25;
/// Recorded when a person confirms a finding without typing a reason: confirming agrees with the engine, so only overriding it needs words.
pub const CONFIRM_REASON: &str = "Confirmed: the finding matches the evidence.";
/// Recorded on a finding confirmed in bulk after a light review. Such decisions are left out of the reliability evidence (`reliability::counted_decisions`).
pub const BULK_REASON: &str = "Confirmed in bulk after a light review.";
const LIGHT_TTL: f64 = 300.0;
const CONFIG_FIELDS: [&str; 15] = ["points", "lane_b_flagged", "lane_b_score", "slice_size", "low_water", "lease_seconds", "aging_per_hour", "ai_per_minute", "ai_daily_budget", "on_shift", "exclusions", "auto_clear_green", "audit_percent", "bulk_confirm_skip_lower", "auto_request_info"];

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The request is valid but the claim or configuration is no longer in the state it assumed (HTTP 409).
    #[error("conflict: {0}")]
    Conflict(String),
    /// No such claim, or not one of yours (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("an identifier survived masking")]
    Redaction,
}

type R<T> = std::result::Result<T, QueueError>;

fn store_err(e: cg_access::store::StoreError) -> QueueError {
    match e {
        cg_access::store::StoreError::Unavailable => AuthError::Unavailable.into(),
        other => QueueError::Invalid(other.to_string()),
    }
}

/// `agents()` for the dispatcher, read from the user store on every call so a demotion applies at once.
pub fn agents_from_access(access: Arc<AccessService>) -> Arc<dyn Fn() -> Vec<Agent> + Send + Sync> {
    Arc::new(move || {
        access
            .store
            .list_users()
            .unwrap_or_default()
            .into_iter()
            .map(|u| match permissions::effective_permissions(u.level, &u.grants, &u.revokes) {
                Ok(p) => Agent { badge_id: u.badge_id, permissions: p.into_iter().map(str::to_string).collect(), active: u.active },
                Err(_) => Agent { badge_id: u.badge_id, permissions: BTreeSet::new(), active: false }, // a corrupt record gets no work
            })
            .collect()
    })
}

fn iso(ts: f64) -> String {
    cg_audit::iso_of(Duration::from_secs_f64(ts.max(0.0)))
}

fn stage(doc: &Value) -> Option<&str> {
    doc["signoff"]["stage"].as_str()
}

fn round(doc: &Value) -> i64 {
    match stage(doc) {
        None => 1,
        Some("countersign") => 2,
        Some("tiebreak") => 3,
        Some(_) => 1,
    }
}

/// True when the system itself recorded the request for information on this rule (`flow::request_decisions`): a person does not decide it.
fn auto_requested(doc: &Value, rule: &str) -> bool {
    doc["decisions"].as_array().into_iter().flatten().any(|d| d["rule_id"] == rule && d["actor"] == crate::flow::AUTO_REQUEST)
}

fn is_high(finding: &Value) -> bool {
    finding["severity"] != "medium" // an unknown severity counts as high, as in triage
}

fn flagged_rows(doc: &Value) -> Vec<&Value> {
    doc["results"].as_array().into_iter().flatten().filter(|r| r["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s))).collect()
}

/// The rule ids still to be decided in this claim's current round: every flagged finding in round one, then only the high-severity ones
/// (the medium ones were settled by one person).
fn targets(doc: &Value) -> Vec<String> {
    flagged_rows(doc).into_iter().filter(|r| stage(doc).is_none() || is_high(r)).filter(|r| !auto_requested(doc, r["rule_id"].as_str().unwrap_or(""))).filter_map(|r| r["rule_id"].as_str().map(str::to_string)).collect()
}

/// The latest action per rule, from all rounds or from one.
fn latest_actions(doc: &Value, rnd: Option<i64>) -> BTreeMap<String, String> {
    let mut latest = BTreeMap::new();
    for d in doc["decisions"].as_array().into_iter().flatten() {
        if rnd.map_or(true, |r| d["round"].as_i64().unwrap_or(1) == r) {
            latest.insert(d["rule_id"].as_str().unwrap_or("").to_string(), d["action"].as_str().unwrap_or("").to_string());
        }
    }
    latest
}

fn short(v: &Value) -> Value {
    let text = cg_audit::canonical::canonical(v);
    if text.chars().count() <= LOG_VALUE_CHARS { Value::String(text) } else { Value::String(format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))) }
}

fn resolving(a: &str) -> bool {
    RESOLVING.contains(&a)
}

pub type RulesFn = Arc<dyn Fn() -> Arc<cg_core::config::Config> + Send + Sync>;

pub struct QueueService {
    pub store: Arc<dyn QueueStore>,
    pub dispatcher: Arc<Dispatcher>,
    pub access: Arc<AccessService>,
    pub securitylog: Arc<SecurityLog>,
    pub review_log: Arc<AuditLog>,
    pub clock: Clock,
    intake: Mutex<Option<Arc<Intake>>>,
    /// The rulebook in force now (policy tables, service catalogue), for the copilot's suggestions; None switches the copilot off.
    rules: Mutex<Option<RulesFn>>,
    /// (computed at, skip_lower it was computed for, rules): the light-review set is a read over every decided claim, so it is kept for five minutes.
    light: Mutex<Option<(f64, f64, BTreeSet<String>)>>,
}

impl QueueService {
    pub fn new(store: Arc<dyn QueueStore>, dispatcher: Arc<Dispatcher>, access: Arc<AccessService>, securitylog: Arc<SecurityLog>, review_log: Arc<AuditLog>, clock: Clock) -> QueueService {
        QueueService { store, dispatcher, access, securitylog, review_log, clock, intake: Mutex::new(None), rules: Mutex::new(None), light: Mutex::new(None) }
    }

    /// Set by the wiring; only the submit route needs it.
    pub fn set_rules(&self, rules: RulesFn) {
        *self.rules.lock().unwrap_or_else(|e| e.into_inner()) = Some(rules);
    }

    fn rules_now(&self) -> Option<Arc<cg_core::config::Config>> {
        self.rules.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|f| f())
    }

    fn intake_now(&self) -> R<Arc<Intake>> {
        self.intake.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| QueueError::Invalid("this server was started without a claim intake".into()))
    }

    pub fn set_intake(&self, intake: Arc<Intake>) {
        *self.intake.lock().unwrap_or_else(|e| e.into_inner()) = Some(intake);
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- permissions and ownership ------------------------------------------------------------------------------------
    fn fresh(&self, principal: &Principal) -> R<Perms> {
        match self.access.store.get_user(principal.badge()) {
            Err(_) => Err(AuthError::Unavailable.into()),
            Ok(None) => Ok(Perms::new()),
            Ok(Some(u)) if !u.active => Ok(Perms::new()),
            Ok(Some(u)) => Ok(permissions::effective_permissions(u.level, &u.grants, &u.revokes).unwrap_or_default()),
        }
    }

    fn require(&self, principal: &Principal, permission: &str) -> R<Perms> {
        let perms = self.fresh(principal)?;
        if perms.contains(permission) { Ok(perms) } else { Err(QueueError::Forbidden(permission.to_string())) }
    }

    /// The latest version of the claim, if it is leased to this person right now; otherwise NotFound or Conflict.
    fn mine(&self, principal: &Principal, claim_id: &str, now: f64) -> R<Value> {
        let doc = self.store.get(claim_id, None).map_err(store_err)?.ok_or_else(|| QueueError::NotFound(claim_id.into()))?;
        if doc["state"] == "leased" && doc["lease"]["badge_id"] == principal.badge() {
            if doc["lease"]["expires_at"].as_f64().unwrap_or(0.0) <= now {
                return Err(QueueError::Conflict("lease_expired".into()));
            }
            return Ok(doc);
        }
        let held_before = doc["events"].as_array().into_iter().flatten().any(|e| {
            ["leased", "ready", "decided"].contains(&e["to"].as_str().unwrap_or("")) && (e["actor"] == principal.badge() || e["detail"]["badge_id"] == principal.badge())
        });
        if held_before { Err(QueueError::Conflict("lease_lost".into())) } else { Err(QueueError::NotFound(claim_id.into())) }
    }

    pub fn routing_config(&self) -> RoutingConfig {
        crate::flow::routing_config(&*self.store)
    }

    // ---- the agent's side ---------------------------------------------------------------------------------------------
    fn actions(perms: &Perms, finding: &Value, latest: &BTreeMap<String, String>) -> Option<Vec<&'static str>> {
        if !finding["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) || !perms.contains("claims.decide") {
            return None;
        }
        if latest.get(finding["rule_id"].as_str().unwrap_or("")).is_some_and(|a| resolving(a)) {
            return None;
        }
        if finding["severity"] != "medium" && !perms.contains("claims.decide_high") {
            return None;
        }
        Some(ACTIONS.to_vec())
    }

    fn view(&self, perms: &Perms, doc: &Value) -> R<Value> {
        let key = &self.access.settings.pii_key;
        let (claim, shaped) = masking::shape_claim(&doc["claim"], &doc["results"], perms, key, false, true).map_err(|_| QueueError::Redaction)?;
        let mapping = masking::identifier_map(&doc["claim"], key).map_err(|_| QueueError::Redaction)?;
        let texts = &doc["explanation"]["findings"];
        let rnd = round(doc);
        let tgt: BTreeSet<String> = targets(doc).into_iter().collect();
        let latest = latest_actions(doc, Some(rnd));
        let raw: BTreeMap<&str, &Value> = doc["results"].as_array().into_iter().flatten().filter_map(|r| r["rule_id"].as_str().map(|id| (id, r))).collect();
        let mut findings = vec![];
        for r in shaped.as_array().into_iter().flatten() {
            let mut row = r.clone();
            let id = r["rule_id"].as_str().unwrap_or("");
            let original = raw.get(id).copied().unwrap_or(r);
            if original["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {
                if texts.get(id).is_some() {
                    row["ai_explanation"] = json!({"text": masking::scrub(&texts[id]["text"], &mapping), "source": texts[id]["source"]});
                }
                if tgt.contains(id) {
                    let action = latest.get(id);
                    row["review_state"] = json!(match action { None => "unreviewed", Some(a) if resolving(a) => "resolved", _ => "awaiting_follow_up" });
                    if let Some(actions) = Self::actions(perms, original, &latest) {
                        row["allowed_actions"] = json!(actions);
                    }
                } else {
                    row["review_state"] = json!(if auto_requested(doc, id) { "info_requested_automatically" } else { "resolved" }); // else: settled in an earlier round
                }
            }
            findings.push(row);
        }
        let receipt = &doc["receipt"];
        let mut view = json!({"claim_id": doc["claim_id"], "version": doc["version"], "lane": receipt["lane"], "score": receipt["score"], "eligibility": receipt["eligibility"],
            "escalated": doc["escalated"] == true, "signoff_stage": stage(doc), "leased_until": doc["lease"]["expires_at"], "claim": claim, "findings": findings,
            "in_batch": doc["batch_id"].is_string()});
        if doc["advisory"].as_array().is_some_and(|a| !a.is_empty()) {
            // advisory (extension) results: shown for information, shaped by the same masking, and never offered as decidable
            view["advisory"] = masking::shape_claim(&doc["claim"], &doc["advisory"], perms, key, false, true).map_err(|_| QueueError::Redaction)?.1;
        }
        if triage::flagged(&doc["results"]).is_empty() && perms.contains("claims.decide") {
            view["allowed_actions"] = json!(GREEN_ACTIONS);
        }
        if let Some(cfg) = self.rules_now() {
            let bundle = crate::copilot::bundle(&doc["claim"], &doc["results"], &cfg);
            if let Some(draft) = self.drafted_request(doc, bundle["suggestions"].as_array().map(Vec::as_slice).unwrap_or(&[])) {
                view["info_request"] = draft;
            }
            view["copilot"] = bundle;
        }
        if perms.contains("claims.recheck") && stage(doc).is_none() {
            view["can_correct"] = json!(true);
        }
        if doc.get("corrected_from").is_some_and(|c| !c.is_null()) {
            view["corrected_from"] = doc["corrected_from"].clone();
        }
        if !masking::leaks(&view, &mapping).is_empty() {
            return Err(QueueError::Redaction);
        }
        Ok(view)
    }

    pub fn inbox(&self, principal: &Principal) -> R<Vec<Value>> {
        let perms = self.require(principal, "claims.decide")?;
        self.store.inbox(principal.badge()).map_err(store_err)?.iter().map(|d| self.view(&perms, d)).collect()
    }

    pub fn next(&self, principal: &Principal) -> R<Option<Value>> {
        let perms = self.require(principal, "claims.decide")?;
        self.dispatcher.next_for(principal.badge()).map(|d| self.view(&perms, &d)).transpose()
    }

    pub fn heartbeat(&self, principal: &Principal) -> R<i64> {
        self.require(principal, "claims.decide")?;
        let now = self.now();
        self.store.heartbeat(principal.badge(), now, now + self.routing_config().lease_seconds as f64).map_err(store_err)
    }

    // ---- decisions ----------------------------------------------------------------------------------------------------
    pub fn decide_finding(&self, principal: &Principal, claim_id: &str, rule_id: &str, action: &str, reason: &str) -> R<Value> {
        let reason = if action == "confirm_issue" && cg_core::text::blank(reason) { CONFIRM_REASON } else { reason };
        let now = self.now();
        let perms = self.require(principal, "claims.decide")?;
        let doc = self.mine(principal, claim_id, now)?;
        let sign = &doc["signoff"];
        if sign["first_by"] == principal.badge() || sign["second_by"] == principal.badge() {
            return Err(QueueError::Conflict("same_person".into())); // the first signer never countersigns, nor the second the tie-break
        }
        let finding = doc["results"].as_array().into_iter().flatten().find(|r| r["rule_id"] == rule_id).ok_or_else(|| QueueError::NotFound(rule_id.into()))?;
        if auto_requested(&doc, rule_id) {
            return Err(QueueError::Invalid("information was requested automatically for this finding".into()));
        }
        if masking::text_leaks_identifier(reason, &doc["claim"], &self.access.settings.pii_key) {
            return Err(QueueError::Invalid("the reason must not contain the patient or member identifier".into()));
        }
        if !finding["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {
            return Err(QueueError::Invalid("only a flagged finding can be decided".into()));
        }
        if !targets(&doc).iter().any(|t| t == rule_id) {
            return Err(QueueError::Invalid("this finding was settled in an earlier round; only high-severity findings are countersigned".into()));
        }
        if is_high(finding) && !perms.contains("claims.decide_high") {
            return Err(QueueError::Forbidden("claims.decide_high".into()));
        }
        let rnd = round(&doc);
        if latest_actions(&doc, Some(rnd)).get(rule_id).is_some_and(|a| resolving(a)) {
            return Err(QueueError::Conflict("already_decided".into()));
        }
        let decision = json!({"claim_id": claim_id, "rule_id": rule_id, "action": action, "actor": principal.badge(), "reason": reason, "created_at": iso(now), "original_status": finding["status"]});
        validate_decision(&decision, finding).map_err(|e| QueueError::Invalid(e.chars().take(300).collect()))?;
        let updated = self
            .store
            .add_decision(claim_id, doc["version"].as_i64().unwrap_or(0), principal.badge(), now, &json!({"rule_id": rule_id, "action": action, "actor": principal.badge(), "reason": reason, "at": now, "round": rnd}))
            .map_err(store_err)?
            .ok_or_else(|| QueueError::Conflict("lease_lost".into()))?;
        self.review_log.append_review_decisions(&[decision]).map_err(|_| AuthError::Unavailable)?;
        self.access.record("decision", json!({"badge_id": principal.badge(), "claim_id": claim_id, "rule_id": rule_id, "action": action}))?;
        let (tgt, latest) = (targets(&updated), latest_actions(&updated, Some(rnd)));
        let mut state = "leased".to_string();
        if tgt.iter().all(|rid| latest.get(rid).is_some_and(|a| resolving(a))) {
            state = self.finish(principal, &updated, rnd, &tgt, &latest, now)?;
        }
        Ok(json!({"recorded": true, "claim_state": state, "review_state": if resolving(action) { "resolved" } else { "awaiting_follow_up" }}))
    }

    fn move_to(&self, doc: &Value, to: &str, principal: &Principal, now: f64, set: Value, detail: Value) -> R<String> {
        let set: Map<String, Value> = set.as_object().cloned().unwrap_or_default();
        let moved = self.store.transition(doc["claim_id"].as_str().unwrap_or(""), doc["version"].as_i64().unwrap_or(0), "leased", to, principal.badge(), now, Some(detail), Some(set), Some(principal.badge())).map_err(store_err)?;
        moved.map(|_| to.to_string()).ok_or_else(|| QueueError::Conflict("lease_lost".into()))
    }

    fn signed(&self, doc: &Value, principal: &Principal, stage: &str, outcome: &str) -> R<()> {
        Ok(self.access.record("claim_signoff", json!({"claim_id": doc["claim_id"], "badge_id": principal.badge(), "stage": stage, "outcome": outcome}))?)
    }

    /// Every finding of this round is resolved: decide the claim, ask for a countersignature, or settle a disagreement.
    fn finish(&self, principal: &Principal, doc: &Value, rnd: i64, tgt: &[String], latest: &BTreeMap<String, String>, now: f64) -> R<String> {
        let badge = principal.badge();
        let sign = if doc["signoff"].is_null() { json!({}) } else { doc["signoff"].clone() };
        let high: BTreeSet<String> = flagged_rows(doc).into_iter().filter(|r| is_high(r)).filter(|r| !auto_requested(doc, r["rule_id"].as_str().unwrap_or(""))).filter_map(|r| r["rule_id"].as_str().map(str::to_string)).collect();
        let detail = json!({"findings": tgt.len()});
        if rnd == 1 {
            if high.is_empty() {
                return self.move_to(doc, "decided", principal, now, json!({"decided_by": badge}), detail);
            }
            let actions: Map<String, Value> = high.iter().map(|rid| (rid.clone(), json!(latest.get(rid)))).collect();
            let signoff = json!({"stage": "countersign", "first_by": badge, "actions": actions});
            let mut d = detail.clone();
            d["event"] = json!("first_signature");
            let state = self.move_to(doc, "awaiting_countersign", principal, now, json!({"lease": null, "signoff": signoff}), d)?;
            self.signed(doc, principal, "first", "pending")?;
            return Ok(state);
        }
        if rnd == 2 {
            let agreed = tgt.iter().all(|rid| json!(latest.get(rid)) == sign["actions"][rid]);
            if agreed {
                let mut s = sign.clone();
                s["stage"] = json!("done");
                s["second_by"] = json!(badge);
                s["outcome"] = json!("agreed");
                let state = self.move_to(doc, "decided", principal, now, json!({"decided_by": badge, "signoff": s}), detail)?;
                self.signed(doc, principal, "countersign", "agreed")?;
                return Ok(state);
            }
            let second: Map<String, Value> = tgt.iter().collect::<BTreeSet<_>>().into_iter().map(|rid| (rid.clone(), json!(latest.get(rid)))).collect();
            let mut s = sign.clone();
            s["stage"] = json!("tiebreak");
            s["second_by"] = json!(badge);
            s["outcome"] = json!("disagreed");
            s["second_actions"] = Value::Object(second);
            let state = self.move_to(doc, "ready", principal, now, json!({"lease": null, "escalated": true, "signoff": s}), json!({"event": "signoff_disagreed", "badge_id": badge, "first_by": sign["first_by"].as_str().unwrap_or("")}))?;
            self.signed(doc, principal, "countersign", "disagreed")?;
            return Ok(state);
        }
        let mut s = sign.clone();
        s["stage"] = json!("done");
        s["third_by"] = json!(badge);
        s["outcome"] = json!("tiebreak");
        let state = self.move_to(doc, "decided", principal, now, json!({"decided_by": badge, "signoff": s}), detail)?;
        self.signed(doc, principal, "tiebreak", "final")?;
        Ok(state)
    }

    pub fn decide_green(&self, principal: &Principal, claim_id: &str, action: &str) -> R<Value> {
        self.decide_green_in(principal, claim_id, action, None, None)
    }

    /// A green claim, decided by an officer. For a claim of a batch, `escalate` means "the engine missed something": the officer says which rule should have
    /// caught it (`category`: R001 to R015 or "other") and the batch is quarantined at once (docs/53). The note stays in the batch history; the administrator never sees it.
    pub fn decide_green_in(&self, principal: &Principal, claim_id: &str, action: &str, category: Option<&str>, note: Option<&str>) -> R<Value> {
        let now = self.now();
        self.require(principal, "claims.decide")?;
        if !GREEN_ACTIONS.contains(&action) {
            return Err(QueueError::Invalid("action must be verify_clear or escalate".into()));
        }
        let doc = self.mine(principal, claim_id, now)?;
        if !triage::flagged(&doc["results"]).is_empty() {
            return Err(QueueError::Invalid("this claim has findings to decide one by one".into()));
        }
        let batch = doc["batch_id"].as_str().map(str::to_string);
        let category = category.map(str::trim).filter(|c| !c.is_empty());
        if action == "escalate" && batch.is_some() && !category.is_some_and(crate::batch::is_category) {
            return Err(QueueError::Invalid("say which rule should have flagged this claim (R001 to R015), or other".into()));
        }
        let note: String = note.unwrap_or("").trim().chars().take(crate::batch::NOTE_CHARS).collect();
        let version = doc["version"].as_i64().unwrap_or(0);
        let lease_lost = || QueueError::Conflict("lease_lost".into());
        if action == "escalate" {
            if let Some(b) = &batch {
                // written first: if anything after it fails the batch is held, which is the safe direction
                self.flag_batch(b, claim_id, category.unwrap_or("other"), &note, principal.badge(), now)?;
            }
        }
        if action == "verify_clear" {
            let mut set = Map::new();
            set.insert("decided_by".into(), json!(principal.badge()));
            self.store.transition(claim_id, version, "leased", "decided", principal.badge(), now, Some(json!({"findings": 0})), Some(set), Some(principal.badge())).map_err(store_err)?.ok_or_else(lease_lost)?;
            self.access.record("claim_decided_green", json!({"badge_id": principal.badge(), "claim_id": claim_id, "action": "verify_clear"}))?;
            if let Some(b) = &batch {
                let _ = self.store.append_batch_event(&json!({"batch_id": b, "kind": "sample_verified", "at": now, "claim_id": claim_id, "by": principal.badge()}));
            }
            return Ok(json!({"claim_state": "decided"}));
        }
        let mut set = Map::new();
        set.insert("lease".into(), Value::Null);
        set.insert("escalated".into(), json!(true));
        self.store.transition(claim_id, version, "leased", "ready", principal.badge(), now, Some(json!({"event": "escalated", "badge_id": principal.badge()})), Some(set), Some(principal.badge())).map_err(store_err)?.ok_or_else(lease_lost)?;
        self.access.record("claim_decided_green", json!({"badge_id": principal.badge(), "claim_id": claim_id, "action": "escalate"}))?;
        Ok(json!({"claim_state": "ready"}))
    }

    // ---- requests for information that leave the system (docs/53) -----------------------------------------------------
    /// The request the system drafted for this claim version, with what became of it (`drafted`, `sent` or `declined`).
    fn drafted_request(&self, doc: &Value, suggestions: &[Value]) -> Option<Value> {
        let mut draft = crate::copilot::info_request(&doc["claim"], &doc["results"], suggestions, doc["version"].as_i64().unwrap_or(0))?;
        let events = self.store.request_events(doc["claim_id"].as_str().unwrap_or(""), 200).unwrap_or_default();
        if let Some(e) = events.iter().rev().find(|e| e["request_id"] == draft["request_id"]) {
            draft["status"] = json!(if e["kind"] == "sent" { "sent" } else { "declined" });
            draft["decided_at"] = e["at"].clone();
        }
        Some(draft)
    }

    /// The officer's one step on a drafted request: confirm that it is sent, or decline. Confirming also records `request_information` on every finding the
    /// request covers (the system fills that in), puts the complete message in the outbox record and writes the security log. Nothing is delivered from here:
    /// there is no outbound channel in this system, the record is what a delivery step would read.
    pub fn decide_info_request(&self, principal: &Principal, claim_id: &str, request_id: &str, send: bool) -> R<Value> {
        let now = self.now();
        let perms = self.require(principal, "claims.decide")?;
        let doc = self.mine(principal, claim_id, now)?;
        let cfg = self.rules_now().ok_or_else(|| QueueError::Invalid("the copilot is not available on this server".into()))?;
        let bundle = crate::copilot::bundle(&doc["claim"], &doc["results"], &cfg);
        let draft = self.drafted_request(&doc, bundle["suggestions"].as_array().map(Vec::as_slice).unwrap_or(&[])).ok_or_else(|| QueueError::Invalid("nothing on this claim needs a request for information".into()))?;
        if draft["request_id"] != request_id {
            return Err(QueueError::Conflict("request_changed".into()));
        }
        if draft["status"] != "drafted" {
            return Err(QueueError::Conflict(format!("already_{}", draft["status"].as_str().unwrap_or("decided"))));
        }
        let version = doc["version"].as_i64().unwrap_or(0);
        if !send {
            self.store.append_request_event(&json!({"request_id": request_id, "claim_id": claim_id, "version": version, "kind": "declined", "at": now, "by": principal.badge()})).map_err(store_err)?;
            return Ok(json!({"status": "declined", "request_id": request_id}));
        }
        // the system records the request on each finding it covers; the officer must be allowed to decide all of them, or nothing is sent
        let latest = latest_actions(&doc, Some(round(&doc)));
        let items: Vec<String> = draft["items"].as_array().into_iter().flatten().filter_map(|i| i["rule_id"].as_str().map(str::to_string)).collect();
        let finding = |rule: &str| doc["results"].as_array().into_iter().flatten().find(|r| r["rule_id"] == rule).cloned().unwrap_or(Value::Null);
        for rule in items.iter().filter(|r| !auto_requested(&doc, r)) {
            if Self::actions(&perms, &finding(rule), &latest).is_none() {
                return Err(QueueError::Forbidden("claims.decide_high".into()));
            }
        }
        let mut recorded = 0;
        for rule in items.iter().filter(|r| !auto_requested(&doc, r)) {
            self.decide_finding(principal, claim_id, rule, "request_information", &format!("Requested from the provider (request {request_id})."))?;
            recorded += 1;
        }
        let mut sent = draft.clone();
        sent["status"] = json!("sent");
        self.store.append_request_event(&json!({"request_id": request_id, "claim_id": claim_id, "version": version, "kind": "sent", "at": now, "by": principal.badge(), "channel": "outbox", "message": sent})).map_err(store_err)?;
        self.access.record("info_request_sent", json!({"badge_id": principal.badge(), "claim_id": claim_id, "request_id": request_id, "channel": "outbox"}))?;
        Ok(json!({"status": "sent", "request_id": request_id, "decisions_recorded": recorded, "channel": "outbox"}))
    }

    // ---- batches (docs/53) ------------------------------------------------------------------------------------------
    fn batch_view(&self, batch_id: &str) -> R<Option<crate::batch::BatchView>> {
        Ok(crate::batch::fold(&self.store.batch_events(Some(batch_id), 100_000).map_err(store_err)?))
    }

    /// Records that a sampled (or re-checked) green claim was wrong. The first flag quarantines the batch and is written to the security log.
    fn flag_batch(&self, batch_id: &str, claim_id: &str, category: &str, note: &str, badge: &str, now: f64) -> R<()> {
        let before = self.batch_view(batch_id)?;
        let held = before.as_ref().is_some_and(|v| crate::batch::holds(&v.status));
        self.store.append_batch_event(&json!({"batch_id": batch_id, "kind": "flagged", "at": now, "claim_id": claim_id, "category": category, "note": note, "by": badge})).map_err(store_err)?;
        if !held {
            self.access.record("batch_quarantined", json!({"batch_id": batch_id, "claim_id": claim_id, "category": category, "badge_id": badge}))?;
        }
        Ok(())
    }

    /// Claims sent back for review that an officer has not decided yet.
    fn review_pending(&self, v: &crate::batch::BatchView) -> R<i64> {
        let mut pending = 0;
        for id in &v.reopened {
            if self.store.get(id, None).map_err(store_err)?.is_some_and(|d| d["state"] != "decided") {
                pending += 1;
            }
        }
        Ok(pending)
    }

    fn batch_json(&self, v: &crate::batch::BatchView) -> R<Value> {
        let pending = if v.status == "in_review" { Some(self.review_pending(v)?) } else { None };
        Ok(v.to_json(pending))
    }

    /// Every batch, newest first: id, counts and categories only (administrator).
    pub fn batches(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "routing.manage")?;
        let events = self.store.batch_events(None, 100_000).map_err(store_err)?;
        let mut ids: Vec<String> = vec![];
        for e in &events {
            if let Some(b) = e["batch_id"].as_str() {
                if !ids.iter().any(|i| i == b) {
                    ids.push(b.to_string());
                }
            }
        }
        let mut rows = vec![];
        for id in ids.iter().rev().take(200) {
            let mine: Vec<Value> = events.iter().filter(|e| e["batch_id"] == id.as_str()).cloned().collect();
            if let Some(v) = crate::batch::fold(&mine) {
                rows.push(self.batch_json(&v)?);
            }
        }
        let held = rows.iter().filter(|r| r["held"] == true).count();
        Ok(json!({"batches": rows, "held": held}))
    }

    pub fn batch(&self, principal: &Principal, batch_id: &str) -> R<Value> {
        self.require(principal, "routing.manage")?;
        let v = self.batch_view(batch_id)?.ok_or_else(|| QueueError::NotFound(batch_id.into()))?;
        self.batch_json(&v)
    }

    /// The administrator's decision on a held batch: `release` it, send `n` of its cleared claims back to officers (`review_n`), or all of them (`review_all`).
    /// The claims sent back are a random draw among the claims the system cleared; they go to the ordinary pool.
    pub fn decide_batch(&self, principal: &Principal, batch_id: &str, action: &str, n: Option<i64>, note: &str) -> R<Value> {
        use rand::{rngs::OsRng, RngCore};
        let now = self.now();
        self.require(principal, "routing.manage")?;
        if !["release", "review_n", "review_all"].contains(&action) {
            return Err(QueueError::Invalid("action must be release, review_n or review_all".into()));
        }
        let v = self.batch_view(batch_id)?.ok_or_else(|| QueueError::NotFound(batch_id.into()))?;
        if !crate::batch::holds(&v.status) {
            return Err(QueueError::Conflict("this batch is not being held".into()));
        }
        if v.status == "in_review" && self.review_pending(&v)? > 0 {
            return Err(QueueError::Conflict("claims already sent back are still with officers".into()));
        }
        let note: String = note.trim().chars().take(crate::batch::NOTE_CHARS).collect();
        let mut sent: Vec<String> = vec![];
        if action != "release" {
            let mut candidates: Vec<(String, i64)> = vec![];
            for id in &v.members {
                if let Some(d) = self.store.get(id, None).map_err(store_err)? {
                    if d["batch_id"] == batch_id && d["state"] == "decided" && d["auto_cleared"] == true {
                        candidates.push((id.clone(), d["version"].as_i64().unwrap_or(0)));
                    }
                }
            }
            let count = if action == "review_all" {
                candidates.len()
            } else {
                match n {
                    Some(n) if n >= 1 && (n as usize) <= candidates.len() => n as usize,
                    _ => return Err(QueueError::Invalid(format!("n must be between 1 and {} (the claims the system cleared that are still there)", candidates.len()))),
                }
            };
            if count == 0 {
                return Err(QueueError::Invalid("no claim the system cleared is left to send back".into()));
            }
            for (id, version) in crate::batch::pick(&candidates, count, OsRng.next_u64()) {
                let mut set = Map::new();
                set.insert("auto_cleared".into(), json!(false));
                set.insert("decided_by".into(), Value::Null);
                let detail = json!({"event": "batch_review", "batch_id": batch_id});
                if self.store.transition(&id, version, "decided", "ready", principal.badge(), now, Some(detail), Some(set), None).map_err(store_err)?.is_some() {
                    sent.push(id);
                }
            }
        }
        self.store
            .append_batch_event(&json!({"batch_id": batch_id, "kind": "decision", "at": now, "action": action, "n": sent.len(), "note": note, "by": principal.badge(), "reopened": sent}))
            .map_err(store_err)?;
        self.access.record("batch_decision", json!({"batch_id": batch_id, "action": action, "n": sent.len(), "by": principal.badge()}))?;
        self.batch(principal, batch_id)
    }

    /// The rules whose findings reviewers almost always uphold (the exact lower bound of the upheld rate is at least `bulk_confirm_skip_lower`), from the
    /// decided claims. Empty while bulk confirm is off, and for any rule without enough decisions from enough different reviewers.
    pub fn light_review_rules(&self) -> BTreeSet<String> {
        let cfg = self.routing_config();
        if cfg.bulk_confirm_skip_lower <= 0.0 {
            return BTreeSet::new();
        }
        let now = self.now();
        if let Some((at, skip, set)) = self.light.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *skip == cfg.bulk_confirm_skip_lower && now - at < LIGHT_TTL {
                return set.clone();
            }
        }
        let decided = self.store.by_state("decided", 100_000).unwrap_or_default();
        let report = crate::workload::report(&decided, &crate::reliability::Params::from_config(&cfg), cfg.bulk_confirm_skip_lower, cfg.lease_seconds as f64);
        let set: BTreeSet<String> = report["rules"].as_array().into_iter().flatten().filter(|r| r["role"] == "light_review").filter_map(|r| r["rule"].as_str().map(str::to_string)).collect();
        *self.light.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, cfg.bulk_confirm_skip_lower, set.clone()));
        set
    }

    /// Confirms one rule's finding on several claims the caller holds, after a light review. The server enforces what the screen shows: the rule must
    /// be a light-review rule right now, each claim goes through the ordinary `decide_finding` (lease, clearance, countersign, masking and the logs all
    /// apply), only confirm is possible, and a random `audit_percent` of the claims is left for a full review so evidence about the rule keeps coming.
    pub fn bulk_confirm(&self, principal: &Principal, rule_id: &str, claim_ids: &[String]) -> R<Value> {
        self.require(principal, "claims.decide")?;
        let cfg = self.routing_config();
        if cfg.bulk_confirm_skip_lower <= 0.0 {
            return Err(QueueError::Invalid("bulk confirm is switched off".into()));
        }
        if claim_ids.is_empty() || claim_ids.len() > MAX_SUBMIT {
            return Err(QueueError::Invalid(format!("send 1 to {MAX_SUBMIT} claims")));
        }
        let mut seen = BTreeSet::new();
        if !claim_ids.iter().all(|c| seen.insert(c.as_str())) {
            return Err(QueueError::Invalid("a claim may appear once".into()));
        }
        if !self.light_review_rules().contains(rule_id) {
            return Err(QueueError::Forbidden("this rule is not eligible for bulk confirm".into()));
        }
        let (mut confirmed, mut full, mut refused) = (0, 0, 0);
        let mut rows = vec![];
        for id in claim_ids {
            if crate::flow::audit_draw() < cfg.audit_percent as f64 {
                full += 1;
                rows.push(json!({"claim_id": id, "outcome": "full_review"}));
                continue;
            }
            match self.decide_finding(principal, id, rule_id, "confirm_issue", BULK_REASON) {
                Ok(v) => {
                    confirmed += 1;
                    rows.push(json!({"claim_id": id, "outcome": "confirmed", "claim_state": v["claim_state"]}));
                }
                Err(e @ QueueError::Auth(_)) => return Err(e),
                Err(e) => {
                    refused += 1;
                    rows.push(json!({"claim_id": id, "outcome": "refused", "error": e.to_string()}));
                }
            }
        }
        Ok(json!({"rule_id": rule_id, "confirmed": confirmed, "left_for_full_review": full, "refused": refused, "results": rows}))
    }

    // ---- the administrator's side -------------------------------------------------------------------------------------
    /// Everything on record about one claim, for an auditor: each stored version with its state history, the rule pack and configuration
    /// it was triaged under, every decision, and the security-log entries that name it. No claim values are returned.
    // ---- corrections: what-if and recheck ------------------------------------------------------------------------------
    /// The corrected copy of the leased claim, the engine's verdict on it and the difference from now. Nothing is stored.
    fn corrected(&self, principal: &Principal, claim_id: &str, changes: &Value) -> R<(Perms, Value, Value, Vec<Value>, Value, Value)> {
        let perms = self.require(principal, "claims.recheck")?;
        let now = self.now();
        let doc = self.mine(principal, claim_id, now)?;
        if stage(&doc).is_some() {
            return Err(QueueError::Invalid("a claim in countersign is decided as it is; it cannot be corrected".into()));
        }
        let changes = crate::copilot::parse_changes(changes).map_err(QueueError::Invalid)?;
        let (fixed, applied) = crate::copilot::apply(&doc["claim"], &changes).map_err(QueueError::Invalid)?;
        crate::flow::check_claim(&fixed).map_err(|e| QueueError::Invalid(format!("the corrected claim is not valid: {e}")))?;
        let after = (self.intake_now()?.engine)(&fixed).map_err(QueueError::Invalid)?;
        let diff = crate::copilot::diff(&doc["results"], &after);
        Ok((perms, doc, fixed, applied, after, diff))
    }

    /// What-if: how a proposed correction would change the validation results. A dry run: no new version, no decision, no change to the claim.
    pub fn what_if(&self, principal: &Principal, claim_id: &str, changes: &Value) -> R<Value> {
        let (perms, doc, fixed, applied, after, diff) = self.corrected(principal, claim_id, changes)?;
        self.access.record("claim_whatif", json!({"badge_id": principal.badge(), "claim_id": claim_id, "version": doc["version"], "changes": applied.len()}))?;
        let key = &self.access.settings.pii_key;
        let (claim_out, findings) = masking::shape_claim(&fixed, &after, &perms, key, false, true).map_err(|_| QueueError::Redaction)?;
        let mut out = json!({"claim_id": claim_id, "version": doc["version"], "stored": false, "applied": applied, "diff": diff, "claim": claim_out, "findings": findings});
        if let Some(cfg) = self.rules_now() {
            out["copilot"] = crate::copilot::bundle(&fixed, &after, &cfg);
        }
        if !masking::leaks(&out, &masking::identifier_map(&fixed, key).map_err(|_| QueueError::Redaction)?).is_empty() {
            return Err(QueueError::Redaction);
        }
        Ok(out)
    }

    /// Recheck: the correction is validated again as a NEW version of the claim; the original version and its findings stay on record.
    /// The findings the correction cleared are recorded as `mark_corrected_for_recheck` on the old version, the old version is closed
    /// (decided, then rechecked) and the new version goes through triage like any claim. Correcting away a high-severity finding needs the
    /// same authority as deciding it.
    pub fn recheck(&self, principal: &Principal, claim_id: &str, changes: &Value, reason: &str, rule_ids: Option<&[String]>) -> R<Value> {
        let (perms, doc, fixed, applied, _after, diff) = self.corrected(principal, claim_id, changes)?;
        if cg_core::text::blank(reason) || reason.chars().count() > 1000 {
            return Err(QueueError::Invalid("a reason of 1 to 1000 characters is required".into()));
        }
        if masking::text_leaks_identifier(reason, &doc["claim"], &self.access.settings.pii_key) {
            return Err(QueueError::Invalid("the reason must not contain the patient or member identifier".into()));
        }
        let before: BTreeMap<&str, &Value> = doc["results"].as_array().into_iter().flatten().filter_map(|r| r["rule_id"].as_str().map(|id| (id, r))).collect();
        let resolved: Vec<String> = diff["rules"].as_array().into_iter().flatten().filter(|r| r["outcome"] == "resolved").filter_map(|r| r["rule_id"].as_str().map(str::to_string)).collect();
        let chosen: Vec<String> = match rule_ids {
            Some(ids) => ids.to_vec(),
            None => resolved.clone(),
        };
        for rid in &chosen {
            let f = before.get(rid.as_str()).ok_or_else(|| QueueError::Invalid(format!("{rid} has no finding on this claim")))?;
            if !f["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {
                return Err(QueueError::Invalid(format!("{rid} is not flagged")));
            }
        }
        if resolved.iter().any(|rid| before.get(rid.as_str()).is_some_and(|f| is_high(f))) && !perms.contains("claims.decide_high") {
            return Err(QueueError::Forbidden("claims.decide_high".into()));
        }
        let (version, now) = (doc["version"].as_i64().unwrap_or(0), self.now());
        let from = json!({"version": version, "by": principal.badge(), "at": now, "reason": reason, "changes": applied, "cleared": resolved});
        let receipt = self.intake_now()?.submit_with(&fixed, Some(from)).map_err(|e| QueueError::Invalid(e.to_string()))?;
        let newest = self.store.get(claim_id, None).map_err(store_err)?.map(|d| d["version"].as_i64().unwrap_or(0)).unwrap_or(0);
        if newest <= version {
            return Err(QueueError::Invalid("this correction recreates an earlier version of the claim; nothing new was stored".into()));
        }
        let rnd = round(&doc);
        let mut decisions = vec![];
        for rid in &chosen {
            let f = before[rid.as_str()];
            let d = json!({"claim_id": claim_id, "rule_id": rid, "action": "mark_corrected_for_recheck", "actor": principal.badge(), "reason": reason, "created_at": iso(now), "original_status": f["status"]});
            validate_decision(&d, f).map_err(|e| QueueError::Invalid(e.chars().take(300).collect()))?;
            self.store
                .add_decision(claim_id, version, principal.badge(), now, &json!({"rule_id": rid, "action": "mark_corrected_for_recheck", "actor": principal.badge(), "reason": reason, "at": now, "round": rnd}))
                .map_err(store_err)?
                .ok_or_else(|| QueueError::Conflict("lease_lost".into()))?;
            decisions.push(d);
        }
        if !decisions.is_empty() {
            self.review_log.append_review_decisions(&decisions).map_err(|_| AuthError::Unavailable)?;
        }
        self.review_log
            .append_system_events(&[json!({"event_type": "recheck_run", "claim_id": claim_id, "prior_run_id": format!("{claim_id}@v{version}"), "new_run_id": format!("{claim_id}@v{newest}"),
                "prior_input_hash": doc["input_hash"], "new_input_hash": receipt["input_hash"], "rechecked_rule_ids": chosen, "requested_by": principal.badge()})])
            .map_err(|_| AuthError::Unavailable)?;
        self.access.record("claim_rechecked", json!({"badge_id": principal.badge(), "claim_id": claim_id, "prior_version": version, "new_version": newest, "cleared": resolved.len(), "changes": applied.len()}))?;
        // close the old version: decided by the person who corrected it, then rechecked
        let mut set = Map::new();
        set.insert("decided_by".into(), json!(principal.badge()));
        let detail = json!({"event": "corrected_for_recheck", "new_version": newest, "findings": chosen.len()});
        self.store.transition(claim_id, version, "leased", "decided", principal.badge(), now, Some(detail.clone()), Some(set), Some(principal.badge())).map_err(store_err)?.ok_or_else(|| QueueError::Conflict("lease_lost".into()))?;
        self.store.transition(claim_id, version, "decided", "rechecked", principal.badge(), now, Some(detail), None, None).map_err(store_err)?;
        let still: Vec<&str> = diff["rules"].as_array().into_iter().flatten().filter(|r| r["outcome"] == "still_flagged" || r["outcome"] == "newly_flagged").filter_map(|r| r["rule_id"].as_str()).collect();
        Ok(json!({"recorded": true, "prior_version": version, "new_version": newest, "diff": diff, "lane": receipt["lane"], "score": receipt["score"], "still_flagged": still, "cleared_for_recheck": chosen}))
    }

    pub fn trace(&self, principal: &Principal, claim_id: &str) -> R<Value> {
        self.require(principal, "audit.view")?;
        let latest = self.store.get(claim_id, None).map_err(store_err)?.ok_or_else(|| QueueError::NotFound(claim_id.into()))?;
        let mut versions = vec![];
        for v in 1..=latest["version"].as_i64().unwrap_or(0) {
            let doc = if Some(v) == latest["version"].as_i64() { Some(latest.clone()) } else { self.store.get(claim_id, Some(v)).map_err(store_err)? };
            let Some(doc) = doc else { continue };
            let receipt = &doc["receipt"];
            let keep = ["lane", "score", "eligibility", "config_version", "rule_pack_hash", "engine_version", "facts_hash", "degraded", "result_hash", "created_at"];
            let sources: Map<String, Value> = doc["explanation"]["findings"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v["source"].clone())).collect();
            versions.push(json!({
                "version": doc["version"], "input_hash": doc["input_hash"], "receipt": keep.iter().map(|k| ((*k).to_string(), receipt.get(*k).cloned().unwrap_or(Value::Null))).collect::<Map<_, _>>(),
                "state": doc["state"], "state_at": doc["state_at"], "escalated": doc["escalated"], "decided_by": doc["decided_by"], "signoff": doc["signoff"], "lease": doc["lease"],
                "events": doc["events"], "decisions": doc["decisions"],
                "findings": flagged_rows(&doc).iter().map(|r| json!({"rule_id": r["rule_id"], "status": r["status"], "severity": r["severity"]})).collect::<Vec<_>>(),
                "explanation_sources": sources,
            }));
        }
        let log: Vec<Value> = self.securitylog.scan(50_000).unwrap_or_default().into_iter().filter(|r| r["event"]["claim_id"] == claim_id).collect();
        let skip = log.len().saturating_sub(200);
        // a claim the system cleared is held while its batch is quarantined or under review: the claim keeps its record, the batch decides whether the clearance stands
        let batch = match latest["batch_id"].as_str() {
            Some(b) => {
                let status = self.batch_view(b)?.map(|v| v.status).unwrap_or_default();
                json!({"batch_id": b, "status": status, "held": crate::batch::holds(&status), "audit_sample": latest["audit_sample"]})
            }
            None => Value::Null,
        };
        Ok(json!({"claim_id": claim_id, "versions": versions, "batch": batch, "security_log": log[skip..]}))
    }

    pub fn dashboard(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "queue.view")?;
        Ok(self.dashboard_snapshot())
    }

    /// The dashboard numbers without a permission check: for the caller that already checked one (the API route, the health report).
    pub fn dashboard_snapshot(&self) -> Value {
        let now = self.now();
        let cfg = self.routing_config();
        let agents: Vec<Agent> = (self.dispatcher.agents)().into_iter().filter(|a| a.active && cfg.on_shift.contains(&a.badge_id)).collect();
        let senior: Vec<&Agent> = agents.iter().filter(|a| a.permissions.contains("claims.decide_high")).collect();
        let junior: Vec<&Agent> = agents.iter().filter(|a| a.permissions.contains("claims.decide")).collect();
        let (mut oldest, mut waiting) = ([0.0_f64; 2], [0_i64; 2]);
        let mut pool = self.store.by_state("ready", 5000).unwrap_or_default();
        pool.extend(self.store.by_state("awaiting_countersign", 5000).unwrap_or_default());
        for d in &pool {
            let k = usize::from(d["receipt"]["eligibility"] == "decide_high" || d["escalated"] == true || !d["signoff"].is_null());
            waiting[k] += 1;
            oldest[k] = oldest[k].max(now - d["state_at"].as_f64().unwrap_or(now));
        }
        let mut shortages = vec![];
        if waiting[1] > 0 && senior.is_empty() {
            shortages.push("high-severity claims are waiting and nobody on shift may decide them");
        }
        if waiting[0] > 0 && junior.is_empty() {
            shortages.push("claims are waiting and nobody is on shift");
        }
        let signed: Vec<&Value> = pool.iter().filter(|d| matches!(d["signoff"]["stage"].as_str(), Some("countersign" | "tiebreak"))).collect();
        if signed.iter().any(|d| !senior.iter().any(|a| d["signoff"]["first_by"] != a.badge_id.as_str() && d["signoff"]["second_by"] != a.badge_id.as_str())) {
            shortages.push("claims are waiting for a countersignature and no other senior is on shift");
        }
        let inboxes: Map<String, Value> = agents.iter().map(|a| (a.badge_id.clone(), json!(self.store.inbox_load(&a.badge_id).map(|l| l.0).unwrap_or(0)))).collect();
        json!({"counts": self.store.counts().unwrap_or_default(), "waiting": {"decide": waiting[0], "decide_high": waiting[1]}, "awaiting_countersign": signed.len(),
               "oldest_waiting_seconds": {"decide": oldest[0], "decide_high": oldest[1]}, "on_shift": agents.len(), "on_shift_senior": senior.len(), "inbox_sizes": inboxes,
               "shortages": shortages, "config_version": cfg.version, "slice_size": cfg.slice_size})
    }

    /// Hands claims to intake (validate, run the engine, write the receipt). The outbox marker intake leaves is what the relay sweep
    /// publishes to the workers, so this call does no processing itself. One claim failing never stops the others.
    pub fn submit_claims(&self, principal: &Principal, claims: &[Value]) -> R<Vec<Value>> {
        self.require(principal, "routing.manage")?;
        let intake = self.intake.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| QueueError::Invalid("this server was started without a claim intake".into()))?;
        if claims.is_empty() || claims.len() > MAX_SUBMIT {
            return Err(QueueError::Invalid(format!("send between 1 and {MAX_SUBMIT} claims at a time")));
        }
        self.access.record("queue_admin", json!({"actor": principal.badge(), "command": "submit_http"}))?;
        match intake.submit_batch(claims, principal.badge()) {
            Ok((_, rows)) => Ok(rows),
            Err(crate::flow::IntakeError::Failed(m) | crate::flow::IntakeError::Refused(m)) => Err(QueueError::Invalid(m)),
        }
    }

    pub fn get_config(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "routing.manage")?;
        let stored = self.store.latest_config().map_err(store_err)?;
        Ok(json!({"stored_version": stored.and_then(|s| s["version"].as_i64()).unwrap_or(0), "config": rc::to_doc(&self.routing_config()).map_err(QueueError::Invalid)?}))
    }

    pub fn set_config(&self, principal: &Principal, changes: &Map<String, Value>, expected_version: i64) -> R<RoutingConfig> {
        self.require(principal, "routing.manage")?;
        if changes.is_empty() {
            return Err(QueueError::Invalid("changes must be a non-empty object".into()));
        }
        let mut unknown: Vec<&str> = changes.keys().map(String::as_str).filter(|k| !CONFIG_FIELDS.contains(k)).collect();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(QueueError::Invalid(format!("these settings cannot be changed: {}", unknown.join(", "))));
        }
        let stored = self.store.latest_config().map_err(store_err)?;
        let stored_version = stored.and_then(|s| s["version"].as_i64()).unwrap_or(0);
        if expected_version != stored_version {
            return Err(QueueError::Conflict("stale_configuration".into()));
        }
        let current = self.routing_config();
        let mut doc = rc::to_doc(&current).map_err(QueueError::Invalid)?;
        for (k, v) in changes {
            doc[k.as_str()] = v.clone();
        }
        doc["version"] = json!(stored_version + 1);
        let new = rc::from_doc(&doc).map_err(QueueError::Invalid)?;
        if !self.store.put_config(&rc::to_doc(&new).map_err(QueueError::Invalid)?, stored_version).map_err(store_err)? {
            return Err(QueueError::Conflict("stale_configuration".into()));
        }
        let (before, after) = (rc::to_doc(&current).map_err(QueueError::Invalid)?, rc::to_doc(&new).map_err(QueueError::Invalid)?);
        let mut names: Vec<&String> = after.as_object().into_iter().flatten().map(|(k, _)| k).filter(|k| k.as_str() != "version" && before.get(k.as_str()) != after.get(k.as_str())).collect();
        names.sort();
        self.access.record(
            "routing_config_changed",
            json!({"actor": principal.badge(), "version": new.version,
                   "before": names.iter().map(|k| ((*k).clone(), short(&before[k.as_str()]))).collect::<Map<_, _>>(),
                   "after": names.iter().map(|k| ((*k).clone(), short(&after[k.as_str()]))).collect::<Map<_, _>>()}),
        )?;
        Ok(new)
    }

    pub fn feedback(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "queue.view")?;
        Ok(feedback_report(&*self.store, 20, 0.5, 0.95))
    }
}

/// `audit.append`'s and `review_workflow.validate_decision`'s checks, on the fields the queue builds itself.
pub fn validate_decision(decision: &Value, finding: &Value) -> Result<(), String> {
    let text = |k: &str| decision[k].as_str().unwrap_or("");
    if !ACTIONS.contains(&text("action")) {
        return Err("Decision does not match review_event schema: action".into());
    }
    if cg_core::text::blank(text("actor")) {
        return Err("actor is required".into());
    }
    if cg_core::text::blank(text("reason")) {
        return Err("A reason is required for every decision".into());
    }
    if !finding["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {
        return Err(format!("{} is {}; only {:?} findings can be reviewed", text("rule_id"), finding["status"], REVIEWABLE));
    }
    if decision["original_status"] != finding["status"] {
        return Err(format!("original_status {} does not match the finding's actual status {}", decision["original_status"], finding["status"]));
    }
    Ok(())
}

const CLAIM_LEVEL: &str = "(claim level)";
const LABEL_MAX: usize = 40;

/// Which (service code, modifier) pairs a rule's finding pointed at (its affected lines), or one claim-level label (`feedback.py::_service_labels`).
fn service_labels(doc: &Value, rule_id: &str) -> Vec<String> {
    let lines: Vec<&Value> = doc["claim"]["lines"].as_array().into_iter().flatten().collect();
    let mut labels = std::collections::BTreeSet::new();
    for r in doc["results"].as_array().into_iter().flatten().filter(|r| r["rule_id"] == rule_id) {
        for line_id in r["affected_line_ids"].as_array().into_iter().flatten() {
            if let Some(line) = lines.iter().find(|l| l["line_id"] == *line_id) {
                let part = |k: &str, none: &str| -> String {
                    let s = line[k].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(none);
                    s.chars().take(LABEL_MAX).collect()
                };
                labels.insert(format!("{} | {}", part("service_code", "?"), part("modifier", "no modifier")));
            }
        }
    }
    if labels.is_empty() { vec![CLAIM_LEVEL.to_string()] } else { labels.into_iter().collect() }
}

/// What the people decided about the engine's findings, summed up so the rules can be reviewed (`src/workqueue/feedback.py`). Counts only:
/// a dismissal reason is free text and may contain personal data, so the report carries how many reasons were recorded, not what they said.
/// A rule is only marked a review candidate when it has at least `min_decisions` final decisions and the exact lower bound of its dismissal
/// rate is above `threshold`.
pub fn feedback_report(store: &dyn QueueStore, min_decisions: i64, threshold: f64, confidence: f64) -> Value {
    const BIG: usize = 100_000;
    let mut rules: BTreeMap<String, BTreeMap<&'static str, i64>> = BTreeMap::new();
    fn row<'a>(rules: &'a mut BTreeMap<String, BTreeMap<&'static str, i64>>, id: &str) -> &'a mut BTreeMap<&'static str, i64> {
        rules.entry(id.to_string()).or_insert_with(|| ["flagged", "confirmed", "dismissed", "request_information", "mark_corrected_for_recheck", "dismissals_with_reason"].iter().map(|k| (*k, 0)).collect())
    }
    let (mut decided, mut escalated, mut verified_clear, mut agreed, mut tiebroken, mut auto_cleared) = (0, 0, 0, 0, 0, 0);
    let (mut auto_requested_claims, mut auto_requested_findings) = (0, 0);
    let mut services: BTreeMap<String, BTreeMap<String, (i64, i64)>> = BTreeMap::new(); // rule -> (service, modifier) -> (confirmed, dismissed)
    for doc in store.by_state("decided", BIG).unwrap_or_default() {
        decided += 1;
        if doc["auto_cleared"] == true {
            auto_cleared += 1; // decided by the system, not verified by a person
            continue;
        }
        if doc["decided_by"] == crate::flow::AUTO_REQUEST {
            auto_requested_claims += 1; // the system asked for the missing data; no person decided anything
            continue;
        }
        let flagged: Vec<&str> = doc["results"].as_array().into_iter().flatten().filter(|r| r["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s))).filter_map(|r| r["rule_id"].as_str()).collect();
        for id in &flagged {
            *row(&mut rules, id).get_mut("flagged").expect("key") += 1;
        }
        if flagged.is_empty() {
            verified_clear += 1;
        }
        let mut last_final: BTreeMap<&str, &str> = BTreeMap::new();
        for d in doc["decisions"].as_array().into_iter().flatten() {
            let (id, action) = (d["rule_id"].as_str().unwrap_or(""), d["action"].as_str().unwrap_or(""));
            if d["actor"] == crate::flow::AUTO_REQUEST {
                auto_requested_findings += 1; // written by the system, not a reviewer's judgment
                continue;
            }
            if resolving(action) {
                last_final.insert(id, action);
            }
            if action == "dismiss_with_reason" && !cg_core::text::blank(d["reason"].as_str().unwrap_or("")) {
                *row(&mut rules, id).get_mut("dismissals_with_reason").expect("key") += 1;
            }
            if let Some(k) = ["request_information", "mark_corrected_for_recheck"].iter().find(|k| **k == action) {
                *row(&mut rules, id).get_mut(k).expect("key") += 1;
            }
        }
        for (id, action) in last_final {
            let confirmed = action == "confirm_issue";
            *row(&mut rules, id).get_mut(if confirmed { "confirmed" } else { "dismissed" }).expect("key") += 1;
            for label in service_labels(&doc, id) {
                let cell = services.entry(id.to_string()).or_default().entry(label).or_insert((0, 0));
                if confirmed { cell.0 += 1 } else { cell.1 += 1 }
            }
        }
        escalated += i64::from(doc["escalated"] == true);
        match doc["signoff"]["outcome"].as_str() {
            Some("agreed") => agreed += 1,
            Some("tiebreak") => tiebroken += 1,
            _ => {}
        }
    }
    let mut out = Map::new();
    for (id, stats) in &rules {
        let n = stats["confirmed"] + stats["dismissed"];
        let ci = shadow::clopper_pearson(stats["dismissed"], n, confidence);
        let mut o: Map<String, Value> = stats.iter().map(|(k, v)| ((*k).to_string(), json!(v))).collect();
        o.insert("decisions".into(), json!(n));
        o.insert("dismissal_rate".into(), if n > 0 { json!(stats["dismissed"] as f64 / n as f64) } else { Value::Null });
        o.insert("dismissal_rate_lower".into(), json!(ci.map(|c| c.0)));
        o.insert("dismissal_rate_upper".into(), json!(ci.map(|c| c.1)));
        o.insert("review_candidate".into(), json!(n >= min_decisions && ci.is_some_and(|c| c.0 > threshold)));
        if let Some(cells) = services.get(id) {
            let by: Map<String, Value> = cells.iter().map(|(label, (c, d))| (label.clone(), json!({"confirmed": c, "dismissed": d, "decisions": c + d, "dismissal_rate": *d as f64 / (c + d) as f64}))).collect();
            o.insert("by_service".into(), Value::Object(by));
        }
        out.insert(id.clone(), Value::Object(o));
    }
    let countersigned = agreed + tiebroken;
    json!({"claims_decided": decided, "verified_clear": verified_clear, "auto_cleared": auto_cleared, "auto_requested_claims": auto_requested_claims, "auto_requested_findings": auto_requested_findings, "escalated": escalated,
           "signoff": {"countersigned": countersigned, "agreed": agreed, "disagreed": tiebroken, "disagreement_rate": if countersigned > 0 { json!(tiebroken as f64 / countersigned as f64) } else { Value::Null }},
           "rules": out, "parameters": {"min_decisions": min_decisions, "threshold": threshold, "confidence": confidence},
           "note": "counts only; dismissal reasons are not included. A review candidate is a prompt for a human to look at the rule, never a change to it."})
}
