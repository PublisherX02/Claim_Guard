//! The queue service end to end, on the real rule engine and the 50 stress claims: intake, pipeline, dealing, decisions, two-senior
//! sign-off, lease races, permission checks, masking, configuration and the audit trail.

use cg_access::securitylog::SecurityLog;
use cg_access::service::testkit::{world, World};
use cg_access::service::Principal;
use cg_audit::AuditLog;
use cg_engine::Engine;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::dispatcher::Dispatcher;
use cg_queue::explain::{default_guard, ExplainStep, NoModel};
use cg_queue::flow::{engine_fn, process, Intake, Runtime};
use cg_queue::routing_config::RoutingConfig;
use cg_queue::service::{agents_from_access, QueueError, QueueService};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use cg_queue::triage;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

const PW: &str = "Correct-Horse-9";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn engine() -> Arc<Engine> {
    static E: OnceLock<Arc<Engine>> = OnceLock::new();
    E.get_or_init(|| Arc::new(Engine::load(&root()).expect("the rule engine loads from the repository"))).clone()
}

fn claims() -> Vec<Value> {
    std::fs::read_to_string(root().join("data/stress/claims.jsonl")).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

struct Q {
    w: World,
    store: Arc<MemoryQueueStore>,
    dispatcher: Arc<Dispatcher>,
    queue: Arc<QueueService>,
    intake: Arc<Intake>,
    rt: Runtime,
    claims: Vec<Value>,
}

fn q() -> Q {
    let w = world();
    for (badge, level) in [("CG-4004", 4), ("CG-2002", 2), ("CG-2003", 2), ("CG-3003", 3), ("CG-3004", 3), ("CG-3005", 3), ("CG-1001", 1)] {
        w.provision(badge, level, PW);
    }
    let store = Arc::new(MemoryQueueStore::new());
    let t = w.clock.clone();
    let clock: cg_access::Clock = Arc::new(move || t.load(Ordering::SeqCst) as f64 / 1000.0);
    let dispatcher = Arc::new(Dispatcher { store: store.clone(), agents: agents_from_access(w.svc.clone()), clock: clock.clone(), rng_seed: Some(3), log: Some(w.svc.log.clone()) });
    let review_log = Arc::new(AuditLog::open(w.dir.path().join("review.jsonl")).unwrap());
    let queue = Arc::new(QueueService::new(store.clone(), dispatcher.clone(), w.svc.clone(), w.svc.log.clone(), review_log, clock.clone()));
    let e = engine();
    let intake = Arc::new(Intake { store: store.clone(), engine: engine_fn(e.clone()), log: w.svc.log.clone(), clock: clock.clone(), rule_pack_hash: cg_queue::flow::fixed_hash(e.pack.hash()), engine_version: e.code_hash(), advisory: Some(cg_queue::flow::advisory_fn(Arc::new(cg_engine::extensions::Extensions::load(&root()).unwrap()), store.clone())) });
    queue.set_intake(intake.clone());
    queue.set_rules({ let e = e.clone(); Arc::new(move || e.cfg()) });
    let sc = store.clone();
    let explain = Arc::new(ExplainStep::new(store.clone(), Arc::new(NoModel), default_guard(), Arc::new(CircuitBreaker::new(clock.clone())), clock.clone(), Arc::new(move || cg_queue::flow::routing_config(&*sc))));
    let rt = Runtime { store: store.clone(), explain, clock, max_retries: 3 };
    let q = Q { w, store, dispatcher, queue, intake, rt, claims: claims() };
    let admin = q.p("CG-4004");
    let mut set = serde_json::Map::new();
    set.insert("on_shift".into(), json!(["CG-2002", "CG-2003", "CG-3003", "CG-3004", "CG-3005"]));
    set.insert("slice_size".into(), json!(6));
    set.insert("low_water".into(), json!(3));
    q.queue.set_config(&admin, &set, 0).unwrap();
    q
}

impl Q {
    fn p(&self, badge: &str) -> Principal {
        self.w.principal(badge, PW)
    }

    /// The first stress claim whose flagged findings match the wanted kind.
    fn find(&self, kind: &str) -> Value {
        for c in &self.claims {
            if self.store.get(c["claim_id"].as_str().unwrap(), None).unwrap().is_some() {
                continue;
            }
            let mut errs = vec![];
            let results = serde_json::to_value(engine().review(c, &mut errs).unwrap()).unwrap();
            let flagged = triage::flagged(&results);
            let kinds: std::collections::BTreeSet<&str> = flagged.iter().map(|r| r["severity"].as_str().unwrap()).collect();
            let hit = match kind {
                "green" => flagged.is_empty(),
                "medium" => !flagged.is_empty() && kinds == ["medium"].into_iter().collect(),
                "high" => kinds.contains("high"),
                "single_high" => flagged.len() == 1 && kinds == ["high"].into_iter().collect(),
                _ => false,
            };
            if hit {
                return c.clone();
            }
        }
        panic!("no stress claim of kind {kind}");
    }

    /// Submits the claim and walks it to `ready`.
    fn ready(&self, kind: &str) -> String {
        let c = self.find(kind);
        let id = c["claim_id"].as_str().unwrap().to_string();
        self.intake.submit(&c).unwrap();
        let d = self.store.get(&id, None).unwrap().unwrap();
        assert_eq!(process(&self.rt, &id, 1, d["input_hash"].as_str().unwrap()), "ready");
        id
    }

    fn lease_to(&self, id: &str, badge: &str) {
        let now = self.dispatcher.clock.as_ref()();
        assert!(self.store.lease(id, 1, badge, now, now + 1800.0).unwrap().is_some());
    }

    fn flagged_rules(&self, id: &str) -> Vec<(String, String)> {
        let d = self.store.get(id, None).unwrap().unwrap();
        triage::flagged(&d["results"]).iter().map(|r| (r["rule_id"].as_str().unwrap().to_string(), r["severity"].as_str().unwrap().to_string())).collect()
    }

    fn resolve_all(&self, who: &Principal, id: &str, action: &str) -> Value {
        let rules = {
            let d = self.store.get(id, None).unwrap().unwrap();
            let high_only = !d["signoff"].is_null();
            self.flagged_rules(id).into_iter().filter(|(_, sev)| !high_only || sev != "medium").map(|(r, _)| r).collect::<Vec<_>>()
        };
        let mut last = Value::Null;
        for r in rules {
            last = self.queue.decide_finding(who, id, &r, action, "checked against the source").unwrap();
        }
        last
    }

    fn state(&self, id: &str) -> String {
        self.store.get(id, None).unwrap().unwrap()["state"].as_str().unwrap().to_string()
    }

    fn security_kinds(&self) -> Vec<String> {
        self.w.svc.log.scan(10_000).unwrap().into_iter().map(|r| r["event"]["event_type"].as_str().unwrap().to_string()).collect()
    }
}

#[test]
fn a_medium_claim_goes_from_intake_to_decided_by_one_reviewer() {
    let q = q();
    let id = q.ready("medium");
    let deal = q.dispatcher.deal(None, true);
    assert!(deal.assigned.iter().any(|a| a.0 == id));
    let holder = deal.assigned.iter().find(|a| a.0 == id).unwrap().1.clone();
    let who = q.p(&holder);
    let inbox = q.queue.inbox(&who).unwrap();
    let view = inbox.iter().find(|v| v["claim_id"] == id.as_str()).unwrap();
    assert!(view["findings"].as_array().unwrap().iter().any(|f| f["allowed_actions"].is_array()), "the agent is offered actions");
    let last = q.resolve_all(&who, &id, "confirm_issue");
    assert_eq!((last["claim_state"].as_str(), last["review_state"].as_str()), (Some("decided"), Some("resolved")));
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!(d["decided_by"], holder.as_str());
    let kinds = q.security_kinds();
    assert!(kinds.contains(&"decision".to_string()) && kinds.contains(&"triage_receipt".to_string()) && kinds.contains(&"claim_dealt".to_string()));
    assert!(std::fs::read_to_string(q.w.dir.path().join("review.jsonl")).unwrap().contains("confirm_issue"), "the human decision is in the review log too");
}

#[test]
fn a_green_claim_is_verified_clear_or_escalated() {
    let q = q();
    let id = q.ready("green");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    assert!(matches!(q.queue.decide_finding(&who, &id, "R001", "confirm_issue", "x"), Err(QueueError::Invalid(m)) if m.contains("only a flagged")), "a finding that passed cannot be decided");
    assert_eq!(q.queue.decide_green(&who, &id, "verify_clear").unwrap()["claim_state"], "decided");
    assert!(matches!(q.queue.decide_green(&who, &id, "verify_clear"), Err(QueueError::NotFound(_)) | Err(QueueError::Conflict(_))), "not leased any more");
    let id2 = q.ready("green");
    q.lease_to(&id2, "CG-2002");
    assert_eq!(q.queue.decide_green(&who, &id2, "escalate").unwrap()["claim_state"], "ready");
    assert_eq!(q.store.get(&id2, None).unwrap().unwrap()["escalated"], true);
    assert!(matches!(q.queue.decide_green(&who, &id2, "nonsense"), Err(QueueError::Invalid(_))));
}

#[test]
fn a_high_severity_claim_needs_two_different_seniors_and_a_disagreement_goes_to_a_third() {
    let q = q();
    let id = q.ready("high");
    q.lease_to(&id, "CG-3003");
    let first = q.p("CG-3003");
    let last = q.resolve_all(&first, &id, "confirm_issue");
    assert_eq!(last["claim_state"], "awaiting_countersign");
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!((d["signoff"]["first_by"].as_str(), d["signoff"]["stage"].as_str()), (Some("CG-3003"), Some("countersign")));
    // the first signer is never dealt the claim again, and cannot countersign even if it were leased to them
    let deal = q.dispatcher.deal(None, true);
    let second_badge = deal.assigned.iter().find(|a| a.0 == id).map(|a| a.1.clone()).expect("a second senior is dealt the claim");
    assert_ne!(second_badge, "CG-3003");
    // disagreement: the second senior dismisses what the first confirmed
    let second = q.p(&second_badge);
    let last = q.resolve_all(&second, &id, "dismiss_with_reason");
    assert_eq!(last["claim_state"], "ready");
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!((d["escalated"].as_bool(), d["signoff"]["stage"].as_str(), d["signoff"]["outcome"].as_str()), (Some(true), Some("tiebreak"), Some("disagreed")));
    // a third senior decides finally; the first two are never dealt it
    let deal = q.dispatcher.deal(None, true);
    let third_badge = deal.assigned.iter().find(|a| a.0 == id).map(|a| a.1.clone()).expect("a third senior is dealt the claim");
    assert!(third_badge != "CG-3003" && third_badge != second_badge);
    let third = q.p(&third_badge);
    assert_eq!(q.resolve_all(&third, &id, "confirm_issue")["claim_state"], "decided");
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!((d["signoff"]["outcome"].as_str(), d["signoff"]["third_by"].as_str()), (Some("tiebreak"), Some(third_badge.as_str())));
    let kinds = q.security_kinds();
    assert_eq!(kinds.iter().filter(|k| *k == "claim_signoff").count(), 3);
}

#[test]
fn agreeing_seniors_decide_the_claim() {
    let q = q();
    let id = q.ready("single_high");
    q.lease_to(&id, "CG-3003");
    q.resolve_all(&q.p("CG-3003"), &id, "confirm_issue");
    q.lease_to(&id, "CG-3004");
    let second = q.p("CG-3004");
    assert_eq!(q.resolve_all(&second, &id, "confirm_issue")["claim_state"], "decided");
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!((d["signoff"]["outcome"].as_str(), d["decided_by"].as_str()), (Some("agreed"), Some("CG-3004")));
}

#[test]
fn the_first_signer_cannot_countersign_their_own_claim() {
    let q = q();
    let id = q.ready("single_high");
    q.lease_to(&id, "CG-3003");
    let first = q.p("CG-3003");
    q.resolve_all(&first, &id, "confirm_issue");
    q.lease_to(&id, "CG-3003");
    let rule = q.flagged_rules(&id)[0].0.clone();
    assert!(matches!(q.queue.decide_finding(&first, &id, &rule, "confirm_issue", "again"), Err(QueueError::Conflict(m)) if m == "same_person"));
}

#[test]
fn juniors_cannot_decide_high_severity_findings_and_administrators_cannot_decide_anything() {
    let q = q();
    let id = q.ready("high");
    q.lease_to(&id, "CG-2002");
    let junior = q.p("CG-2002");
    let (rule, _) = q.flagged_rules(&id).into_iter().find(|(_, s)| s == "high").unwrap();
    assert!(matches!(q.queue.decide_finding(&junior, &id, &rule, "confirm_issue", "x"), Err(QueueError::Forbidden(_))));
    let admin = q.p("CG-4004");
    assert!(matches!(q.queue.decide_finding(&admin, &id, &rule, "confirm_issue", "x"), Err(QueueError::Forbidden(_))));
    assert!(matches!(q.queue.inbox(&admin), Err(QueueError::Forbidden(_))));
    let viewer = q.p("CG-1001");
    assert!(matches!(q.queue.inbox(&viewer), Err(QueueError::Forbidden(_))));
    assert!(matches!(q.queue.dashboard(&junior), Err(QueueError::Forbidden(_))));
}

#[test]
fn nobody_decides_a_claim_that_is_not_theirs_or_whose_lease_ran_out() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let (rule, _) = q.flagged_rules(&id)[0].clone();
    let other = q.p("CG-2003");
    assert!(matches!(q.queue.decide_finding(&other, &id, &rule, "confirm_issue", "x"), Err(QueueError::NotFound(_))), "not yours: it does not even exist for you");
    let mine = q.p("CG-2002");
    q.w.advance(1800.0 + 5.0);
    assert!(matches!(q.queue.decide_finding(&mine, &id, &rule, "confirm_issue", "x"), Err(QueueError::Conflict(m)) if m == "lease_expired"));
    assert_eq!(q.dispatcher.expire(None), 1);
    let again = q.p("CG-2002");
    assert!(matches!(q.queue.decide_finding(&again, &id, &rule, "confirm_issue", "x"), Err(QueueError::Conflict(m)) if m == "lease_lost"), "it was yours once, so the answer says it was lost");
}

