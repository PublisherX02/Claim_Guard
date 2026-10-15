//! The learning routes: who may use them, that a lone administrator activates directly, that two administrators need a second
//! signature, and that nothing is used for routing before then. The queue is the in-memory twin; the approval hook is the same shape as
//! the one `cg-cli` installs.

use cg_access::service::testkit::{world, World};
use cg_api::claimstore::FileClaimStore;
use cg_api::{Api, ApiParts, ApiRequest, ApiResponse};
use cg_audit::AuditLog;
use cg_queue::dispatcher::Dispatcher;
use cg_queue::reliability::{self, Counted, Params};
use cg_queue::service::{agents_from_access, QueueService};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PW: &str = "Correct-Horse-9";

struct T {
    api: Api,
    w: World,
    store: Arc<MemoryQueueStore>,
}

fn t() -> T {
    let w = world();
    for (b, l) in [("CG-4004", 4), ("CG-2002", 2), ("CG-3003", 3)] {
        w.provision(b, l, PW);
    }
    let store = Arc::new(MemoryQueueStore::new());
    let t0 = w.clock.clone();
    let clock: cg_access::Clock = Arc::new(move || t0.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0);
    let dispatcher = Arc::new(Dispatcher { store: store.clone(), agents: agents_from_access(w.svc.clone()), clock: clock.clone(), rng_seed: Some(3), log: Some(w.svc.log.clone()) });
    let review_log = Arc::new(AuditLog::open(w.dir.path().join("review.jsonl")).unwrap());
    let queue = Arc::new(QueueService::new(store.clone(), dispatcher, w.svc.clone(), w.svc.log.clone(), review_log.clone(), clock.clone()));
    let (hook_store, log) = (store.clone(), w.svc.clone());
    w.svc.set_action_hook(Arc::new(move |a, requester, approver| match a.action.as_str() {
        "learning_snapshot" => {
            let p: Value = serde_json::from_str(&a.payload).map_err(|e| e.to_string())?;
            reliability::activate(&*hook_store, p["version"].as_i64().ok_or("no version")?, requester, Some(approver), log.now())
        }
        _ => Err("unknown action".into()),
    }));
    let api = Api::new(ApiParts {
        access: w.svc.clone(),
        claims: Arc::new(FileClaimStore::new(vec![])),
        review_log,
        securitylog: w.svc.log.clone(),
        settings: cg_access::settings::test_settings(),
        clock,
        queue: Some(queue),
        ui: true,
        extra_probes: vec![],
        data_dir: w.dir.path().to_path_buf(),
        import: None,
        payer: None,
    });
    T { api, w, store }
}

