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
fn a_reason_is_required_and_a_decision_is_idempotent_only_once() {
    let q = q();
    let id = q.ready("medium");
    q.lease_to(&id, "CG-2002");
    let who = q.p("CG-2002");
    let rules = q.flagged_rules(&id);
    let rule = rules[0].0.clone();
    assert!(matches!(q.queue.decide_finding(&who, &id, &rule, "confirm_issue", "   "), Err(QueueError::Invalid(_))));
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

#[test]
fn routing_by_reliability_is_off_by_default_and_a_doubtful_rule_sends_a_medium_claim_to_a_senior() {
    let q = q();
    let admin = q.p("CG-4004");
    let c = q.find("medium");
    let mut errs = vec![];
    let results = serde_json::to_value(engine().review(&c, &mut errs).unwrap()).unwrap();
    let rule = triage::flagged(&results)[0]["rule_id"].as_str().unwrap().to_string();
    // an approved snapshot in which that rule is doubtful
    let p = cg_queue::reliability::Params::from_config(&RoutingConfig::default());
    let snap = json!({"version": 1, "status": "proposed", "created_at": 0.0, "config_version": 1, "parameters": p.to_doc(), "decisions": 40, "inputs_hash": "x",
                      "rules": {rule.clone(): cg_queue::reliability::row(4, 36, 5, &p)}});
    assert!(q.store.put_reliability(&snap, 0).unwrap());
    q.store.set_reliability_status(1, "proposed", "approved", "CG-4004", 1.0, None).unwrap();
    // off: nothing changes
    let r = q.intake.submit(&c).unwrap();
    assert_eq!(r["eligibility"], "decide");
    assert!(r.get("reliability_version").is_none(), "with the feature off the receipt gains no field");
    // on: the same body under a new claim id goes to a senior
    let mut set = serde_json::Map::new();
    set.insert("route_by_reliability".into(), json!(true));
    q.queue.set_config(&admin, &set, 1).unwrap();
    let mut c2 = c.clone();
    c2["claim_id"] = json!("CG-LEARN-1");
    let r2 = q.intake.submit(&c2).unwrap();
    assert_eq!((r2["eligibility"].as_str(), r2["doubtful_rules"].clone(), r2["reliability_version"].as_i64()), (Some("decide_high"), json!([rule]), Some(1)));
    assert_eq!((r2["lane"].clone(), r2["score"].clone()), (r["lane"].clone(), r["score"].clone()), "lane and score are untouched");
    // a stored snapshot that was altered after approval is ignored for routing
    let mut bad = q.store.reliability_history().unwrap()[0].clone();
    bad["rules"][&rule]["status"] = json!("measured");
    assert!(cg_queue::reliability::verify(&bad).is_err());
}