#[test]
fn a_dismissal_needs_a_reason_and_a_decision_is_idempotent_only_once() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let rules = q.flagged_rules(&id);
    let rule = rules[0].0.clone();
    assert!(matches!(q.queue.decide_finding(&who, &id, &rule, "dismiss_with_reason", "   "), Err(QueueError::Invalid(_))), "a dismissal overrides the engine and needs a reason; a confirm does not (see confirming_needs_no_typed_reason)");
    assert!(matches!(q.queue.decide_finding(&who, &id, &rule, "approve", "ok"), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.decide_finding(&who, &id, "R999", "confirm_issue", "ok"), Err(QueueError::NotFound(_))));
    let r = q.queue.decide_finding(&who, &id, &rule, "request_information", "need the invoice").unwrap();
    assert_eq!(r["review_state"], "awaiting_follow_up");
    q.queue.decide_finding(&who, &id, &rule, "confirm_issue", "now confirmed").unwrap();
    if rules.len() > 1 {
        assert!(matches!(q.queue.decide_finding(&who, &id, &rule, "dismiss_with_reason", "changed my mind"), Err(QueueError::Conflict(m)) if m == "already_decided"));
    }
}

#[test]
fn the_view_never_carries_a_raw_patient_or_member_identifier() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let c = q.store.get(&id, None).unwrap().unwrap()["claim"].clone();
    let view = serde_json::to_string(&q.queue.inbox(&q.p("CG-2002")).unwrap()).unwrap();
    for field in ["patient_id", "member_id"] {
        let raw = c[field].as_str().unwrap();
        assert!(!view.to_lowercase().contains(&raw.to_lowercase()), "{field} {raw} leaked");
    }
    assert!(view.contains("PAT-"), "pseudonyms are shown instead");
}

