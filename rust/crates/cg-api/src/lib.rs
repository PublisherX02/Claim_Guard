//! The reviewer HTTP API (`src/access/api.py`, `src/workqueue/api.py`).
//!
//! `Api::handle` is a plain function from a request to a response, with no web framework in it: it only translates requests into calls on
//! the access and queue services and shapes the answers, so every rule below is tested directly, without a socket. `server` puts it
//! behind axum.
//!
//! Rules this layer enforces on every request:
//! * a session cookie (HttpOnly, SameSite=Strict) authenticates; every unsafe method must also echo the CSRF token signed into that session;
//! * permissions are checked per endpoint and what a caller may not see is absent from the response, not flagged;
//! * the reviewer recorded on a decision is the session's badge, never anything the client sends;
//! * bodies are small, JSON only, strictly typed with unknown fields refused, and errors never echo input or internals.

pub mod claimstore;
pub mod import;
pub mod ops;
pub mod server;

use cg_access::masking;
use cg_access::permissions::{self, Perms};
use cg_access::securitylog::SecurityLog;
use cg_access::service::{AccessService, AuthError, Changes, Forbidden, Gate, Principal, ServiceError};
use cg_access::store::Approval;
use cg_access::settings::Settings;
use cg_access::Clock;
use cg_audit::AuditLog;
use cg_queue::service::{QueueError, QueueService};
use claimstore::{review_state, FileClaimStore, UI_CSP, UI_PREFIX};
use ops::{AuditStatus, Metrics, Probe};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

pub const API: &str = "/api/v1";
pub const SESSION_COOKIE: &str = "cg_session";
pub const CSRF_COOKIE: &str = "cg_csrf";
pub const MAX_BODY_BYTES: usize = 65_536;
/// The import routes take whole files (a scanned page, a FHIR export): a larger body, on those two paths only.
pub const MAX_UPLOAD_BYTES: usize = 16 * 1024 * 1024;
pub const IMPORT_FILE_PATH: &str = "/api/v1/queue/import/file";
pub const IMPORT_CSV_PATH: &str = "/api/v1/queue/import/csv";
/// One submission is one batch of up to 1,000 claims (docs/53), so this route takes a larger body than the others (about 1.5 KB a claim).
pub const SUBMIT_PATH: &str = "/api/v1/queue/submit";
pub const MAX_SUBMIT_BYTES: usize = 4 * 1024 * 1024;

pub fn body_limit(path: &str) -> usize {
    if path == IMPORT_FILE_PATH || path == IMPORT_CSV_PATH { MAX_UPLOAD_BYTES } else if path == SUBMIT_PATH { MAX_SUBMIT_BYTES } else { MAX_BODY_BYTES }
}
/// Failed sign-ins one client address may make, over all badges, in one window. (Per badge the service allows far fewer: PAIR_LIMIT.)
pub const LOGIN_FAILURE_LIMIT: i64 = 100;
pub const LOGIN_FAILURE_WINDOW: f64 = 900.0;
const ACTIONS: [&str; 4] = ["confirm_issue", "dismiss_with_reason", "request_information", "mark_corrected_for_recheck"];
const SAFE_METHODS: [&str; 3] = ["GET", "HEAD", "OPTIONS"];
const PASSWORD_CHANGE_PATHS: [&str; 3] = ["/api/v1/auth/me", "/api/v1/auth/logout", "/api/v1/auth/change-password"];

pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Header names in lower case.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub client: String,
}

pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ApiResponse {
    fn json(status: u16, v: &Value) -> ApiResponse {
        ApiResponse { status, headers: vec![("content-type".into(), "application/json".into())], body: v.to_string().into_bytes() }
    }

    fn with_cookie(mut self, c: String) -> ApiResponse {
        self.headers.push(("set-cookie".into(), c));
        self
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: u16,
    code: String,
    headers: Vec<(String, String)>,
    extra: Map<String, Value>,
}

impl ApiError {
    fn new(status: u16, code: &str) -> ApiError {
        ApiError { status, code: code.into(), headers: vec![], extra: Map::new() }
    }

    fn with(mut self, k: &str, v: Value) -> ApiError {
        self.extra.insert(k.into(), v);
        self
    }

    fn response(self) -> ApiResponse {
        let mut body = json!({"error": self.code});
        body.as_object_mut().map(|o| o.extend(self.extra));
        let mut r = ApiResponse::json(self.status, &body);
        r.headers.extend(self.headers);
        r
    }
}

type R = Result<ApiResponse, ApiError>;

pub struct Api {
    pub access: Arc<AccessService>,
    pub claims: Arc<FileClaimStore>,
    pub review_log: Arc<AuditLog>,
    pub securitylog: Arc<SecurityLog>,
    pub settings: Settings,
    pub clock: Clock,
    pub queue: Option<Arc<QueueService>>,
    pub ui: bool,
    /// How many claims one person may unmask per hour (UNMASK_PER_HOUR, default 60).
    pub unmask_per_hour: i64,
    pub metrics: Arc<Metrics>,
    probes: Vec<Probe>,
    audit_status: Arc<AuditStatus>,
    import: Option<import::ImportParts>,
    payer: Option<Arc<cg_engine::payer::PayerRules>>,
    doc_slots: import::DocumentSlots,
    ledger: Arc<ops::LedgerView>,
    ui_files: BTreeMap<&'static str, (&'static [u8], &'static str)>,
}

pub struct ApiParts {
    pub access: Arc<AccessService>,
    pub claims: Arc<FileClaimStore>,
    pub review_log: Arc<AuditLog>,
    pub securitylog: Arc<SecurityLog>,
    pub settings: Settings,
    pub clock: Clock,
    pub queue: Option<Arc<QueueService>>,
    pub ui: bool,
    /// Probes of the queue side (see `ops::queue_probes`); the access-side probes are added here.
    pub extra_probes: Vec<Probe>,
    pub data_dir: PathBuf,
    /// The document reader and the catalogue for importing claims from files; None keeps the import routes to the plain text formats.
    pub import: Option<import::ImportParts>,
    /// The editable payer rules (docs/45); None leaves the routes answering 404.
    pub payer: Option<Arc<cg_engine::payer::PayerRules>>,
}

// ---- request bodies: strict types, unknown fields refused ----------------------------------------------------------------------------
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    badge_id: String,
    password: String,
    totp: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePasswordBody {
    old_password: String,
    new_password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WhatIfBody {
    changes: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkRecheckBody {
    changes: Value,
    reason: String,
    #[serde(default)]
    rule_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBody {
    action: String,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkBody {
    rule_id: String,
    claim_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReasonBody {
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecheckBody {
    rule_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUserBody {
    badge_id: String,
    name: String,
    password: String,
    level: i64,
    #[serde(default)]
    grants: Vec<String>,
    #[serde(default)]
    revokes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyBody {
    action: String,
    /// For a claim of a batch, `escalate` names the rule that should have flagged it (R001 to R015 or "other"). Ignored for a claim on its own.
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InfoRequestBody {
    request_id: String,
    /// "send" (the officer confirms) or "decline".
    action: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchDecisionBody {
    action: String,
    #[serde(default)]
    n: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitBody {
    claims: Vec<Value>,
    /// true: answer at once with a job id (202) and take the claims in in the background; poll GET /api/v1/queue/submit/{job}
    #[serde(default, rename = "async")]
    run_async: bool,
}

fn too_long(s: &str, max: usize) -> bool {
    s.chars().count() > max
}

fn invalid() -> ApiError {
    ApiError::new(422, "invalid_request")
}

fn body_of<T: serde::de::DeserializeOwned>(req: &ApiRequest) -> Result<T, ApiError> {
    serde_json::from_slice(&req.body).map_err(|_| invalid())
}

fn object_body(req: &ApiRequest) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(&req.body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(invalid()),
    }
}

fn cookie(req: &ApiRequest, name: &str) -> Option<String> {
    req.headers.get("cookie")?.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn query(req: &ApiRequest) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(req.query.as_bytes()) {
        out.entry(k.into_owned()).or_insert_with(|| v.into_owned());
    }
    out
}

fn q_int(q: &HashMap<String, String>, name: &str, default: i64, low: i64, high: i64) -> Result<i64, ApiError> {
    match q.get(name) {
        None => Ok(default),
        Some(v) => v.trim().parse::<i64>().ok().filter(|n| (low..=high).contains(n)).ok_or_else(invalid),
    }
}

fn q_text(q: &HashMap<String, String>, name: &str, max: usize) -> Result<Option<String>, ApiError> {
    match q.get(name) {
        None => Ok(None),
        Some(v) if v.chars().count() <= max => Ok(Some(v.clone())),
        Some(_) => Err(invalid()),
    }
}

fn q_float(q: &HashMap<String, String>, name: &str) -> Result<Option<f64>, ApiError> {
    match q.get(name) {
        None => Ok(None),
        Some(v) => v.trim().parse::<f64>().ok().filter(|f| f.is_finite() && (0.0..=4e9).contains(f)).map(Some).ok_or_else(invalid),
    }
}

fn q_bool(q: &HashMap<String, String>, name: &str) -> Result<bool, ApiError> {
    match q.get(name).map(|s| s.to_lowercase()).as_deref() {
        None => Ok(false),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => Err(invalid()),
    }
}

fn fmt_cookie(name: &str, value: &str, max_age: i64, http_only: bool, secure: bool) -> String {
    format!("{name}={value}; Max-Age={max_age}; Path=/; SameSite=Strict{}{}", if http_only { "; HttpOnly" } else { "" }, if secure { "; Secure" } else { "" })
}

impl Api {
    pub fn new(p: ApiParts) -> Api {
        let metrics = Arc::new(Metrics::new(p.clock.clone()));
        let (sl, rl) = (p.securitylog.clone(), p.review_log.clone());
        let audit_status = Arc::new(AuditStatus::new(
            Box::new(move || {
                let review = if !rl.path().exists() {
                    json!({"ok": true, "events": 0})
                } else {
                    match cg_audit::verify_with_anchor(rl.path(), None, false) {
                        Ok((_, count)) => json!({"ok": true, "events": count}),
                        Err(e) => json!({"ok": false, "error": e.to_string().chars().take(300).collect::<String>()}),
                    }
                };
                json!({"security": sl.verify(), "review": review})
            }),
            p.clock.clone(),
        ));
        let ledger = Arc::new(ops::LedgerView::new(p.data_dir.clone(), vec![("security".into(), p.securitylog.path().to_path_buf()), ("review".into(), p.review_log.path().to_path_buf())]));
        let mut probes = ops::access_probes(p.access.clone(), p.securitylog.clone(), audit_status.clone(), p.data_dir.clone(), metrics.clone(), p.clock.clone(), ledger.clone());
        probes.extend(p.extra_probes);
        Api {
            access: p.access,
            claims: p.claims,
            review_log: p.review_log,
            securitylog: p.securitylog,
            settings: p.settings,
            clock: p.clock,
            queue: p.queue,
            ui: p.ui,
            unmask_per_hour: std::env::var("UNMASK_PER_HOUR").ok().and_then(|v| v.trim().parse::<i64>().ok()).filter(|n| *n >= 1).unwrap_or(60),
            metrics,
            probes,
            audit_status,
            import: p.import,
            payer: p.payer,
            doc_slots: import::DocumentSlots::default(),
            ledger,
            ui_files: claimstore::ui_files(),
        }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- the layers around every request ------------------------------------------------------------------------------
    pub fn handle(&self, req: ApiRequest) -> ApiResponse {
        self.handle_with(req, true)
    }

    /// The same layers, but not counted in the traffic figures: the live-update stream asks the same question every few seconds on behalf of
    /// each open console, and those internal checks would drown the real traffic in the health panel.
    pub fn handle_quiet(&self, req: ApiRequest) -> ApiResponse {
        self.handle_with(req, false)
    }

    fn handle_with(&self, req: ApiRequest, record: bool) -> ApiResponse {
        let started = std::time::Instant::now();
        let mut resp = self.guarded(&req);
        let headers: Vec<(&str, &str)> = vec![
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("x-frame-options", "DENY"),
            ("referrer-policy", "no-referrer"),
            ("permissions-policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), interest-cohort=()"),
            ("cross-origin-opener-policy", "same-origin"),
            ("cross-origin-resource-policy", "same-origin"),
        ];
        resp.headers.retain(|(n, _)| !headers.iter().any(|(h, _)| h == n) && n != "content-security-policy");
        for (n, v) in headers {
            resp.headers.push((n.into(), v.into()));
        }
        let csp = if req.path.starts_with(UI_PREFIX) { UI_CSP } else { "default-src 'none'" };
        resp.headers.push(("content-security-policy".into(), csp.into()));
        if record {
            self.metrics.record(&req.method, &req.path, resp.status, started.elapsed().as_secs_f64());
        }
        resp
    }

    /// Reads at most MAX_BODY_BYTES, insists on JSON for bodies, and turns any unexpected panic into a plain 500 with no details.
    fn guarded(&self, req: &ApiRequest) -> ApiResponse {
        let reply = |s: u16, c: &str| ApiError::new(s, c).response();
        let cl = req.headers.get("content-length");
        if cl.is_some_and(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit())) {
            return reply(400, "bad_request");
        }
        let limit = body_limit(&req.path);
        if cl.and_then(|v| v.parse::<usize>().ok()).is_some_and(|n| n > limit) || req.body.len() > limit {
            return reply(413, "payload_too_large");
        }
        if ["POST", "PUT", "PATCH"].contains(&req.method.as_str()) {
            let has_body = cl.is_some_and(|v| v != "0") || req.headers.contains_key("transfer-encoding") || !req.body.is_empty();
            let ctype = req.headers.get("content-type").map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase()).unwrap_or_default();
            if has_body && ctype != "application/json" && req.path != IMPORT_FILE_PATH {
                return reply(415, "unsupported_media_type");
            }
        }
        if !SAFE_METHODS.contains(&req.method.as_str()) && Self::cross_site(req) {
            return reply(403, "cross_site");
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.route(req))) {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => e.response(),
            Err(_) => reply(500, "server_error"),
        }
    }

    /// A state-changing request that the browser itself says comes from another site is refused before anything else looks at it. The
    /// anti-forgery token and the SameSite cookie already stop it; this is a third, independent check. Requests from programs (no `Origin`,
    /// no `Sec-Fetch-Site`) are not affected.
    fn cross_site(req: &ApiRequest) -> bool {
        if req.headers.get("sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
            return true;
        }
        match req.headers.get("origin") {
            None => false,
            Some(o) => {
                let host = o.split_once("://").map(|(_, rest)| rest.split('/').next().unwrap_or("")).unwrap_or("");
                host.is_empty() || !req.headers.get("host").is_some_and(|h| h.eq_ignore_ascii_case(host))
            }
        }
    }

    // ---- dependencies ---------------------------------------------------------------------------------------------------
    fn principal(&self, req: &ApiRequest) -> Result<Principal, ApiError> {
        let token = cookie(req, SESSION_COOKIE).ok_or_else(|| ApiError::new(401, "unauthenticated"))?;
        let p = self.access.authenticate(&token).map_err(|e| if e == AuthError::Unavailable { ApiError::new(503, "unavailable") } else { ApiError::new(401, "unauthenticated") })?;
        if !SAFE_METHODS.contains(&req.method.as_str()) && !constant_time_eq(req.headers.get("x-csrf-token").map(String::as_str).unwrap_or(""), &p.csrf) {
            return Err(ApiError::new(403, "csrf"));
        }
        if p.user.must_change_password && !PASSWORD_CHANGE_PATHS.contains(&req.path.as_str()) {
            return Err(ApiError::new(403, "password_change_required"));
        }
        Ok(p)
    }

    fn forbid(&self, req: &ApiRequest, p: &Principal) -> ApiError {
        self.access.note_forbidden(p.badge(), &req.method, &req.path);
        ApiError::new(403, "forbidden")
    }

    fn need(&self, req: &ApiRequest, perm: &str) -> Result<Principal, ApiError> {
        let p = self.principal(req)?;
        if p.permissions.contains(perm) { Ok(p) } else { Err(self.forbid(req, &p)) }
    }

    fn serr(&self, e: ServiceError, req: &ApiRequest, p: &Principal) -> ApiError {
        match e {
            ServiceError::Forbidden(Forbidden(_)) | ServiceError::Permission(permissions::PermissionError::Denied(_)) => self.forbid(req, p),
            ServiceError::Duplicate => ApiError::new(409, "conflict"),
            ServiceError::NotFound => ApiError::new(404, "not_found"),
            ServiceError::Invalid(m) | ServiceError::Permission(permissions::PermissionError::Invalid(m)) => invalid().with("detail", json!(m.chars().take(300).collect::<String>())),
            ServiceError::Auth(AuthError::Unavailable) => ApiError::new(503, "unavailable"),
            ServiceError::Auth(_) => ApiError::new(401, "invalid_credentials"),
            ServiceError::Throttled => {
                self.access.note_forbidden(p.badge(), &req.method, &format!("{} (too many wrong codes)", req.path));
                let mut e = ApiError::new(429, "too_many_attempts");
                e.headers.push(("retry-after".into(), "900".into()));
                e
            }
        }
    }

    fn qerr(&self, e: QueueError, req: &ApiRequest, p: &Principal) -> ApiError {
        match e {
            QueueError::Forbidden(_) => self.forbid(req, p),
            QueueError::Conflict(m) => ApiError::new(409, "conflict").with("reason", json!(m.chars().take(60).collect::<String>())),
            QueueError::NotFound(_) => ApiError::new(404, "not_found"),
            QueueError::Invalid(m) => invalid().with("detail", json!(m.chars().take(300).collect::<String>())),
            QueueError::Auth(AuthError::Unavailable) => ApiError::new(503, "unavailable"),
            QueueError::Auth(_) => ApiError::new(401, "invalid_credentials"),
            QueueError::Redaction => ApiError::new(500, "server_error"),
        }
    }

    fn queue(&self) -> Result<&Arc<QueueService>, ApiError> {
        self.queue.as_ref().ok_or_else(|| ApiError::new(404, "not_found"))
    }

    // ---- routing --------------------------------------------------------------------------------------------------------
    fn route(&self, req: &ApiRequest) -> R {
        let path = req.path.as_str();
        if path == "/" || path == "/ui" {
            return if self.ui { Ok(ApiResponse { status: 307, headers: vec![("location".into(), "/ui/".into())], body: vec![] }) } else { Err(ApiError::new(404, "not_found")) };
        }
        if let Some(name) = path.strip_prefix(UI_PREFIX) {
            return self.ui_file(name, req);
        }
        if path == "/healthz" {
            return Ok(if self.access.store.ping() { ApiResponse::json(200, &json!({"status": "ok"})) } else { ApiResponse::json(503, &json!({"status": "degraded"})) });
        }
        let Some(rest) = path.strip_prefix("/api/v1/") else { return Err(ApiError::new(404, "not_found")) };
        let seg: Vec<&str> = rest.split('/').collect();
        let m = req.method.as_str();
        let wrong = || ApiError::new(405, "method_not_allowed");
        match seg.as_slice() {
            ["auth", "login"] => if m == "POST" { self.login(req) } else { Err(wrong()) },
            ["auth", "logout"] => if m == "POST" { self.logout(req) } else { Err(wrong()) },
            ["auth", "me"] => if m == "GET" { self.me(req) } else { Err(wrong()) },
            ["auth", "change-password"] => if m == "POST" { self.change_password(req) } else { Err(wrong()) },
            ["claims"] => if m == "GET" { self.list_claims(req) } else { Err(wrong()) },
            ["claims", id] => if m == "GET" { self.get_claim(req, id) } else { Err(wrong()) },
            ["claims", id, "unmask"] => if m == "POST" { self.unmask(req, id) } else { Err(wrong()) },
            ["claims", id, "recheck"] => if m == "POST" { self.recheck(req, id) } else { Err(wrong()) },
            ["claims", id, "findings", rule, "decision"] => if m == "POST" { self.decide_direct(req, id, rule) } else { Err(wrong()) },
            ["audit", "events"] => if m == "GET" { self.audit_events(req) } else { Err(wrong()) },
            ["audit", "verify"] => if m == "GET" { self.audit_verify(req) } else { Err(wrong()) },
            ["knowledge", "search"] => if m == "GET" { self.knowledge(req) } else { Err(wrong()) },
            ["rules", "payer"] => match m { "GET" => self.payer_get(req), "POST" => self.payer_post(req), _ => Err(wrong()) },
            ["rules", "payer", "validate"] => if m == "POST" { self.payer_validate(req) } else { Err(wrong()) },
            ["ledger"] => if m == "GET" { self.ledger_status(req) } else { Err(wrong()) },
            ["ledger", "proof"] => if m == "GET" { self.ledger_proof(req) } else { Err(wrong()) },
            ["ops", "audit", "summary"] => if m == "GET" { self.audit_overview(req) } else { Err(wrong()) },
            ["ops", "health"] => if m == "GET" { self.health(req) } else { Err(wrong()) },
            ["ops", "trace", id] => if m == "GET" { self.trace(req, id) } else { Err(wrong()) },
            ["users"] => match m { "GET" => self.users_list(req), "POST" => self.users_create(req), _ => Err(wrong()) },
            ["users", badge] => if m == "PATCH" { self.users_patch(req, badge) } else { Err(wrong()) },
            ["users", badge, "unlock"] => if m == "POST" { self.users_unlock(req, badge) } else { Err(wrong()) },
            ["users", badge, "reset-totp"] => if m == "POST" { self.users_reset_totp(req, badge) } else { Err(wrong()) },
            ["approvals"] => if m == "GET" { self.approvals_list(req) } else { Err(wrong()) },
            ["approvals", id, "approve"] => if m == "POST" { self.approvals_approve(req, id) } else { Err(wrong()) },
            ["approvals", id, "reject"] => if m == "POST" { self.approvals_reject(req, id) } else { Err(wrong()) },
            ["approvals", id, "cancel"] => if m == "POST" { self.approvals_cancel(req, id) } else { Err(wrong()) },
            ["work", "inbox"] if self.queue.is_some() => if m == "GET" { self.work_inbox(req) } else { Err(wrong()) },
            ["work", "changes"] if self.queue.is_some() => if m == "GET" { self.work_changes(req) } else { Err(wrong()) },
            ["work", "next"] if self.queue.is_some() => if m == "POST" { self.work_next(req) } else { Err(wrong()) },
            ["work", "heartbeat"] if self.queue.is_some() => if m == "POST" { self.work_heartbeat(req) } else { Err(wrong()) },
            ["work", "claims", id, "findings", rule, "decision"] if self.queue.is_some() => if m == "POST" { self.work_decide(req, id, rule) } else { Err(wrong()) },
            ["work", "claims", id, "verify"] if self.queue.is_some() => if m == "POST" { self.work_verify(req, id) } else { Err(wrong()) },
            ["work", "claims", id, "info-request"] if self.queue.is_some() => if m == "POST" { self.work_info_request(req, id) } else { Err(wrong()) },
            ["work", "claims", id, "whatif"] if self.queue.is_some() => if m == "POST" { self.work_whatif(req, id) } else { Err(wrong()) },
            ["work", "claims", id, "recheck"] if self.queue.is_some() => if m == "POST" { self.work_recheck(req, id) } else { Err(wrong()) },
            ["work", "bulk-confirm"] if self.queue.is_some() => if m == "POST" { self.work_bulk_confirm(req) } else { Err(wrong()) },
            ["work", "bulk-confirm"] if self.queue.is_some() => if m == "POST" { self.work_bulk_confirm(req) } else { Err(wrong()) },
            ["queue", "dashboard"] if self.queue.is_some() => if m == "GET" { self.q_dashboard(req) } else { Err(wrong()) },
            ["queue", "feedback"] if self.queue.is_some() => if m == "GET" { self.q_feedback(req) } else { Err(wrong()) },
            ["queue", "import", "file"] => if m == "POST" { self.import_file(req) } else { Err(wrong()) },
            ["queue", "import", "csv"] => if m == "POST" { self.import_csv(req) } else { Err(wrong()) },
            ["queue", "import", "validate"] => if m == "POST" { self.import_validate(req) } else { Err(wrong()) },
            ["queue", "batches"] if self.queue.is_some() => if m == "GET" { self.q_batches(req) } else { Err(wrong()) },
            ["queue", "batches", id] if self.queue.is_some() => if m == "GET" { self.q_batch(req, id) } else { Err(wrong()) },
            ["queue", "batches", id, "decision"] if self.queue.is_some() => if m == "POST" { self.q_batch_decision(req, id) } else { Err(wrong()) },
            ["queue", "submit"] if self.queue.is_some() => if m == "POST" { self.q_submit(req) } else { Err(wrong()) },
            ["queue", "submit", id] if self.queue.is_some() => if m == "GET" { self.q_submit_job(req, id) } else { Err(wrong()) },
            ["queue", "config"] if self.queue.is_some() => match m { "GET" => self.q_get_config(req), "PATCH" => self.q_patch_config(req), _ => Err(wrong()) },
            _ => Err(ApiError::new(404, "not_found")),
        }
    }

    fn ui_file(&self, name: &str, _req: &ApiRequest) -> R {
        if !self.ui {
            return Err(ApiError::new(404, "not_found"));
        }
        let name = if name.is_empty() { "index.html" } else { name };
        match self.ui_files.get(name) {
            Some((bytes, ctype)) => Ok(ApiResponse { status: 200, headers: vec![("content-type".into(), (*ctype).into())], body: bytes.to_vec() }),
            None => Ok(ApiResponse { status: 404, headers: vec![("content-type".into(), "application/json".into())], body: br#"{"error": "not_found"}"#.to_vec() }),
        }
    }

    // ---- authentication -------------------------------------------------------------------------------------------------
    /// The per-address failure counter lives in the shared database, so a restart or a second server process does not clear it.
    fn ip_key(&self, client: &str) -> String {
        format!("ip|{}", self.access.client_key(client))
    }

    fn note_failed_sign_in(&self, client: &str, now: f64) {
        // best effort: if the counter cannot be written the sign-in itself is refused anyway
        let _ = self.access.store.bump_counter(&self.ip_key(client), LOGIN_FAILURE_WINDOW as u64, now);
    }

    fn login(&self, req: &ApiRequest) -> R {
        let now = self.now();
        let client = req.client.as_str();
        let unavailable = |_| ApiError::new(503, "unavailable");
        let body: LoginBody = match body_of::<LoginBody>(req) {
            Ok(b) if !(too_long(&b.badge_id, 64) || too_long(&b.password, 128) || too_long(&b.totp, 12)) => b,
            _ => {
                self.note_failed_sign_in(client, now); // never echo the input: it may hold a password
                return Err(invalid());
            }
        };
        if self.access.store.counter(&self.ip_key(client), LOGIN_FAILURE_WINDOW as u64, now).map_err(unavailable)? >= LOGIN_FAILURE_LIMIT {
            let mut e = ApiError::new(429, "too_many_attempts");
            e.headers.push(("retry-after".into(), (LOGIN_FAILURE_WINDOW as i64).to_string()));
            return Err(e);
        }
        match self.access.login(&body.badge_id, &body.password, &body.totp, client) {
            Ok(s) => Ok(ApiResponse::json(200, &json!({"expires_at": s.expires_at, "csrf": s.csrf, "must_change_password": s.must_change_password}))
                .with_cookie(fmt_cookie(SESSION_COOKIE, &s.token, self.settings.token_ttl_seconds, true, self.settings.cookie_secure))
                .with_cookie(fmt_cookie(CSRF_COOKIE, &s.csrf, self.settings.token_ttl_seconds, false, self.settings.cookie_secure))),
            Err(AuthError::Unavailable) => Err(ApiError::new(503, "unavailable")),
            Err(_) => {
                self.note_failed_sign_in(client, now);
                Err(ApiError::new(401, "invalid_credentials"))
            }
        }
    }

    fn logout(&self, req: &ApiRequest) -> R {
        self.principal(req)?;
        self.access.logout(&cookie(req, SESSION_COOKIE).unwrap_or_default());
        let clear = |n: &str| format!("{n}=; Max-Age=0; Path=/; SameSite=Strict");
        Ok(ApiResponse::json(200, &json!({"status": "logged_out"})).with_cookie(clear(SESSION_COOKIE)).with_cookie(clear(CSRF_COOKIE)))
    }

    fn me(&self, req: &ApiRequest) -> R {
        let p = self.principal(req)?;
        let perms: Vec<&str> = p.permissions.iter().copied().collect();
        Ok(ApiResponse::json(200, &json!({"badge_id": p.user.badge_id, "name": p.user.name, "level": p.user.level, "permissions": perms, "must_change_password": p.user.must_change_password})))
    }

    fn change_password(&self, req: &ApiRequest) -> R {
        let p = self.principal(req)?;
        let b: ChangePasswordBody = body_of(req).ok().filter(|b: &ChangePasswordBody| !(too_long(&b.old_password, 128) || too_long(&b.new_password, 128))).ok_or_else(invalid)?;
        self.access.change_password(&p, &b.old_password, &b.new_password).map_err(|e| self.serr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"status": "changed"})))
    }

    // ---- claims ---------------------------------------------------------------------------------------------------------
    fn finding_actions(p: &Principal, finding: &Value) -> Option<Vec<&'static str>> {
        if !finding["status"].as_str().is_some_and(|s| s == "FAIL" || s == "UNABLE_TO_ASSESS") || !p.permissions.contains("claims.decide") {
            return None;
        }
        if finding["severity"] == "high" && !p.permissions.contains("claims.decide_high") {
            return None;
        }
        Some(ACTIONS.to_vec())
    }

    fn claim_view(&self, p: &Principal, claim: &Value, results: &Value, unmasked: bool) -> Result<Value, ApiError> {
        let (claim_out, mut results_out) = masking::shape_claim(claim, results, &p.permissions, &self.settings.pii_key, unmasked, true).map_err(|_| ApiError::new(500, "server_error"))?;
        let state = review_state(results, self.review_log.path());
        let by_rule: BTreeMap<&str, &Value> = results.as_array().into_iter().flatten().filter_map(|r| r["rule_id"].as_str().map(|id| (id, r))).collect();
        let claim_id = claim["claim_id"].as_str().unwrap_or("").to_string();
        for f in results_out.as_array_mut().into_iter().flatten() {
            let rid = f["rule_id"].as_str().unwrap_or("").to_string();
            if let Some(s) = state.get(&(claim_id.clone(), rid.clone())) {
                f["review_state"] = json!(s);
            }
            if let Some(orig) = by_rule.get(rid.as_str()) {
                if let Some(a) = Self::finding_actions(p, orig) {
                    f["allowed_actions"] = json!(a);
                }
            }
        }
        let mut view = json!({"claim": claim_out, "findings": results_out});
        let actions: Vec<&str> = [("unmask", "pii.unmask"), ("recheck", "claims.recheck")].iter().filter(|(_, perm)| p.permissions.contains(perm)).map(|(a, _)| *a).collect();
        if !actions.is_empty() {
            view["allowed_actions"] = json!(actions);
        }
        Ok(view)
    }

    fn list_claims(&self, req: &ApiRequest) -> R {
        self.need(req, "claims.view")?;
        let q = query(req);
        let (limit, offset) = (q_int(&q, "limit", 50, 1, 200)?, q_int(&q, "offset", 0, 0, i64::MAX)?);
        let (status, rule) = (q_text(&q, "status", 20)?, q_text(&q, "rule_id", 10)?);
        let rows = self.claims.summaries(limit as usize, offset as usize, status.as_deref(), rule.as_deref()).map_err(|_| invalid())?;
        Ok(ApiResponse::json(200, &json!({"claims": rows})))
    }

    fn get_claim(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "claims.view")?;
        let (c, r) = self.claims.get(id).ok_or_else(|| ApiError::new(404, "not_found"))?;
        Ok(ApiResponse::json(200, &self.claim_view(&p, &c, &r, false)?))
    }

    fn unmask(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "pii.unmask")?;
        let b: ReasonBody = body_of(req).ok().filter(|b: &ReasonBody| !b.reason.is_empty() && !too_long(&b.reason, 500)).ok_or_else(invalid)?;
        let (c, r) = self.claims.get(id).ok_or_else(|| ApiError::new(404, "not_found"))?;
        if cg_access::masking::text_leaks_identifier(&b.reason, &c, &self.settings.pii_key) {
            return Err(invalid().with("detail", json!("the reason must not contain the patient or member identifier")));
        }
        // Unmasking is how personal data leaves the system, so it is rationed per person: a reviewer who unmasks hundreds of claims an
        // hour is copying, not reviewing. The count is shared by every server process.
        let used = self.access.store.bump_counter(&format!("unmask|{}", p.badge()), 3600, self.now()).map_err(|_| ApiError::new(503, "unavailable"))?;
        if used > self.unmask_per_hour {
            self.access.note_forbidden(p.badge(), "POST", "/claims/*/unmask (hourly limit)");
            let mut e = ApiError::new(429, "too_many_unmasks");
            e.headers.push(("retry-after".into(), "3600".into()));
            return Err(e);
        }
        self.access.record("unmask", json!({"badge_id": p.badge(), "claim_id": id.chars().take(64).collect::<String>(), "reason": b.reason})).map_err(|e| self.serr(e.into(), req, &p))?;
        Ok(ApiResponse::json(200, &self.claim_view(&p, &c, &r, true)?))
    }

    fn decide_direct(&self, req: &ApiRequest, id: &str, rule: &str) -> R {
        let p = self.need(req, "claims.decide")?;
        let b: DecisionBody = body_of(req).ok().filter(|b: &DecisionBody| ACTIONS.contains(&b.action.as_str()) && !b.reason.is_empty() && !too_long(&b.reason, 1000)).ok_or_else(invalid)?;
        let (claim, results) = self.claims.get(id).ok_or_else(|| ApiError::new(404, "not_found"))?;
        if cg_access::masking::text_leaks_identifier(&b.reason, &claim, &self.settings.pii_key) {
            return Err(invalid().with("detail", json!("the reason must not contain the patient or member identifier")));
        }
        let finding = results.as_array().into_iter().flatten().find(|r| r["rule_id"] == rule).cloned().ok_or_else(|| ApiError::new(404, "not_found"))?;
        if finding["severity"] == "high" && !p.permissions.contains("claims.decide_high") {
            return Err(self.forbid(req, &p));
        }
        let decision = json!({"claim_id": id, "rule_id": rule, "action": b.action, "actor": p.badge(), "reason": b.reason,
            "created_at": cg_audit::iso_of(std::time::Duration::from_secs_f64(self.now())), "original_status": finding["status"]});
        let rejected = |detail: Option<String>| {
            let e = ApiError::new(422, "decision_rejected");
            match detail { Some(d) => e.with("detail", json!(d.chars().take(300).collect::<String>())), None => e }
        };
        cg_queue::service::validate_decision(&decision, &finding).map_err(|m| rejected(Some(m)))?;
        self.review_log.append_review_decisions(&[decision]).map_err(|_| rejected(None))?;
        self.access.record("decision", json!({"badge_id": p.badge(), "claim_id": id, "rule_id": rule, "action": b.action})).map_err(|e| self.serr(e.into(), req, &p))?;
        let state = review_state(&results, self.review_log.path()).get(&(id.to_string(), rule.to_string())).copied();
        Ok(ApiResponse::json(200, &json!({"recorded": true, "review_state": state})))
    }

    fn recheck(&self, req: &ApiRequest, _id: &str) -> R {
        self.need(req, "claims.recheck")?;
        let b: RecheckBody = body_of(req).ok().filter(|b: &RecheckBody| (1..=15).contains(&b.rule_ids.len())).ok_or_else(invalid)?;
        let _ = b;
        // A real recheck needs prior run traces and stored result versions, which arrive with the queue: a correction is a new version.
        Err(ApiError::new(501, "not_implemented"))
    }

    // ---- audit and operations -------------------------------------------------------------------------------------------
    fn audit_events(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "audit.view")?;
        let q = query(req);
        let (limit, offset) = (q_int(&q, "limit", 100, 1, 1000)? as usize, q_int(&q, "offset", 0, 0, i64::MAX)? as usize);
        let (event_type, badge, claim_id) = (q_text(&q, "event_type", 40)?, q_text(&q, "badge", 64)?, q_text(&q, "claim_id", 80)?);
        let (since, until, newest) = (q_float(&q, "since")?, q_float(&q, "until")?, q_bool(&q, "newest_first")?);
        if event_type.as_deref().is_some_and(|t| !cg_audit::SECURITY_EVENT_TYPES.contains(&t)) {
            return Err(invalid());
        }
        self.access.record("audit_read", json!({"badge_id": p.badge()})).map_err(|e| self.serr(e.into(), req, &p))?;
        if event_type.is_none() && badge.is_none() && claim_id.is_none() && since.is_none() && until.is_none() && !newest {
            return Ok(ApiResponse::json(200, &json!({"events": self.securitylog.events(limit, offset).map_err(|_| invalid())?})));
        }
        let mut rows = ops::matching(self.securitylog.scan(ops::MAX_SCAN_ROWS).unwrap_or_default(), event_type.as_deref(), badge.as_deref(), claim_id.as_deref(), since, until);
        if newest {
            rows.reverse();
        }
        let matched = rows.len();
        Ok(ApiResponse::json(200, &json!({"events": rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>(), "matched": matched})))
    }

    fn audit_verify(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "audit.verify")?;
        let r = self.audit_status.refresh();
        let ok = r["security"]["ok"] == true && r["review"]["ok"] == true;
        self.access.record("audit_verify", json!({"badge_id": p.badge(), "ok": ok})).map_err(|e| self.serr(e.into(), req, &p))?;
        Ok(ApiResponse::json(200, &json!({"security": r["security"], "review": r["review"]})))
    }

    // ---- payer rules ---------------------------------------------------------------------------------------------------------
    fn payer(&self) -> Result<&Arc<cg_engine::payer::PayerRules>, ApiError> {
        self.payer.as_ref().ok_or_else(|| ApiError::new(404, "not_found"))
    }

    /// Ask the rulebook: retrieval with citations over the rules and the payer policies in force. Never touches a claim.
    fn knowledge(&self, req: &ApiRequest) -> R {
        self.need(req, "claims.view")?;
        let pr = self.payer()?;
        let q = query(req).get("q").cloned().unwrap_or_default();
        if q.trim().is_empty() || q.chars().count() > 300 {
            return Err(ApiError::new(422, "invalid_request").with("detail", json!("ask a question of 1 to 300 characters")));
        }
        Ok(ApiResponse::json(200, &pr.ask(q.trim(), 4)))
    }

    fn payer_get(&self, req: &ApiRequest) -> R {
        self.need(req, "routing.manage")?;
        let pr = self.payer()?;
        let _ = pr.refresh();
        let mut cur = pr.current();
        cur["history"] = json!(pr.history());
        Ok(ApiResponse::json(200, &cur))
    }

    fn payer_validate(&self, req: &ApiRequest) -> R {
        self.need(req, "routing.manage")?;
        let body = object_body(req)?;
        let policies = body.get("policies").ok_or_else(invalid)?;
        Ok(ApiResponse::json(200, &self.payer()?.validate(policies).to_json()))
    }

    /// Changes the payer rules. With two or more administrators the change waits for a different administrator to approve it (the same dual control as the
    /// other dangerous actions); a lone administrator acts alone and the security log says so. Either way it is a new numbered version in a hash-chained log.
    fn payer_post(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        let pr = self.payer()?;
        let body = object_body(req)?;
        let policies = body.get("policies").ok_or_else(invalid)?.clone();
        let reason = body.get("reason").and_then(Value::as_str).ok_or_else(invalid)?.to_string();
        let v = pr.validate(&policies);
        if !v.errors.is_empty() {
            return Err(ApiError::new(422, "invalid_request").with("detail", json!(v.errors.join("; "))));
        }
        if reason.chars().count() < 10 || reason.chars().count() > 300 {
            return Err(ApiError::new(422, "invalid_request").with("detail", json!("write a reason of 10 to 300 characters")));
        }
        let summary = format!("Change the payer rules ({} difference(s)): {}", v.changes.len(), v.changes.iter().take(3).cloned().collect::<Vec<_>>().join("; "));
        match self.access.gate_action(&p, "payer_rules", "payer-rules", summary, json!({"policies": policies, "reason": reason})).map_err(|e| self.serr(e, req, &p))? {
            Gate::Pending { approval, uri } => Ok(Self::pending(&approval, uri)),
            Gate::Done(()) => {
                let version = pr.apply(&policies, p.badge(), None, &reason).map_err(|e| ApiError::new(422, "invalid_request").with("detail", json!(e)))?;
                self.access.record("queue_admin", json!({"actor": p.badge(), "command": "payer_rules", "version": version})).map_err(|e| self.serr(e.into(), req, &p))?;
                Ok(ApiResponse::json(200, &json!({"status": "applied", "version": version})))
            }
        }
    }

    /// The signed ledger: per log, how many checkpoints, what the newest covers, and whether every one verifies with the public key.
    fn ledger_status(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "audit.view")?;
        self.access.record("audit_read", json!({"badge_id": p.badge()})).map_err(|e| self.serr(e.into(), req, &p))?;
        Ok(ApiResponse::json(200, &self.ledger.get(0.0)))
    }

    /// A proof that one event was in a log at a signed checkpoint, to be checked elsewhere with the public key alone (`cg ledger verify-proof`).
    fn ledger_proof(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "audit.view")?;
        let q = query(req);
        let label = q.get("log").map(String::as_str).unwrap_or("");
        let seq = q_int(&q, "sequence", 0, 1, 1_000_000_000)? as u64;
        let (Some(path), Some(pk)) = (self.ledger.log_path(label), self.ledger.public_key()) else { return Err(ApiError::new(404, "not_found")) };
        let Some(cp) = cg_audit::ledger::latest_valid_checkpoint(path, label, &pk).filter(|c| c.count >= seq) else {
            return Err(ApiError::new(404, "not_found").with("detail", json!("no signed checkpoint covers that event yet")));
        };
        self.access.record("audit_read", json!({"badge_id": p.badge()})).map_err(|e| self.serr(e.into(), req, &p))?;
        cg_audit::ledger::prove(path, seq, &cp).map(|v| ApiResponse::json(200, &v)).map_err(|e| ApiError::new(409, "conflict").with("detail", json!(e)))
    }

    fn audit_overview(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "audit.view")?;
        let hours = q_int(&query(req), "hours", 24, 1, 168)?;
        self.access.record("audit_read", json!({"badge_id": p.badge()})).map_err(|e| self.serr(e.into(), req, &p))?;
        Ok(ApiResponse::json(200, &ops::audit_summary(&self.securitylog.scan(ops::MAX_SCAN_ROWS).unwrap_or_default(), self.now(), hours)))
    }

    fn health(&self, req: &ApiRequest) -> R {
        self.need(req, "queue.view")?;
        Ok(ApiResponse::json(200, &ops::build_report(&self.probes, &self.metrics, &self.clock)))
    }

    fn trace(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "audit.view")?;
        let q = self.queue()?;
        q.trace(&p, id).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    // ---- users ------------------------------------------------------------------------------------------------------------
    fn users_list(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "users.manage")?;
        let users = self.access.list_users(&p).map_err(|e| self.serr(e, req, &p))?;
        let now = self.now();
        let rows: Vec<Value> = users.iter().map(|u| json!({"badge_id": u.badge_id, "name": u.name, "level": u.level, "active": u.active, "locked": u.locked_until.is_some_and(|l| l > now),
            "must_change_password": u.must_change_password, "last_login": u.last_login})).collect();
        Ok(ApiResponse::json(200, &json!({"users": rows})))
    }

    fn users_create(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "users.manage")?;
        let b: CreateUserBody = body_of(req)
            .ok()
            .filter(|b: &CreateUserBody| !(too_long(&b.badge_id, 64) || too_long(&b.name, 200) || too_long(&b.password, 128) || b.grants.len() > 20 || b.revokes.len() > 20))
            .ok_or_else(invalid)?;
        match self.access.create_user_gated(&p, &b.badge_id, &b.name, &b.password, b.level, &b.grants, &b.revokes).map_err(|e| self.serr(e, req, &p))? {
            Gate::Done((user, uri)) => Ok(ApiResponse::json(201, &json!({"badge_id": user.badge_id, "provisioning_uri": uri}))),
            Gate::Pending { approval, uri } => Ok(Self::pending(&approval, uri)),
        }
    }

    fn users_patch(&self, req: &ApiRequest, badge: &str) -> R {
        let p = self.need(req, "users.manage")?;
        let m = object_body(req)?;
        if m.keys().any(|k| !["name", "level", "grants", "revokes", "active"].contains(&k.as_str())) || m.values().any(Value::is_null) {
            return Err(invalid());
        }
        let strings = |k: &str| -> Result<Option<Vec<String>>, ApiError> {
            match m.get(k) {
                None => Ok(None),
                Some(Value::Array(a)) if a.len() <= 20 => a.iter().map(|s| s.as_str().map(str::to_string)).collect::<Option<Vec<_>>>().map(Some).ok_or_else(invalid),
                Some(_) => Err(invalid()),
            }
        };
        let changes = Changes {
            name: match m.get("name") { None => None, Some(Value::String(s)) if !too_long(s, 200) => Some(s.clone()), Some(_) => return Err(invalid()) },
            level: match m.get("level") { None => None, Some(v) if v.is_i64() => v.as_i64(), Some(_) => return Err(invalid()) },
            grants: strings("grants")?,
            revokes: strings("revokes")?,
            active: match m.get("active") { None => None, Some(Value::Bool(b)) => Some(*b), Some(_) => return Err(invalid()) },
        };
        match self.access.update_user_gated(&p, badge, &changes).map_err(|e| self.serr(e, req, &p))? {
            Gate::Done(u) => Ok(ApiResponse::json(200, &json!({"badge_id": u.badge_id, "level": u.level, "active": u.active}))),
            Gate::Pending { approval, uri } => Ok(Self::pending(&approval, uri)),
        }
    }

    fn users_unlock(&self, req: &ApiRequest, badge: &str) -> R {
        let p = self.need(req, "users.manage")?;
        self.access.unlock_user(&p, badge).map_err(|e| self.serr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"status": "unlocked"})))
    }

    fn users_reset_totp(&self, req: &ApiRequest, badge: &str) -> R {
        let p = self.need(req, "users.manage")?;
        match self.access.reset_totp_gated(&p, badge).map_err(|e| self.serr(e, req, &p))? {
            Gate::Done(uri) => Ok(ApiResponse::json(200, &json!({"provisioning_uri": uri}))),
            Gate::Pending { approval, uri } => Ok(Self::pending(&approval, uri)),
        }
    }

    // ---- approvals (dual control) ------------------------------------------------------------------------------------------
    /// 202: nothing has happened yet; a second administrator has to approve. The authenticator link (where there is one) goes to the person
    /// who asked, now, and only works once the request is approved. The stored payload is never shown.
    fn pending(a: &Approval, uri: Option<String>) -> ApiResponse {
        let mut body = json!({"status": "pending_approval", "approval": Self::approval_view(a, 0.0)});
        if let Some(u) = uri {
            body["provisioning_uri"] = json!(u);
        }
        ApiResponse::json(202, &body)
    }

    fn approval_view(a: &Approval, now: f64) -> Value {
        let status = if a.status == "pending" && now > 0.0 && a.expires_at <= now { "expired" } else { a.status.as_str() };
        json!({"id": a.id, "action": a.action, "badge_id": a.target, "summary": a.summary, "requested_by": a.requested_by, "requested_at": a.requested_at, "expires_at": a.expires_at,
            "status": status, "decided_by": a.decided_by, "decided_at": a.decided_at, "note": a.note})
    }

    fn approval_id(id: &str) -> Result<(), ApiError> {
        if id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(ApiError::new(404, "not_found"));
        }
        Ok(())
    }

    fn approvals_list(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "users.manage")?;
        let now = self.now();
        let rows: Vec<Value> = self.access.list_approvals(&p).map_err(|e| self.serr(e, req, &p))?.iter().map(|a| Self::approval_view(a, now)).collect();
        Ok(ApiResponse::json(200, &json!({"approvals": rows, "dual_control": self.access.settings.dual_control})))
    }

    fn approvals_approve(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "users.manage")?;
        Self::approval_id(id)?;
        let m = object_body(req)?;
        let code = match (m.len(), m.get("totp_code")) {
            (1, Some(Value::String(c))) if c.len() <= 12 => c.clone(),
            _ => return Err(invalid()),
        };
        // a wrong code is 422, not 401: the person is still signed in, and the console must not throw them out for a typing mistake
        let a = self.access.approve(&p, id, &code).map_err(|e| match e {
            ServiceError::Auth(AuthError::InvalidCredentials) => ApiError::new(422, "wrong_code"),
            other => self.serr(other, req, &p),
        })?;
        Ok(ApiResponse::json(200, &json!({"approval": Self::approval_view(&a, 0.0)})))
    }

    fn approvals_reject(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "users.manage")?;
        Self::approval_id(id)?;
        let m = object_body(req)?;
        let note = match (m.keys().all(|k| k == "note"), m.get("note")) {
            (true, None) => String::new(),
            (true, Some(Value::String(n))) if n.chars().count() <= 200 => n.clone(),
            _ => return Err(invalid()),
        };
        let a = self.access.reject_request(&p, id, &note).map_err(|e| self.serr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"approval": Self::approval_view(&a, 0.0)})))
    }

    fn approvals_cancel(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "users.manage")?;
        Self::approval_id(id)?;
        let a = self.access.cancel(&p, id).map_err(|e| self.serr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"approval": Self::approval_view(&a, 0.0)})))
    }

    // ---- the queue --------------------------------------------------------------------------------------------------------
    fn work_inbox(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "claims.decide")?;
        let q = self.queue()?;
        let claims = q.inbox(&p).map_err(|e| self.qerr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"claims": claims, "light_review": q.light_review_rules()})))
    }

    /// A fingerprint of what the caller's open page shows, for the live-update stream (server.rs): it changes when the caller's own inbox
    /// changes (`topics=inbox`) or when the queue overview's counts change (`topics=queue`, needs queue.view). It carries no claim data,
    /// only a hash, so the stream can never show anyone more than they could already read; the page re-reads the real data through the
    /// ordinary authenticated routes. Times that grow every second (waiting times, lease clocks) are left out on purpose.
    fn work_changes(&self, req: &ApiRequest) -> R {
        use sha2::{Digest, Sha256};
        let p = self.principal(req)?;
        let q = self.queue()?;
        let topics = query(req).get("topics").cloned().unwrap_or_else(|| "inbox".into());
        let want: Vec<&str> = topics.split(',').collect();
        if want.is_empty() || want.len() > 2 || want.iter().any(|t| !["inbox", "queue"].contains(t)) || (want.len() == 2 && want[0] == want[1]) {
            return Err(invalid());
        }
        let mut h = Sha256::new();
        if want.contains(&"inbox") {
            let docs = q.inbox(&p).map_err(|e| self.qerr(e, req, &p))?;
            h.update(b"inbox");
            for d in &docs {
                h.update(d["claim_id"].as_str().unwrap_or("").as_bytes());
                h.update(d["signoff_stage"].as_str().unwrap_or("").as_bytes());
                h.update(b",");
            }
        }
        if want.contains(&"queue") {
            let d = q.dashboard(&p).map_err(|e| self.qerr(e, req, &p))?;
            let stable = json!({"counts": d["counts"], "waiting": d["waiting"], "countersign": d["awaiting_countersign"], "on_shift": d["on_shift"],
                                "inbox_sizes": d["inbox_sizes"], "shortages": d["shortages"], "config_version": d["config_version"]});
            h.update(b"queue");
            h.update(stable.to_string().as_bytes());
        }
        Ok(ApiResponse::json(200, &json!({"signature": hex::encode(h.finalize()), "topics": want})))
    }

    fn work_next(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "claims.decide")?;
        let claim = self.queue()?.next(&p).map_err(|e| self.qerr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"claim": claim})))
    }

    fn work_heartbeat(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "claims.decide")?;
        let n = self.queue()?.heartbeat(&p).map_err(|e| self.qerr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"extended": n})))
    }

    fn work_decide(&self, req: &ApiRequest, id: &str, rule: &str) -> R {
        let p = self.need(req, "claims.decide")?;
        let b: DecisionBody = body_of(req).ok().filter(|b: &DecisionBody| ACTIONS.contains(&b.action.as_str()) && (!b.reason.is_empty() || b.action == "confirm_issue") && !too_long(&b.reason, 1000)).ok_or_else(invalid)?;
        self.queue()?.decide_finding(&p, id, rule, &b.action, &b.reason).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn work_bulk_confirm(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "claims.decide")?;
        let b: BulkBody = body_of(req)
            .ok()
            .filter(|b: &BulkBody| b.rule_id.len() <= 10 && (1..=25).contains(&b.claim_ids.len()) && b.claim_ids.iter().all(|c| !c.is_empty() && c.len() <= 64))
            .ok_or_else(invalid)?;
        self.queue()?.bulk_confirm(&p, &b.rule_id, &b.claim_ids).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn work_whatif(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "claims.recheck")?;
        let b: WhatIfBody = body_of(req).map_err(|_| invalid())?;
        self.queue()?.what_if(&p, id, &b.changes).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn work_recheck(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "claims.recheck")?;
        let b: WorkRecheckBody = body_of(req).ok().filter(|b: &WorkRecheckBody| !b.reason.is_empty() && !too_long(&b.reason, 1000) && b.rule_ids.as_ref().map_or(true, |r| r.len() <= 15 && r.iter().all(|x| x.len() <= 10))).ok_or_else(invalid)?;
        self.queue()?.recheck(&p, id, &b.changes, &b.reason, b.rule_ids.as_deref()).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn work_verify(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "claims.decide")?;
        let b: VerifyBody = body_of(req)
            .ok()
            .filter(|b: &VerifyBody| (b.action == "verify_clear" || b.action == "escalate") && b.category.as_deref().map_or(true, |c| !too_long(c, 10)) && b.note.as_deref().map_or(true, |n| !too_long(n, 1000)))
            .ok_or_else(invalid)?;
        self.queue()?.decide_green_in(&p, id, &b.action, b.category.as_deref(), b.note.as_deref()).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn work_info_request(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "claims.decide")?;
        let b: InfoRequestBody = body_of(req).ok().filter(|b: &InfoRequestBody| (1..=40).contains(&b.request_id.len()) && (b.action == "send" || b.action == "decline")).ok_or_else(invalid)?;
        self.queue()?.decide_info_request(&p, id, &b.request_id, b.action == "send").map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_batches(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        self.queue()?.batches(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_batch(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "routing.manage")?;
        if too_long(id, 40) {
            return Err(ApiError::new(404, "not_found"));
        }
        self.queue()?.batch(&p, id).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_batch_decision(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "routing.manage")?;
        if too_long(id, 40) {
            return Err(ApiError::new(404, "not_found"));
        }
        let b: BatchDecisionBody = body_of(req).ok().filter(|b: &BatchDecisionBody| b.note.as_deref().map_or(true, |n| !too_long(n, 1000))).ok_or_else(invalid)?;
        self.queue()?.decide_batch(&p, id, &b.action, b.n, b.note.as_deref().unwrap_or("")).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_dashboard(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "queue.view")?;
        self.queue()?.dashboard(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_feedback(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "queue.view")?;
        self.queue()?.feedback(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_submit(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        let b: SubmitBody = body_of(req).ok().filter(|b: &SubmitBody| (1..=cg_queue::service::MAX_SUBMIT_CLAIMS).contains(&b.claims.len()) && b.claims.iter().all(Value::is_object)).ok_or_else(invalid)?;
        if b.run_async {
            let job = self.queue()?.submit_claims_async(&p, b.claims).map_err(|e| self.qerr(e, req, &p))?;
            return Ok(ApiResponse::json(202, &job));
        }
        let rows = self.queue()?.submit_claims(&p, &b.claims).map_err(|e| self.qerr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"results": rows})))
    }

    fn q_submit_job(&self, req: &ApiRequest, id: &str) -> R {
        let p = self.need(req, "routing.manage")?;
        if id.len() > 40 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ApiError::new(404, "not_found"));
        }
        self.queue()?.submit_job(&p, id).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_get_config(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        self.queue()?.get_config(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_patch_config(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        let mut m = object_body(req)?;
        let expected = m.remove("expected_version").filter(Value::is_i64).and_then(|v| v.as_i64()).filter(|v| *v >= 0).ok_or_else(invalid)?;
        const ALLOWED: [&str; 15] = ["points", "lane_b_flagged", "lane_b_score", "slice_size", "low_water", "lease_seconds", "aging_per_hour", "ai_per_minute", "ai_daily_budget", "on_shift", "exclusions", "auto_clear_green", "audit_percent", "bulk_confirm_skip_lower", "auto_request_info"];
        if m.keys().any(|k| !ALLOWED.contains(&k.as_str())) || m.values().any(Value::is_null) {
            return Err(invalid());
        }
        for k in ["on_shift", "exclusions"] {
            if m.get(k).is_some_and(|v| v.as_array().is_none_or(|a| a.len() > 1000)) {
                return Err(invalid());
            }
        }
        let new = self.queue()?.set_config(&p, &m, expected).map_err(|e| self.qerr(e, req, &p))?;
        Ok(ApiResponse::json(200, &json!({"version": new.version})))
    }
}

pub fn perms_list(p: &Perms) -> Vec<&'static str> {
    p.iter().copied().collect()
}