/// A proposed snapshot in which R006 is doubtful (as `learn` would have stored it).
fn propose(store: &MemoryQueueStore, version: i64) {
    let counted: Vec<Counted> = (0..40)
        .map(|i| Counted { rule_id: "R006".into(), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", i % 4), at: i as f64, upheld: i < 6 })
        .collect();
    let mut snap = reliability::make_snapshot(&counted, &Params { min_decisions: 20, min_reviewers: 3, reviewer_cap: 1.0, doubtful_below: 0.6 }, version, 1.0, 1);
    snap["replay"] = json!({"claims": 10, "senior_before": 1, "senior_after": 4, "senior_share_before": 0.1, "senior_share_after": 0.4, "note": "x"});
    assert!(store.put_reliability(&snap, version - 1).unwrap());
}

fn j(r: &ApiResponse) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

fn cookie_of(r: &ApiResponse, name: &str) -> String {
    r.headers.iter().filter(|(n, _)| n == "set-cookie").find_map(|(_, v)| v.strip_prefix(&format!("{name}=")).map(|s| s.split(';').next().unwrap().to_string())).unwrap()
}

fn req(method: &str, path: &str, body: Option<Value>, s: Option<&(String, String)>) -> ApiRequest {
    let mut headers = HashMap::new();
    if let Some((c, csrf)) = s {
        headers.insert("cookie".to_string(), format!("cg_session={c}"));
        if method != "GET" {
            headers.insert("x-csrf-token".to_string(), csrf.clone());
        }
    }
    let body = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    if !body.is_empty() {
        headers.insert("content-type".into(), "application/json".into());
    }
    ApiRequest { method: method.into(), path: path.into(), query: String::new(), headers, body, client: "10.0.0.1".into() }
}

impl T {
    fn login(&self, badge: &str) -> (String, String) {
        let code = self.w.code(badge);
        let r = self.api.handle(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": badge, "password": PW, "totp": code})), None));
        assert_eq!(r.status, 200, "{:?}", j(&r));
        (cookie_of(&r, "cg_session"), j(&r)["csrf"].as_str().unwrap().to_string())
    }

    fn get(&self, path: &str, s: &(String, String)) -> ApiResponse {
        self.api.handle(req("GET", path, None, Some(s)))
    }

    fn post(&self, path: &str, body: Value, s: &(String, String)) -> ApiResponse {
        self.api.handle(req("POST", path, Some(body), Some(s)))
    }
}

#[test]
fn only_administrators_reach_the_learning_routes() {
    let t = t();
    propose(&t.store, 1);
    let routes = [("GET", "/api/v1/queue/reliability"), ("POST", "/api/v1/queue/reliability/learn"), ("GET", "/api/v1/queue/reliability/1/verify"), ("POST", "/api/v1/queue/reliability/1/activate")];
    for (m, path) in routes {
        assert_eq!(t.api.handle(req(m, path, if m == "POST" { Some(json!({})) } else { None }, None)).status, 401, "{path} without a session");
    }
    for badge in ["CG-2002", "CG-3003"] {
        let s = t.login(badge);
        for (m, path) in routes {
            let r = if m == "GET" { t.get(path, &s) } else { t.post(path, json!({}), &s) };
            assert_eq!(r.status, 403, "{badge} {path}");
        }
    }
    let admin = t.login("CG-4004");
    assert_eq!(t.get("/api/v1/queue/reliability", &admin).status, 200);
    let v = j(&t.get("/api/v1/queue/reliability/1/verify", &admin));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(t.get("/api/v1/queue/reliability/9/verify", &admin).status, 404);
    assert_eq!(t.get("/api/v1/queue/reliability/abc/verify", &admin).status, 404);
    assert_eq!(t.post("/api/v1/queue/reliability/9/activate", json!({}), &admin).status, 404);
    assert_eq!(t.post("/api/v1/queue/reliability/learn", json!({}), &admin).status, 200);
}

#[test]
fn a_lone_administrator_activates_directly_and_a_second_snapshot_supersedes_the_first() {
    let t = t();
    propose(&t.store, 1);
    let admin = t.login("CG-4004");
    let r = t.post("/api/v1/queue/reliability/1/activate", json!({}), &admin);
    assert_eq!((r.status, j(&r)["status"].clone()), (200, json!("applied")), "{:?}", j(&r));
    assert_eq!(reliability::active(&*t.store).unwrap()["version"], 1);
    assert_eq!(t.post("/api/v1/queue/reliability/1/activate", json!({}), &admin).status, 409, "an already activated snapshot is not proposed any more");
    let shown = j(&t.get("/api/v1/queue/reliability", &admin));
    assert_eq!(shown["active"]["version"], 1);
    assert_eq!(shown["active"]["verified"], true);
}

#[test]
fn with_two_administrators_nothing_is_used_until_the_second_one_approves() {
    let t = t();
    t.w.provision("CG-4005", 4, PW);
    propose(&t.store, 1);
    let (first, second) = (t.login("CG-4004"), t.login("CG-4005"));
    let asked = t.post("/api/v1/queue/reliability/1/activate", json!({}), &first);
    assert_eq!(asked.status, 202, "{:?}", j(&asked));
    assert_eq!(j(&asked)["status"], "pending_approval");
    assert!(reliability::active(&*t.store).is_none(), "asking changes nothing");
    let id = j(&asked)["approval"]["id"].as_str().unwrap().to_string();
    assert!(j(&asked)["approval"]["summary"].to_string().contains("R006"), "the second administrator reads which rules become doubtful");
    // the requester cannot approve their own request
    let own = t.w.code("CG-4004");
    assert_eq!(t.post(&format!("/api/v1/approvals/{id}/approve"), json!({"totp_code": own}), &first).status, 403);
    assert!(reliability::active(&*t.store).is_none());
    let code = t.w.code("CG-4005");
    let done = t.post(&format!("/api/v1/approvals/{id}/approve"), json!({"totp_code": code}), &second);
    assert_eq!(done.status, 200, "{:?}", j(&done));
    let active = reliability::active(&*t.store).expect("approved by the second administrator");
    assert_eq!(active["version"], 1);
    let h = &t.store.reliability_history().unwrap()[0];
    assert_eq!((h["history"][0]["actor"].as_str(), h["history"][0]["detail"]["requested_by"].as_str(), h["history"][0]["detail"]["approved_by"].as_str()), (Some("CG-4005"), Some("CG-4004"), Some("CG-4005")));
}