#[test]
fn the_dashboard_counts_the_pool_and_names_shortages() {
    let q = q();
    q.ready("medium");
    q.ready("high");
    let admin = q.p("CG-4004");
    let d = q.queue.dashboard(&admin).unwrap();
    assert_eq!((d["waiting"]["decide"].as_i64(), d["waiting"]["decide_high"].as_i64(), d["on_shift"].as_i64(), d["on_shift_senior"].as_i64()), (Some(1), Some(1), Some(5), Some(3)));
    assert!(d["shortages"].as_array().unwrap().is_empty());
    let mut set = serde_json::Map::new();
    set.insert("on_shift".into(), json!(["CG-2002"]));
    q.queue.set_config(&admin, &set, 1).unwrap();
    let d = q.queue.dashboard(&admin).unwrap();
    assert!(d["shortages"].as_array().unwrap().iter().any(|s| s.as_str().unwrap().contains("nobody on shift may decide")));
}

#[test]
fn configuration_changes_are_versioned_validated_conflict_checked_and_logged() {
    let q = q();
    let admin = q.p("CG-4004");
    let get = q.queue.get_config(&admin).unwrap();
    assert_eq!((get["stored_version"].as_i64(), get["config"]["slice_size"].as_i64()), (Some(1), Some(6)));
    let mut set = serde_json::Map::new();
    set.insert("slice_size".into(), json!(8));
    assert!(matches!(q.queue.set_config(&admin, &set, 0), Err(QueueError::Conflict(m)) if m == "stale_configuration"));
    assert_eq!(q.queue.set_config(&admin, &set, 1).unwrap().version, 2);
    for bad in [("slice_size", json!(0)), ("low_water", json!(50)), ("version", json!(9)), ("nonsense", json!(1)), ("on_shift", json!(["a", "a"]))] {
        let mut s = serde_json::Map::new();
        s.insert(bad.0.into(), bad.1);
        assert!(matches!(q.queue.set_config(&admin, &s, 2), Err(QueueError::Invalid(_))), "{}", bad.0);
    }
    assert!(matches!(q.queue.set_config(&admin, &serde_json::Map::new(), 2), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.set_config(&q.p("CG-3003"), &set, 2), Err(QueueError::Forbidden(_))));
    assert!(q.security_kinds().contains(&"routing_config_changed".to_string()));
    let log = q.w.svc.log.scan(10_000).unwrap();
    let ev = log.iter().rev().find(|r| r["event"]["event_type"] == "routing_config_changed").unwrap();
    assert_eq!((ev["event"]["before"]["slice_size"].as_str(), ev["event"]["after"]["slice_size"].as_str()), (Some("6"), Some("8")));
}

#[test]
fn administrators_submit_claims_and_the_trace_and_feedback_report_what_happened() {
    let q = q();
    let admin = q.p("CG-4004");
    let batch = vec![q.find("medium"), json!({"claim_id": "bad id"}), json!("not an object")];
    let rows = q.queue.submit_claims(&admin, &batch).unwrap();
    assert_eq!((rows[0]["accepted"].as_bool(), rows[1]["accepted"].as_bool(), rows[2]["accepted"].as_bool()), (Some(true), Some(false), Some(false)));
    assert!(matches!(q.queue.submit_claims(&admin, &[]), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.submit_claims(&q.p("CG-3003"), &batch), Err(QueueError::Forbidden(_))));
    let id = rows[0]["claim_id"].as_str().unwrap().to_string();
    assert_eq!(q.state(&id), "triaged");
    let t = q.queue.trace(&admin, &id).unwrap();
    assert_eq!((t["versions"][0]["version"].as_i64(), t["versions"][0]["state"].as_str()), (Some(1), Some("triaged")));
    assert!(t["versions"][0]["receipt"]["rule_pack_hash"].is_string());
    let text = t.to_string();
    let c = q.store.get(&id, None).unwrap().unwrap()["claim"].clone();
    assert!(!text.contains(c["patient_id"].as_str().unwrap()), "a trace carries no claim values");
    assert!(matches!(q.queue.trace(&admin, "NOPE"), Err(QueueError::NotFound(_))));
    assert!(matches!(q.queue.trace(&q.p("CG-2002"), &id), Err(QueueError::Forbidden(_))));
    let report = q.queue.feedback(&admin).unwrap();
    assert_eq!(report["claims_decided"], 0);
}

#[test]
fn a_heartbeat_extends_only_my_leases() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let before = q.store.get(&id, None).unwrap().unwrap()["lease"]["expires_at"].as_f64().unwrap();
    q.w.advance(600.0);
    assert_eq!(q.queue.heartbeat(&who).unwrap(), 1);
    let after = q.store.get(&id, None).unwrap().unwrap()["lease"]["expires_at"].as_f64().unwrap();
    assert!(after > before + 500.0);
    assert!(matches!(q.queue.heartbeat(&q.p("CG-4004")), Err(QueueError::Forbidden(_))));
    let _ = RoutingConfig::default();
}

#[test]
fn advisory_results_are_stored_with_the_claim_and_see_earlier_claims() {
    let q = q();
    let first = q.claims[0].clone();
    q.intake.submit(&first).unwrap();
    let mut again = first.clone();
    again["claim_id"] = json!("CG-REPEAT-0001");
    again["submission_date"] = json!("2099-01-01");
    q.intake.submit(&again).unwrap();
    let doc = q.store.get("CG-REPEAT-0001", None).unwrap().unwrap();
    let adv = doc["advisory"].as_array().unwrap();
    assert_eq!(adv.len(), 8);
    let e101 = adv.iter().find(|a| a["rule_id"] == "E101").unwrap();
    assert_eq!(e101["status"], "FAIL", "the same lines were billed on an earlier claim");
    let original = q.store.get(first["claim_id"].as_str().unwrap(), None).unwrap().unwrap();
    assert_ne!(original["advisory"].as_array().unwrap().iter().find(|a| a["rule_id"] == "E101").unwrap()["status"], "FAIL", "a claim has no earlier claim of its own");
}

#[test]
fn a_decision_reason_that_contains_the_patients_identifier_is_refused() {
    let q = q();
    let id = q.ready("medium");
    let deal = q.dispatcher.deal(None, true);
    let (_, badge, _) = deal.assigned.iter().find(|a| a.0 == id).cloned().expect("dealt");
    let who = q.p(&badge);
    let claim = q.store.get(&id, None).unwrap().unwrap()["claim"].clone();
    let patient = claim["patient_id"].as_str().unwrap().to_string();
    let rule = q.flagged_rules(&id)[0].0.clone();
    assert!(matches!(q.queue.decide_finding(&who, &id, &rule, "confirm_issue", &format!("seen on the card of {patient}")), Err(QueueError::Invalid(m)) if m.contains("identifier")));
    q.queue.decide_finding(&who, &id, &rule, "confirm_issue", "seen on the card").unwrap();
}

// ---- one-click confirm and bulk confirm -------------------------------------------------------------------------------------

#[test]
fn confirming_needs_no_typed_reason_but_dismissing_still_does() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let rules = q.flagged_rules(&id);
    assert!(!rules.is_empty());
    let r = &rules[0].0;
    assert!(matches!(q.queue.decide_finding(&who, &id, r, "dismiss_with_reason", "   "), Err(QueueError::Invalid(m)) if m.contains("reason")), "overriding the engine still needs words");
    assert!(q.queue.decide_finding(&who, &id, r, "confirm_issue", "").is_ok());
    let d = q.store.get(&id, None).unwrap().unwrap();
    let rec = d["decisions"].as_array().unwrap().iter().find(|x| x["rule_id"] == r.as_str()).unwrap();
    assert_eq!(rec["reason"], cg_queue::service::CONFIRM_REASON);
    assert!(std::fs::read_to_string(q.w.dir.path().join("review.jsonl")).unwrap().contains(cg_queue::service::CONFIRM_REASON), "the review log carries the recorded reason");
}

/// Decided dossiers for one rule, so that the rule has enough decisions from enough reviewers to be measured.
fn seed_decided(q: &Q, rule: &str, upheld: usize, overridden: usize) {
    let badges = ["CG-2002", "CG-2003", "CG-3003", "CG-3004", "CG-3005"];
    for i in 0..(upheld + overridden) {
        let (id, badge) = (format!("SEED-{rule}-{i}"), badges[i % badges.len()]);
        let d = json!({"claim_id": id, "version": 1, "input_hash": format!("h-{id}"), "claim": {"claim_id": id}, "results": [{"rule_id": rule, "status": "FAIL", "severity": "medium"}],
                       "receipt": {"lane": "A", "eligibility": "decide", "created_at": 1.0}});
        assert!(q.store.put_triaged(&d).unwrap());
        assert!(q.store.transition(&id, 1, "triaged", "ready", "t", 2.0, None, None, None).unwrap().is_some());
        assert!(q.store.lease(&id, 1, badge, 3.0, 99_999.0).unwrap().is_some());
        let action = if i < upheld { "confirm_issue" } else { "dismiss_with_reason" };
        assert!(q.store.add_decision(&id, 1, badge, 4.0, &json!({"rule_id": rule, "action": action, "actor": badge, "reason": "seed", "at": 4.0, "round": 1})).unwrap().is_some());
        let mut set = serde_json::Map::new();
        set.insert("decided_by".into(), json!(badge));
        assert!(q.store.transition(&id, 1, "leased", "decided", badge, 5.0, None, Some(set), Some(badge)).unwrap().is_some());
    }
}

fn set_bulk(q: &Q, skip: f64, audit: i64) {
    let admin = q.p("CG-4004");
    let v = q.store.latest_config().unwrap().unwrap()["version"].as_i64().unwrap();
    let mut set = serde_json::Map::new();
    set.insert("bulk_confirm_skip_lower".into(), json!(skip));
    set.insert("audit_percent".into(), json!(audit));
    q.queue.set_config(&admin, &set, v).unwrap();
}

/// Ready claims whose only flagged finding is this rule, leased to the reviewer.
fn leased_with(q: &Q, who: &str, n: usize, rule: &str) -> Vec<String> {
    let mut ids = vec![];
    for i in 0..n {
        let id = format!("BULK-{who}-{rule}-{i}");
        let d = json!({"claim_id": id, "version": 1, "input_hash": format!("h-{id}"), "claim": {"claim_id": id, "patient_id": "P1", "member_id": "M1"},
                       "results": [{"rule_id": rule, "status": "FAIL", "severity": "medium", "affected_line_ids": [], "evidence": []}],
                       "receipt": {"lane": "A", "eligibility": "decide", "created_at": 1.0}});
        assert!(q.store.put_triaged(&d).unwrap());
        assert!(q.store.transition(&id, 1, "triaged", "ready", "t", 2.0, None, None, None).unwrap().is_some());
        q.lease_to(&id, who);
        ids.push(id);
    }
    ids
}

#[test]
fn bulk_confirm_is_off_until_an_administrator_turns_it_on_and_only_for_a_rule_reviewers_almost_always_uphold() {
    let q = q();
    seed_decided(&q, "R007", 60, 0);
    seed_decided(&q, "R008", 10, 10);
    let who = q.p("CG-2002");
    let ids = leased_with(&q, "CG-2002", 3, "R007");
    assert!(matches!(q.queue.bulk_confirm(&who, "R007", &ids), Err(QueueError::Invalid(m)) if m.contains("switched off")), "off by default");
    assert!(q.queue.light_review_rules().is_empty());
    set_bulk(&q, 0.9, 1);
    assert_eq!(q.queue.light_review_rules().into_iter().collect::<Vec<_>>(), vec!["R007".to_string()], "R008 is upheld half the time: never eligible");
    let ids8 = leased_with(&q, "CG-2002", 2, "R008");
    assert!(matches!(q.queue.bulk_confirm(&who, "R008", &ids8), Err(QueueError::Forbidden(_))));
    assert!(ids8.iter().all(|id| q.state(id) == "leased"), "a refused request changes nothing");
    assert!(matches!(q.queue.bulk_confirm(&who, "R007", &[]), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.bulk_confirm(&who, "R007", &[ids[0].clone(), ids[0].clone()]), Err(QueueError::Invalid(m)) if m.contains("once")));
}

#[test]
fn bulk_confirm_decides_through_the_ordinary_rules_and_its_decisions_are_not_evidence() {
    let q = q();
    seed_decided(&q, "R007", 60, 0);
    set_bulk(&q, 0.9, 1);
    let who = q.p("CG-2002");
    let ids = leased_with(&q, "CG-2002", 12, "R007");
    let r = q.queue.bulk_confirm(&who, "R007", &ids).unwrap();
    let (confirmed, full) = (r["confirmed"].as_i64().unwrap(), r["left_for_full_review"].as_i64().unwrap());
    assert_eq!(confirmed + full + r["refused"].as_i64().unwrap(), 12);
    assert!(confirmed >= 9, "at a 1% audit almost all are confirmed (got {confirmed})");
    for row in r["results"].as_array().unwrap() {
        let id = row["claim_id"].as_str().unwrap();
        let d = q.store.get(id, None).unwrap().unwrap();
        match row["outcome"].as_str().unwrap() {
            "confirmed" => {
                assert_eq!(d["state"], "decided");
                assert_eq!(d["decided_by"], "CG-2002", "decided by the person, never by the system");
                assert_eq!(d["decisions"][0]["reason"], cg_queue::service::BULK_REASON);
                assert_eq!(d["decisions"][0]["action"], "confirm_issue");
            }
            "full_review" => assert_eq!((d["state"].as_str(), d["decisions"].as_array().unwrap().len()), (Some("leased"), 0), "left for a person to decide"),
            other => panic!("unexpected outcome {other}: {row}"),
        }
    }
    let kinds = q.security_kinds();
    assert!(kinds.iter().filter(|k| *k == "decision").count() as i64 >= confirmed, "every bulk decision is in the security log like any other");
    // not evidence: the rule's counted decisions are still the 60 seeded ones
    let decided = q.store.by_state("decided", 10_000).unwrap();
    assert_eq!(cg_queue::reliability::counted_decisions(&decided).len(), 60);
    // claims leased to someone else are refused one by one
    let theirs = leased_with(&q, "CG-2003", 2, "R007");
    let other = q.queue.bulk_confirm(&who, "R007", &theirs).unwrap();
    assert_eq!(other["refused"].as_i64().unwrap() + other["left_for_full_review"].as_i64().unwrap(), 2);
    assert!(theirs.iter().all(|id| q.state(id) == "leased"));
}

#[test]
fn bulk_confirm_keeps_a_high_severity_finding_behind_a_senior() {
    let q = q();
    seed_decided(&q, "R007", 60, 0);
    set_bulk(&q, 0.9, 1);
    let id = "BULK-HIGH".to_string();
    let d = json!({"claim_id": id, "version": 1, "input_hash": "h", "claim": {"claim_id": id}, "results": [{"rule_id": "R007", "status": "FAIL", "severity": "high", "affected_line_ids": [], "evidence": []}],
                   "receipt": {"lane": "A", "eligibility": "decide_high", "created_at": 1.0}});
    assert!(q.store.put_triaged(&d).unwrap());
    assert!(q.store.transition(&id, 1, "triaged", "ready", "t", 2.0, None, None, None).unwrap().is_some());
    q.lease_to(&id, "CG-2002");
    let r = q.queue.bulk_confirm(&q.p("CG-2002"), "R007", &[id.clone()]).unwrap();
    let outcome = r["results"][0]["outcome"].as_str().unwrap();
    assert!(outcome == "refused" || outcome == "full_review", "a junior cannot confirm a high-severity finding, in bulk or one by one: {r}");
    assert_eq!(q.state(&id), "leased");
}

#[test]
fn bulk_confirm_at_a_100_percent_audit_confirms_nothing_and_leaves_every_claim_for_a_full_review() {
    let q = q();
    seed_decided(&q, "R007", 60, 0);
    set_bulk(&q, 0.9, 100);
    let who = q.p("CG-2002");
    let ids = leased_with(&q, "CG-2002", 6, "R007");
    let r = q.queue.bulk_confirm(&who, "R007", &ids).unwrap();
    assert_eq!((r["confirmed"].as_i64(), r["left_for_full_review"].as_i64()), (Some(0), Some(6)));
    assert!(ids.iter().all(|id| q.state(id) == "leased" && q.store.get(id, None).unwrap().unwrap()["decisions"].as_array().unwrap().is_empty()));
}

// ---- a person on a claim where the system already asked for the missing data -------------------------------------------------------

/// A real stress claim with crafted results (the flagged findings are exactly `flags`), stored as triaged and run through the pipeline.
fn crafted(q: &Q, id: &str, flags: &[(&str, &str, &str)]) -> String {
    let mut claim = q.find("green");
    claim["claim_id"] = json!(id);
    let results: Vec<Value> = triage::rule_ids()
        .into_iter()
        .map(|r| match flags.iter().find(|f| f.0 == r) {
            Some((_, status, sev)) => json!({"rule_id": r, "status": status, "severity": sev, "affected_line_ids": [], "evidence": [], "explanation": "engine text", "corrective_action": "Request the missing source information."}),
            None => json!({"rule_id": r, "status": "PASS", "severity": "medium", "affected_line_ids": [], "evidence": [], "explanation": "ok"}),
        })
        .collect();
    let results = Value::Array(results);
    let receipt = triage::make_receipt(&claim, &results, &cg_queue::flow::routing_config(&*q.store), "pack", "eng", 1.0);
    let ihash = triage::input_hash(&claim);
    let d = json!({"claim_id": id, "version": 1, "input_hash": ihash, "claim": claim, "results": results, "receipt": receipt, "advisory": []});
    assert!(q.store.put_triaged(&d).unwrap());
    process(&q.rt, id, 1, &ihash);
    id.to_string()
}

fn set_request_info(q: &Q, scope: &str) {
    let admin = q.p("CG-4004");
    let v = q.store.latest_config().unwrap().unwrap()["version"].as_i64().unwrap();
    let mut set = serde_json::Map::new();
    set.insert("auto_request_info".into(), json!(scope));
    set.insert("audit_percent".into(), json!(1));
    q.queue.set_config(&admin, &set, v).unwrap();
}

#[test]
fn a_person_decides_the_real_finding_and_the_requested_information_does_not_block_the_claim() {
    let q = q();
    set_request_info(&q, "medium");
    // retry until the claim is not in the 1% audit sample
    let mut id = String::new();
    for i in 0..30 {
        let candidate = crafted(&q, &format!("MIX-{i}"), &[("R003", "FAIL", "medium"), ("R009", "UNABLE_TO_ASSESS", "medium")]);
        if q.store.get(&candidate, None).unwrap().unwrap()["auto_requested"] == true {
            id = candidate;
            break;
        }
    }
    assert!(!id.is_empty(), "30 draws at a 1% sample cannot all be audited");
    assert_eq!(q.state(&id), "ready");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let view = q.queue.inbox(&who).unwrap().into_iter().find(|v| v["claim_id"] == id.as_str()).unwrap();
    let row = |rule: &str| view["findings"].as_array().unwrap().iter().find(|f| f["rule_id"] == rule).unwrap().clone();
    assert_eq!(row("R009")["review_state"], "info_requested_automatically");
    assert!(row("R009")["allowed_actions"].is_null(), "the person is not offered the finding the system already asked about");
    assert!(row("R003")["allowed_actions"].is_array());
    assert!(matches!(q.queue.decide_finding(&who, &id, "R009", "confirm_issue", ""), Err(QueueError::Invalid(m)) if m.contains("requested automatically")));
    let r = q.queue.decide_finding(&who, &id, "R003", "confirm_issue", "").unwrap();
    assert_eq!(r["claim_state"], "decided", "the claim is decided once the real finding is: the request for data stays on record");
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!(d["decided_by"], "CG-2002");
    assert_eq!(d["decisions"].as_array().unwrap().len(), 2);
}

#[test]
fn a_high_severity_request_the_system_wrote_does_not_send_the_claim_to_a_second_senior() {
    let q = q();
    set_request_info(&q, "all");
    let mut id = String::new();
    for i in 0..30 {
        let candidate = crafted(&q, &format!("HMIX-{i}"), &[("R003", "FAIL", "medium"), ("R009", "UNABLE_TO_ASSESS", "high")]);
        if q.store.get(&candidate, None).unwrap().unwrap()["auto_requested"] == true {
            id = candidate;
            break;
        }
    }
    assert!(!id.is_empty());
    q.lease_to(&id, "CG-2002");
    let r = q.queue.decide_finding(&q.p("CG-2002"), &id, "R003", "confirm_issue", "").unwrap();
    assert_eq!(r["claim_state"], "decided", "the only high-severity finding was asked about by the system, so no countersignature is waiting on it");
}


// ---- corrections: what-if and recheck -----------------------------------------------------------------------------------------------

/// A green stress claim whose first line amount is wrong (R007 and, because the total no longer adds up, R012), ready and leased to `who`.
fn wrong_amount(q: &Q, id: &str, who: &str) -> Value {
    let mut claim = q.find("green");
    claim["claim_id"] = json!(id);
    let line = &claim["lines"][0];
    let right = line["quantity"].as_f64().unwrap() * line["unit_price"].as_f64().unwrap();
    claim["lines"][0]["net_amount"] = json!(right + 11.0);
    q.intake.submit(&claim).unwrap();
    let d = q.store.get(id, None).unwrap().unwrap();
    assert_eq!(process(&q.rt, id, 1, d["input_hash"].as_str().unwrap()), "ready");
    q.lease_to(id, who);
    claim
}

fn fix_both(claim: &Value) -> Value {
    let line = &claim["lines"][0];
    let right = line["quantity"].as_f64().unwrap() * line["unit_price"].as_f64().unwrap();
    let others: f64 = claim["lines"].as_array().unwrap().iter().skip(1).map(|l| l["net_amount"].as_f64().unwrap()).sum();
    json!([{"path": "/lines/0/net_amount", "value": right}, {"path": "/total_amount", "value": right + others}])
}

#[test]
fn a_what_if_shows_what_a_correction_would_do_and_stores_nothing() {
    let q = q();
    let claim = wrong_amount(&q, "FIX-1", "CG-3003");
    let who = q.p("CG-3003");
    let view = q.queue.inbox(&who).unwrap().into_iter().find(|v| v["claim_id"] == "FIX-1").unwrap();
    let advice = &view["copilot"];
    assert_eq!(advice["summary"]["cleared"], json!(false));
    let r007 = advice["suggestions"].as_array().unwrap().iter().find(|s| s["rule_id"] == "R007").unwrap();
    assert!(!r007["proposed_edits"].as_array().unwrap().is_empty(), "R007 proposes the amount from quantity x price");
    assert_eq!(view["can_correct"], json!(true));

    let before = q.store.get("FIX-1", None).unwrap().unwrap();
    let out = q.queue.what_if(&who, "FIX-1", &fix_both(&claim)).unwrap();
    assert_eq!(out["stored"], json!(false));
    assert_eq!(out["diff"]["would_clear"], json!(true), "{}", out["diff"]);
    assert_eq!(out["diff"]["counts"]["newly_flagged"], json!(0));
    assert!(out["copilot"]["summary"]["cleared"].as_bool().unwrap());
    assert_eq!(q.store.get("FIX-1", None).unwrap().unwrap(), before, "a what-if changes nothing");
    assert!(q.store.get("FIX-1", Some(2)).unwrap().is_none());
    assert!(!std::fs::read_to_string(q.w.dir.path().join("review.jsonl")).unwrap_or_default().contains("recheck_run"));

    // an identifier, a note, a field that does not exist and a no-op are refused
    for bad in [json!([{"path": "/patient_id", "value": "PAT-X"}]), json!([{"path": "/notes", "value": "x"}]), json!([{"path": "/lines/9/net_amount", "value": 1}]),
                json!([{"path": "/currency", "value": claim["currency"]}]), json!([]), json!([{"path": "/lines/0/quantity", "value": {"a": 1}}])] {
        assert!(matches!(q.queue.what_if(&who, "FIX-1", &bad), Err(QueueError::Invalid(_))), "{bad}");
    }
    // someone who does not hold the claim cannot probe it
    assert!(q.queue.what_if(&q.p("CG-3004"), "FIX-1", &fix_both(&claim)).is_err());
    assert!(q.security_kinds().contains(&"claim_whatif".to_string()));
}

#[test]
fn a_recheck_is_a_new_version_the_old_one_stays_and_every_step_is_on_record() {
    let q = q();
    let claim = wrong_amount(&q, "FIX-2", "CG-3003");
    let who = q.p("CG-3003");
    let out = q.queue.recheck(&who, "FIX-2", &fix_both(&claim), "amount and total corrected from the bill", None).unwrap();
    assert_eq!((out["prior_version"].as_i64(), out["new_version"].as_i64()), (Some(1), Some(2)));
    assert_eq!(out["still_flagged"], json!([]));
    let mut cleared: Vec<String> = out["cleared_for_recheck"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    cleared.sort();
    assert_eq!(cleared, ["R007", "R012"]);

    // the original is untouched and closed; the new version carries who corrected what
    let v1 = q.store.get("FIX-2", Some(1)).unwrap().unwrap();
    assert_eq!(v1["state"], "rechecked");
    assert_eq!(v1["claim"], claim, "the original claim is preserved exactly");
    assert!(v1["decisions"].as_array().unwrap().iter().all(|d| d["action"] == "mark_corrected_for_recheck"));
    let v2 = q.store.get("FIX-2", None).unwrap().unwrap();
    assert_eq!(v2["version"], 2);
    assert_ne!(v2["input_hash"], v1["input_hash"]);
    assert_eq!(v2["corrected_from"]["by"], "CG-3003");
    assert_eq!(v2["corrected_from"]["changes"].as_array().unwrap().len(), 2);
    assert!(triage::flagged(&v2["results"]).is_empty(), "the corrected claim passes every check");

    // the audit trail links both versions
    let review = std::fs::read_to_string(q.w.dir.path().join("review.jsonl")).unwrap();
    assert!(review.contains("mark_corrected_for_recheck") && review.contains("recheck_run") && review.contains("FIX-2@v2"));
    assert!(q.security_kinds().contains(&"claim_rechecked".to_string()));
    let trace = q.queue.trace(&q.p("CG-4004"), "FIX-2").unwrap();
    assert_eq!(trace["versions"].as_array().unwrap().len(), 2);

    // the new version walks the pipeline like any claim, and recorded decisions are not carried over to it
    let h2 = v2["input_hash"].as_str().unwrap().to_string();
    assert!(["ready", "decided"].contains(&process(&q.rt, "FIX-2", 2, &h2)));
    assert!(q.store.get("FIX-2", Some(2)).unwrap().unwrap()["decisions"].as_array().map_or(true, |d| d.is_empty()));
    // the lease on version 1 is gone: the same correction cannot be sent twice
    assert!(q.queue.recheck(&who, "FIX-2", &fix_both(&claim), "again", None).is_err());
}

#[test]
fn correcting_away_a_high_severity_finding_needs_a_senior_but_anyone_may_try_it_in_a_what_if() {
    let q = q();
    let claim = wrong_amount(&q, "FIX-3", "CG-2002");
    let junior = q.p("CG-2002");
    assert!(q.queue.what_if(&junior, "FIX-3", &fix_both(&claim)).is_ok(), "a what-if is open to a reviewer");
    let denied = q.queue.recheck(&junior, "FIX-3", &fix_both(&claim), "fixed", None);
    assert!(matches!(denied, Err(QueueError::Forbidden(_))), "{denied:?}");
    assert!(q.store.get("FIX-3", Some(2)).unwrap().is_none(), "a refused recheck stores nothing");
    assert_eq!(q.state("FIX-3"), "leased");
    // administrators and trainees have no part in it
    assert!(q.queue.what_if(&q.p("CG-4004"), "FIX-3", &fix_both(&claim)).is_err());
    assert!(q.queue.what_if(&q.p("CG-1001"), "FIX-3", &fix_both(&claim)).is_err());
}

#[test]
fn a_recheck_needs_a_reason_that_does_not_leak_the_identifier() {
    let q = q();
    let claim = wrong_amount(&q, "FIX-4", "CG-3003");
    let who = q.p("CG-3003");
    assert!(matches!(q.queue.recheck(&who, "FIX-4", &fix_both(&claim), "   ", None), Err(QueueError::Invalid(_))));
    let leak = format!("fixed for {}", claim["patient_id"].as_str().unwrap());
    assert!(matches!(q.queue.recheck(&who, "FIX-4", &fix_both(&claim), &leak, None), Err(QueueError::Invalid(_))));
    assert!(q.store.get("FIX-4", Some(2)).unwrap().is_none());
}

// ---- batches: the audit sample is exact, one flag quarantines the batch, the administrator decides -----------------------------

fn green_copies(q: &Q, n: usize) -> Vec<Value> {
    let base = q.find("green");
    (0..n)
        .map(|i| {
            let mut c = base.clone();
            c["claim_id"] = json!(format!("BATCH-{i:03}"));
            c
        })
        .collect()
}

fn batch_world(green: usize) -> (Q, String, Vec<String>) {
    let q = q();
    let admin = q.p("CG-4004");
    let mut set = serde_json::Map::new();
    set.insert("auto_clear_green".into(), json!(true));
    set.insert("audit_percent".into(), json!(5));
    q.queue.set_config(&admin, &set, 1).unwrap();
    let mut claims = green_copies(&q, green);
    let flagged = q.find("medium");
    claims.push(flagged);
    let (batch, rows) = q.intake.submit_batch(&claims, "CG-4004").unwrap();
    assert!(rows.iter().all(|r| r["accepted"] == true));
    let ids: Vec<String> = claims.iter().map(|c| c["claim_id"].as_str().unwrap().to_string()).collect();
    for id in &ids {
        let d = q.store.get(id, None).unwrap().unwrap();
        process(&q.rt, id, 1, d["input_hash"].as_str().unwrap());
    }
    (q, batch.expect("a batch id"), ids)
}

fn sampled_ids(q: &Q, ids: &[String]) -> Vec<String> {
    ids.iter().filter(|id| q.store.get(id, None).unwrap().unwrap()["audit_sample"] == true).cloned().collect()
}

fn is_cleared(q: &Q, id: &str) -> bool {
    let d = q.store.get(id, None).unwrap().unwrap();
    d["state"] == "decided" && d["auto_cleared"] == true
}

#[test]
fn the_audit_sample_of_a_batch_is_an_exact_share_of_its_green_claims() {
    for green in [1usize, 19, 100] {
        let (q, batch, ids) = batch_world(green);
        let expect = cg_queue::batch::sample_size(green, 5);
        let sampled = sampled_ids(&q, &ids);
        assert_eq!(sampled.len(), expect, "{green} green claims -> {expect} sampled");
        assert_eq!(ids.iter().filter(|id| is_cleared(&q, id)).count(), green - expect, "every other green claim is cleared by the system");
        for id in &sampled {
            assert_eq!(q.state(id), "ready", "a sampled claim waits for an officer");
        }
        assert_ne!(q.state(ids.last().unwrap()), "decided", "a claim with a finding is never cleared by the system");
        let view = q.queue.batch(&q.p("CG-4004"), &batch).unwrap();
        assert_eq!(
            (view["claims"].as_i64(), view["green"].as_i64(), view["sampled"].as_i64(), view["status"].as_str()),
            (Some(green as i64 + 1), Some(green as i64), Some(expect as i64), Some("open"))
        );
    }
}

#[test]
fn one_flagged_sample_claim_holds_the_whole_batch_and_the_administrator_decides_what_happens() {
    let (q, batch, ids) = batch_world(100);
    let (admin, officer, other) = (q.p("CG-4004"), q.p("CG-2002"), q.p("CG-2003"));
    let sampled = sampled_ids(&q, &ids);
    assert_eq!(sampled.len(), 5);
    // an officer who finds the clearance wrong must say which rule should have caught it
    q.lease_to(&sampled[0], "CG-2002");
    assert!(matches!(q.queue.decide_green_in(&officer, &sampled[0], "escalate", None, None), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.decide_green_in(&officer, &sampled[0], "escalate", Some("R99"), None), Err(QueueError::Invalid(_))));
    assert_eq!(q.queue.batch(&admin, &batch).unwrap()["status"], "open", "a refused flag changes nothing");
    q.queue.decide_green_in(&officer, &sampled[0], "escalate", Some("R007"), Some("amount does not match quantity")).unwrap();
    let view = q.queue.batch(&admin, &batch).unwrap();
    assert_eq!(
        (view["status"].as_str(), view["held"].as_bool(), view["sample_flagged"].as_i64(), view["sampled"].as_i64(), view["categories"]["R007"].as_i64()),
        (Some("quarantined"), Some(true), Some(1), Some(5), Some(1))
    );
    assert!(!view.to_string().contains("amount does not match"), "the administrator sees categories, never the words of an officer");
    assert!(!view.to_string().contains("BATCH-"), "and no claim id");
    // a cleared claim of the batch says it is held
    let cleared = ids.iter().find(|id| is_cleared(&q, id)).unwrap().clone();
    let trace = q.queue.trace(&admin, &cleared).unwrap();
    assert_eq!((trace["batch"]["held"].as_bool(), trace["batch"]["batch_id"].as_str()), (Some(true), Some(batch.as_str())));
    // the rest of the sample is still analysed by officers; a second flag only adds to the count and is not a second quarantine
    q.lease_to(&sampled[1], "CG-2003");
    q.queue.decide_green_in(&other, &sampled[1], "escalate", Some("R012"), None).unwrap();
    q.lease_to(&sampled[2], "CG-2002");
    q.queue.decide_green_in(&officer, &sampled[2], "verify_clear", None, None).unwrap();
    let view = q.queue.batch(&admin, &batch).unwrap();
    assert_eq!((view["sample_flagged"].as_i64(), view["sample_verified"].as_i64(), view["sample_waiting"].as_i64()), (Some(2), Some(1), Some(2)));
    assert_eq!(q.security_kinds().iter().filter(|k| *k == "batch_quarantined").count(), 1);
    assert_eq!(q.security_kinds().iter().filter(|k| *k == "batch_created").count(), 1);
    // only an administrator sees or decides batches
    assert!(matches!(q.queue.batches(&officer), Err(QueueError::Forbidden(_))));
    assert!(matches!(q.queue.decide_batch(&officer, &batch, "release", None, ""), Err(QueueError::Forbidden(_))));
    assert_eq!(q.queue.batches(&admin).unwrap()["held"], 1);
    // send some back: a random draw among the claims the system cleared
    assert!(matches!(q.queue.decide_batch(&admin, &batch, "review_n", Some(0), ""), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.decide_batch(&admin, &batch, "review_n", Some(500), ""), Err(QueueError::Invalid(_))));
    assert!(matches!(q.queue.decide_batch(&admin, &batch, "burn", None, ""), Err(QueueError::Invalid(_))));
    let view = q.queue.decide_batch(&admin, &batch, "review_n", Some(3), "check a few").unwrap();
    assert_eq!((view["status"].as_str(), view["review"]["sent_back"].as_i64(), view["review"]["still_with_officers"].as_i64()), (Some("in_review"), Some(3), Some(3)));
    let back: Vec<String> = ids
        .iter()
        .filter(|id| q.store.get(id, None).unwrap().unwrap()["events"].as_array().unwrap().iter().any(|e| e["detail"]["event"] == "batch_review"))
        .cloned()
        .collect();
    assert_eq!(back.len(), 3);
    for id in &back {
        let d = q.store.get(id, None).unwrap().unwrap();
        assert_eq!((d["state"].as_str(), d["auto_cleared"].as_bool(), d["decided_by"].is_null()), (Some("ready"), Some(false), true), "back with the officers, no longer cleared by the system");
    }
    assert!(matches!(q.queue.decide_batch(&admin, &batch, "release", None, ""), Err(QueueError::Conflict(_))), "not while the claims sent back are still with officers");
    for id in &back {
        q.lease_to(id, "CG-2003");
        q.queue.decide_green_in(&other, id, "verify_clear", None, None).unwrap();
    }
    let view = q.queue.batch(&admin, &batch).unwrap();
    assert_eq!((view["status"].as_str(), view["review"]["still_with_officers"].as_i64()), (Some("review_done"), Some(0)));
    // the administrator may now order everything that is left checked
    let still_cleared = ids.iter().filter(|id| is_cleared(&q, id)).count() as i64;
    let view = q.queue.decide_batch(&admin, &batch, "review_all", None, "").unwrap();
    assert_eq!((view["status"].as_str(), view["review"]["sent_back"].as_i64()), (Some("in_review"), Some(3 + still_cleared)));
}

#[test]
fn a_released_batch_holds_nothing_and_a_batch_nobody_flagged_cannot_be_decided() {
    let (q, batch, ids) = batch_world(40);
    let (admin, officer) = (q.p("CG-4004"), q.p("CG-2002"));
    assert!(matches!(q.queue.decide_batch(&admin, &batch, "release", None, ""), Err(QueueError::Conflict(_))), "an open batch is not being held");
    assert!(matches!(q.queue.batch(&admin, "B-nope"), Err(QueueError::NotFound(_))));
    let sampled = sampled_ids(&q, &ids);
    assert_eq!(sampled.len(), 2);
    q.lease_to(&sampled[0], "CG-2002");
    q.queue.decide_green_in(&officer, &sampled[0], "escalate", Some("other"), None).unwrap();
    let view = q.queue.decide_batch(&admin, &batch, "release", None, "one-off, understood").unwrap();
    assert_eq!((view["status"].as_str(), view["held"].as_bool()), (Some("released"), Some(false)));
    let cleared = ids.iter().find(|id| is_cleared(&q, id)).unwrap().clone();
    assert_eq!(q.queue.trace(&admin, &cleared).unwrap()["batch"]["held"], false);
    assert_eq!(q.security_kinds().iter().filter(|k| *k == "batch_decision").count(), 1);
    // a later flag in the same batch holds it again
    q.lease_to(&sampled[1], "CG-2002");
    q.queue.decide_green_in(&officer, &sampled[1], "escalate", Some("R007"), None).unwrap();
    assert_eq!(q.queue.batch(&admin, &batch).unwrap()["status"], "quarantined");
    assert_eq!(q.security_kinds().iter().filter(|k| *k == "batch_quarantined").count(), 2);
}

#[test]
fn a_claim_that_is_not_in_a_batch_keeps_working_as_before() {
    let q = q();
    let id = q.ready("green");
    q.lease_to(&id, "CG-2002");
    q.queue.decide_green_in(&q.p("CG-2002"), &id, "escalate", None, None).unwrap();
    assert!(q.queue.batches(&q.p("CG-4004")).unwrap()["batches"].as_array().unwrap().is_empty());
}

// ---- the request for information the system drafts: the officer only confirms that it is sent --------------------------------

/// A stress claim whose findings include a request for information, with the draft the system makes for it.
fn claim_needing_a_request(high: bool) -> (Value, Value) {
    let e = engine();
    for c in claims() {
        let mut errs = vec![];
        let results = serde_json::to_value(e.review(&c, &mut errs).unwrap()).unwrap();
        let bundle = cg_queue::copilot::bundle(&c, &results, &e.cfg());
        let Some(draft) = cg_queue::copilot::info_request(&c, &results, bundle["suggestions"].as_array().unwrap(), 1) else { continue };
        let has_high = triage::flagged(&results).iter().any(|r| r["severity"] == "high");
        // the request must cover every flagged finding, so the claim is finished by sending it (keeps the test about the request, not about the other findings)
        let covered = draft["items"].as_array().unwrap().len() == triage::flagged(&results).len();
        if covered && has_high == high {
            return (c, draft);
        }
    }
    panic!("no stress claim whose findings are all requests for information (high: {high})");
}

#[test]
fn the_system_drafts_the_whole_request_and_the_officer_only_confirms_that_it_is_sent() {
    let q = q();
    let (claim, draft) = claim_needing_a_request(false);
    let id = claim["claim_id"].as_str().unwrap().to_string();
    let rid = draft["request_id"].as_str().unwrap().to_string();
    let body = draft["body"].as_str().unwrap();
    assert!(body.contains(&id) && body.contains("not a decision on the claim and not a promise of payment"), "{body}");
    assert!(!body.contains(claim["patient_id"].as_str().unwrap()) && !body.contains(claim["member_id"].as_str().unwrap()), "the message names no patient");
    assert_eq!(draft["status"], "drafted");
    q.intake.submit(&claim).unwrap();
    let d = q.store.get(&id, None).unwrap().unwrap();
    assert_eq!(process(&q.rt, &id, 1, d["input_hash"].as_str().unwrap()), "ready");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    // the officer sees the draft with the claim
    let seen = q.queue.inbox(&who).unwrap().into_iter().find(|v| v["claim_id"] == id.as_str());
    if let Some(v) = &seen {
        assert_eq!((v["info_request"]["request_id"].as_str(), v["info_request"]["status"].as_str()), (Some(rid.as_str()), Some("drafted")));
    }
    // a stale or invented request id sends nothing
    assert!(matches!(q.queue.decide_info_request(&who, &id, "0000000000000000", true), Err(QueueError::Conflict(m)) if m == "request_changed"));
    assert!(q.store.request_events(&id, 10).unwrap().is_empty());
    assert!(!q.security_kinds().contains(&"info_request_sent".to_string()));
    // only a person with the permission can confirm
    assert!(q.queue.decide_info_request(&q.p("CG-4004"), &id, &rid, true).is_err());
    let out = q.queue.decide_info_request(&who, &id, &rid, true).unwrap();
    assert_eq!((out["status"].as_str(), out["channel"].as_str()), (Some("sent"), Some("outbox")));
    assert_eq!(out["decisions_recorded"].as_u64(), Some(draft["items"].as_array().unwrap().len() as u64), "the system recorded the request on every finding it covers");
    let after = q.store.get(&id, None).unwrap().unwrap();
    assert!(after["decisions"].as_array().unwrap().iter().all(|d| d["action"] == "request_information"));
    assert_eq!(after["state"], "leased", "a request is a follow-up, not a resolution: the claim stays with the officer until the information comes back, as when the officer records the request by hand");
    let events = q.store.request_events(&id, 10).unwrap();
    assert_eq!((events.len(), events[0]["kind"].as_str(), events[0]["message"]["body"].as_str()), (1, Some("sent"), Some(body)), "the complete message is in the outbox record");
    assert_eq!(q.security_kinds().iter().filter(|k| *k == "info_request_sent").count(), 1);
    // it cannot be sent twice
    assert!(q.queue.decide_info_request(&who, &id, &rid, true).is_err());
}

#[test]
fn an_officer_may_decline_a_drafted_request_and_nothing_is_recorded_on_the_findings() {
    let q = q();
    let (claim, draft) = claim_needing_a_request(false);
    let id = claim["claim_id"].as_str().unwrap().to_string();
    let rid = draft["request_id"].as_str().unwrap().to_string();
    q.intake.submit(&claim).unwrap();
    let d = q.store.get(&id, None).unwrap().unwrap();
    process(&q.rt, &id, 1, d["input_hash"].as_str().unwrap());
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let out = q.queue.decide_info_request(&who, &id, &rid, false).unwrap();
    assert_eq!(out["status"], "declined");
    assert!(q.store.get(&id, None).unwrap().unwrap()["decisions"].as_array().unwrap().is_empty());
    assert_eq!(q.store.request_events(&id, 10).unwrap()[0]["kind"], "declined");
    assert!(!q.security_kinds().contains(&"info_request_sent".to_string()));
    assert!(matches!(q.queue.decide_info_request(&who, &id, &rid, true), Err(QueueError::Conflict(m)) if m == "already_declined"), "a declined draft is not sent afterwards");
}

#[test]
fn a_request_that_covers_a_high_severity_finding_is_confirmed_by_a_senior_not_a_reviewer() {
    let q = q();
    let (claim, draft) = claim_needing_a_request(true);
    let id = claim["claim_id"].as_str().unwrap().to_string();
    let rid = draft["request_id"].as_str().unwrap().to_string();
    q.intake.submit(&claim).unwrap();
    let d = q.store.get(&id, None).unwrap().unwrap();
    process(&q.rt, &id, 1, d["input_hash"].as_str().unwrap());
    q.lease_to(&id, "CG-2002");
    let denied = q.queue.decide_info_request(&q.p("CG-2002"), &id, &rid, true);
    assert!(matches!(denied, Err(QueueError::Forbidden(_))), "{denied:?}");
    assert!(q.store.request_events(&id, 10).unwrap().is_empty() && q.store.get(&id, None).unwrap().unwrap()["decisions"].as_array().unwrap().is_empty(), "nothing sent, nothing recorded");
}
