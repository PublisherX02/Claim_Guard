//! Operational telemetry for the admin console: request metrics, and the health report built from small, independent checks
//! (`src/access/ops.py`, `src/workqueue/health.py`, `src/workqueue/jobs.py`).
//!
//! Nothing here decides anything about a claim and nothing here stores personal data. Requests are counted by method, route shape and
//! status (never the query string, a body, a badge or a claim id), so the numbers are safe to show to any administrator and cheap to keep:
//! one fixed-size ring of one-minute buckets in memory per API process. That is also its limit, stated in the console: it shows this
//! process since it started, not the whole fleet (the fleet-wide view is the database-backed queue and job records).

use cg_access::securitylog::SecurityLog;
use cg_access::service::AccessService;
use cg_access::Clock;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::service::QueueService;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const BUCKET_SECONDS: i64 = 60;
pub const MAX_BUCKETS: usize = 180;
pub const LATENCY_EDGES_MS: [f64; 9] = [5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0];
pub const MAX_RECENT_ERRORS: usize = 50;
pub const MAX_SCAN_ROWS: usize = 50_000;
pub const HOUR: i64 = 3600;
const ID_AFTER: [&str; 3] = ["claims", "users", "findings"];

pub const FAILURE_EVENTS: [&str; 4] = ["login_failure", "lockout", "forbidden", "token_rejected"];

/// The route with identifiers collapsed, so /claims/CG-123/findings/R001/decision and any other claim count as one route.
pub fn route_shape(path: &str) -> String {
    let parts: Vec<&str> = path.split('?').next().unwrap_or("").split('/').filter(|p| !p.is_empty()).collect();
    let shaped: Vec<String> = parts.iter().enumerate().map(|(i, p)| if i > 0 && ID_AFTER.contains(&parts[i - 1]) { ":id".to_string() } else { p.chars().take(40).collect() }).collect();
    format!("/{}", shaped.iter().take(8).cloned().collect::<Vec<_>>().join("/"))
}

fn bucket_of(ms: f64) -> usize {
    LATENCY_EDGES_MS.iter().position(|e| ms <= *e).unwrap_or(LATENCY_EDGES_MS.len())
}

/// Upper edge of the latency bucket that contains the q-th fraction of requests (an honest upper bound, not an interpolation).
fn percentile(counts: &[i64], q: f64) -> Option<f64> {
    let total: i64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let mut seen = 0;
    for (i, c) in counts.iter().enumerate() {
        seen += c;
        if seen as f64 / total as f64 >= q {
            return LATENCY_EDGES_MS.get(i).copied();
        }
    }
    None
}

#[derive(Clone)]
struct Bucket {
    t: i64,
    n: i64,
    e4: i64,
    e5: i64,
    lat: Vec<i64>,
}

#[derive(Default)]
struct Route {
    n: i64,
    e4: i64,
    e5: i64,
    ms: f64,
    lat: Vec<i64>,
}

struct MetricsInner {
    buckets: VecDeque<Bucket>,
    errors: VecDeque<Value>,
    routes: BTreeMap<String, Route>,
}

pub struct Metrics {
    clock: Clock,
    started: f64,
    inner: Mutex<MetricsInner>,
}

impl Metrics {
    pub fn new(clock: Clock) -> Metrics {
        let started = clock();
        Metrics { clock, started, inner: Mutex::new(MetricsInner { buckets: VecDeque::new(), errors: VecDeque::new(), routes: BTreeMap::new() }) }
    }

    pub fn record(&self, method: &str, path: &str, status: u16, seconds: f64) {
        let now = (self.clock)();
        let ms = (seconds * 1000.0).max(0.0);
        let key = format!("{method} {}", route_shape(path));
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let t = (now as i64).div_euclid(BUCKET_SECONDS) * BUCKET_SECONDS;
        if g.buckets.back().map(|b| b.t) != Some(t) {
            if g.buckets.len() == MAX_BUCKETS {
                g.buckets.pop_front();
            }
            g.buckets.push_back(Bucket { t, n: 0, e4: 0, e5: 0, lat: vec![0; LATENCY_EDGES_MS.len() + 1] });
        }
        let b = g.buckets.back_mut().expect("just pushed");
        b.n += 1;
        b.lat[bucket_of(ms)] += 1;
        match status {
            400..=499 => b.e4 += 1,
            500.. => b.e5 += 1,
            _ => {}
        }
        let key = if !g.routes.contains_key(&key) && g.routes.len() >= 200 { format!("{method} /other") } else { key }; // a scanner cannot grow the table without bound
        let r = g.routes.entry(key.clone()).or_insert_with(|| Route { lat: vec![0; LATENCY_EDGES_MS.len() + 1], ..Default::default() });
        r.n += 1;
        r.ms += ms;
        r.lat[bucket_of(ms)] += 1;
        match status {
            500.. => {
                r.e5 += 1;
                if g.errors.len() == MAX_RECENT_ERRORS {
                    g.errors.pop_front();
                }
                g.errors.push_back(json!({"at": now, "route": key, "status": status}));
            }
            400..=499 => r.e4 += 1,
            _ => {}
        }
    }

    pub fn snapshot(&self, minutes: i64) -> Value {
        let now = (self.clock)();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let first = (now as i64).div_euclid(BUCKET_SECONDS) * BUCKET_SECONDS - (minutes - 1) * BUCKET_SECONDS;
        let by_t: BTreeMap<i64, &Bucket> = g.buckets.iter().filter(|b| b.t >= first).map(|b| (b.t, b)).collect();
        let (mut series, mut lat_total, mut n, mut e4, mut e5) = (vec![], vec![0i64; LATENCY_EDGES_MS.len() + 1], 0, 0, 0);
        for k in 0..minutes {
            let t = first + k * BUCKET_SECONDS;
            match by_t.get(&t) {
                Some(b) => {
                    series.push(json!({"t": t, "n": b.n, "e4": b.e4, "e5": b.e5, "p95_ms": percentile(&b.lat, 0.95)}));
                    n += b.n;
                    e4 += b.e4;
                    e5 += b.e5;
                    lat_total.iter_mut().zip(&b.lat).for_each(|(a, c)| *a += c);
                }
                None => series.push(json!({"t": t, "n": 0, "e4": 0, "e5": 0, "p95_ms": null})),
            }
        }
        let mut routes: Vec<(&String, &Route)> = g.routes.iter().collect();
        routes.sort_by_key(|(_, r)| std::cmp::Reverse(r.n));
        let routes: Vec<Value> = routes
            .into_iter()
            .take(15)
            .map(|(k, v)| json!({"route": k, "requests": v.n, "client_errors": v.e4, "server_errors": v.e5, "mean_ms": if v.n > 0 { json!((v.ms / v.n as f64 * 100.0).round() / 100.0) } else { Value::Null }, "p95_ms": percentile(&v.lat, 0.95)}))
            .collect();
        let recent: Vec<Value> = g.errors.iter().rev().take(10).cloned().collect();
        json!({"uptime_seconds": ((now - self.started) * 10.0).round() / 10.0, "window_minutes": minutes, "requests": n, "client_errors": e4, "server_errors": e5,
               "p50_ms": percentile(&lat_total, 0.5), "p95_ms": percentile(&lat_total, 0.95), "p99_ms": percentile(&lat_total, 0.99), "latency_edges_ms": LATENCY_EDGES_MS,
               "latency_counts": lat_total, "series": series, "routes": routes, "recent_errors": recent})
    }
}

// ---- health -----------------------------------------------------------------------------------------------------------------
pub const OK: &str = "ok";
pub const DEGRADED: &str = "degraded";
pub const DOWN: &str = "down";
pub const UNKNOWN: &str = "unknown";
const MAX_DETAIL: usize = 240;

fn rank(s: &str) -> u8 {
    match s {
        OK => 0,
        UNKNOWN => 1,
        DEGRADED => 2,
        _ => 3,
    }
}

pub fn worst<'a>(statuses: impl Iterator<Item = &'a str>) -> &'a str {
    statuses.max_by_key(|s| rank(s)).unwrap_or(OK)
}

pub struct Probe {
    pub id: &'static str,
    pub label: &'static str,
    pub weight: &'static str,
    pub run: Box<dyn Fn() -> (String, String, Map<String, Value>) + Send + Sync>,
}

/// Runs one probe. A probe that panics is reported as down, never as an error.
pub fn check(p: &Probe, clock: &Clock) -> Value {
    let t = std::time::Instant::now();
    let (status, detail, extra) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (p.run)())).unwrap_or_else(|_| (DOWN.into(), "check failed: panic".into(), Map::new()));
    let mut out = json!({"id": p.id, "label": p.label, "status": status, "detail": detail.chars().take(MAX_DETAIL).collect::<String>(), "weight": p.weight,
                         "latency_ms": (t.elapsed().as_secs_f64() * 10000.0).round() / 10.0, "checked_at": clock()});
    if let Some(o) = out.as_object_mut() {
        o.extend(extra);
    }
    out
}

/// A core component that is down makes the system down; any other problem makes it degraded. A component that has not reported yet
/// (unknown) is shown as such but does not by itself change the headline.
pub fn overall(components: &[Value]) -> &'static str {
    if components.iter().any(|c| c["weight"] == "core" && c["status"] == DOWN) {
        return DOWN;
    }
    if components.iter().any(|c| c["status"] == DEGRADED || c["status"] == DOWN) { DEGRADED } else { OK }
}

pub fn build_report(probes: &[Probe], metrics: &Metrics, clock: &Clock) -> Value {
    let components: Vec<Value> = probes.iter().map(|p| check(p, clock)).collect();
    json!({"overall": overall(&components), "checked_at": clock(), "components": components, "metrics": metrics.snapshot(60)})
}

// ---- audit analytics ---------------------------------------------------------------------------------------------------------
fn parse_time(text: &Value) -> Option<f64> {
    let s = text.as_str()?;
    // 2026-10-09T12:38:17.176630+00:00 (what the audit writer produces)
    let (date, rest) = s.split_once('T')?;
    let day = cg_core::Day::parse(date)?;
    let time = rest.get(..8)?;
    let (h, m, sec): (i64, i64, i64) = (time.get(0..2)?.parse().ok()?, time.get(3..5)?.parse().ok()?, time.get(6..8)?.parse().ok()?);
    let frac: f64 = rest.get(8..).and_then(|f| f.strip_prefix('.')).map(|f| f.chars().take_while(char::is_ascii_digit).collect::<String>()).filter(|d| !d.is_empty()).map(|d| format!("0.{d}").parse().unwrap_or(0.0)).unwrap_or(0.0);
    let days = day.days_since(&cg_core::Day::parse("1970-01-01")?);
    Some((days * 86400 + h * 3600 + m * 60 + sec) as f64 + frac)
}

pub fn event_actor(e: &Value) -> Option<String> {
    ["actor", "badge_id"].iter().find_map(|k| e[*k].as_str().filter(|s| !s.is_empty()).map(str::to_string))
}

/// Filters audit rows ({sequence, recorded_at, event}). Every filter is an exact match except the time bounds (epoch seconds).
pub fn matching(rows: Vec<Value>, event_type: Option<&str>, badge: Option<&str>, claim_id: Option<&str>, since: Option<f64>, until: Option<f64>) -> Vec<Value> {
    rows.into_iter()
        .filter(|r| {
            let e = &r["event"];
            if event_type.is_some_and(|t| e["event_type"] != t) || claim_id.is_some_and(|c| e["claim_id"] != c) {
                return false;
            }
            if badge.is_some_and(|b| event_actor(e).as_deref() != Some(b) && e["badge_id"] != b) {
                return false;
            }
            if since.is_some() || until.is_some() {
                let Some(t) = parse_time(&r["recorded_at"]) else { return false };
                if since.is_some_and(|s| t < s) || until.is_some_and(|u| t > u) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Counts for the audit panel: by type, per hour for the last `hours`, the busiest actors and the failure signals.
pub fn audit_summary(rows: &[Value], now: f64, hours: i64) -> Value {
    let first = (now as i64).div_euclid(HOUR) * HOUR - (hours - 1) * HOUR;
    let mut series: BTreeMap<i64, (i64, i64)> = (0..hours).map(|k| (first + k * HOUR, (0, 0))).collect();
    let (mut by_type, mut actors): (BTreeMap<String, i64>, BTreeMap<String, i64>) = (BTreeMap::new(), BTreeMap::new());
    for r in rows {
        let e = &r["event"];
        let kind = e["event_type"].as_str().unwrap_or("?").to_string();
        *by_type.entry(kind.clone()).or_default() += 1;
        if let Some(t) = parse_time(&r["recorded_at"]) {
            if let Some(slot) = series.get_mut(&((t as i64).div_euclid(HOUR) * HOUR)) {
                slot.0 += 1;
                slot.1 += i64::from(FAILURE_EVENTS.contains(&kind.as_str()));
                if let Some(who) = event_actor(e) {
                    *actors.entry(who).or_default() += 1;
                }
            }
        }
    }
    let mut types: Vec<(&String, &i64)> = by_type.iter().collect();
    types.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut top: Vec<(&String, &i64)> = actors.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut failures = Map::new();
    let mut names = FAILURE_EVENTS;
    names.sort();
    for k in names {
        failures.insert(k.into(), json!(by_type.get(k).copied().unwrap_or(0)));
    }
    json!({"total": rows.len(), "window_hours": hours, "by_type": types.into_iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<_, _>>(),
           "series": series.iter().map(|(t, (n, f))| json!({"t": t, "events": n, "failures": f})).collect::<Vec<_>>(),
           "actors": top.into_iter().take(10).map(|(b, n)| json!({"badge": b, "events": n})).collect::<Vec<_>>(), "failures": failures,
           "first_at": rows.first().map(|r| r["recorded_at"].clone()), "last_at": rows.last().map(|r| r["recorded_at"].clone())})
}

/// The last result of verifying both hash-chained logs, remembered so the health report does not re-verify on every refresh.
pub struct AuditStatus {
    verify: Box<dyn Fn() -> Value + Send + Sync>,
    clock: Clock,
    last: Mutex<Option<Value>>,
}

impl AuditStatus {
    pub fn new(verify: Box<dyn Fn() -> Value + Send + Sync>, clock: Clock) -> AuditStatus {
        AuditStatus { verify, clock, last: Mutex::new(None) }
    }

    pub fn refresh(&self) -> Value {
        let mut result = (self.verify)();
        result["at"] = json!((self.clock)());
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
        result
    }

    pub fn get(&self, max_age: f64) -> Value {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match last {
            Some(l) if (self.clock)() - l["at"].as_f64().unwrap_or(0.0) <= max_age => l,
            _ => self.refresh(),
        }
    }
}

// ---- job heartbeats -----------------------------------------------------------------------------------------------------------
pub const EXPECTED_EVERY: [(&str, f64); 4] = [("relay", 10.0), ("deal", 15.0), ("expire", 60.0), ("reconcile", 300.0)];

/// Runs `f`, remembers when it ran and whether it worked, returns its result. Recording never breaks a job.
pub fn run_job<T>(store: &dyn cg_queue::store::QueueStore, clock: &Clock, name: &str, f: impl FnOnce() -> T, describe: impl Fn(&T) -> String) -> T {
    let result = f();
    let _ = store.record_job(name, clock(), true, &describe(&result).chars().take(200).collect::<String>());
    result
}

/// The audit-protection jobs that are switched on in this environment, with how often each should run (seconds):
/// `audit_verify` always (the chains and anchors of both logs), `audit_mirror` when AUDIT_MIRROR_DIR is set (a second copy on another
/// volume), `audit_witness` when TSA_URL is set (an RFC 3161 timestamp of each anchor).
pub fn audit_guard_jobs(env: &std::collections::HashMap<String, String>) -> Vec<(&'static str, f64)> {
    let secs = |name: &str, default: f64| env.get(name).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| *v >= 5.0).unwrap_or(default);
    let on = |name: &str| env.get(name).is_some_and(|v| !v.trim().is_empty());
    let mut jobs = vec![("audit_verify", secs("AUDIT_VERIFY_SECONDS", 3600.0))];
    if on("AUDIT_MIRROR_DIR") {
        jobs.push(("audit_mirror", secs("AUDIT_MIRROR_SECONDS", 300.0)));
    }
    if on("TSA_URL") {
        jobs.push(("audit_witness", secs("AUDIT_WITNESS_SECONDS", 3600.0)));
    }
    if on("LEDGER_SIGNING_KEY") {
        jobs.push(("audit_ledger", secs("LEDGER_CHECKPOINT_SECONDS", 300.0)));
    }
    jobs
}

/// Like `run_job`, for a job that can fail: an Err is recorded as a failed run (with its text) and returned.
pub fn run_job_checked<T>(store: &dyn cg_queue::store::QueueStore, clock: &Clock, name: &str, f: impl FnOnce() -> Result<T, String>, describe: impl Fn(&T) -> String) -> Result<T, String> {
    let result = f();
    let (ok, detail) = match &result {
        Ok(v) => (true, describe(v)),
        Err(e) => (false, e.clone()),
    };
    let _ = store.record_job(name, clock(), ok, &detail.chars().take(200).collect::<String>());
    result
}

/// Judges the audit-protection jobs. A job that FAILED is DOWN, not merely degraded: it means a chain does not verify, a log no longer
/// matches its mirror, or the timestamp authority cannot be reached, and the first two are what tampering looks like.
pub fn audit_guard_status(jobs: &[Value], expected: &[(&'static str, f64)], now: f64) -> Vec<Value> {
    expected
        .iter()
        .map(|(name, every)| match jobs.iter().find(|j| j["name"] == *name) {
            None => json!({"name": name, "status": UNKNOWN, "detail": "has not run yet", "every_seconds": every}),
            Some(j) => {
                let age = now - j["at"].as_f64().unwrap_or(0.0);
                let (status, detail) = if j["ok"] != true {
                    (DOWN, format!("FAILED {} s ago: {}", age as i64, j["detail"].as_str().filter(|s| !s.is_empty()).unwrap_or("no detail")))
                } else if age > every * 3.0 {
                    (DOWN, format!("last ran {} s ago, expected every {} s", age as i64, *every as i64))
                } else {
                    (OK, format!("ran {} s ago: {}", age as i64, j["detail"].as_str().unwrap_or("")))
                };
                json!({"name": name, "status": status, "detail": detail, "every_seconds": every, "last_run": j["at"]})
            }
        })
        .collect()
}

/// Judges each expected job: ok when it succeeded within `grace` times its interval; unknown when it never ran.
pub fn job_status(jobs: &[Value], now: f64, grace: f64) -> Vec<Value> {
    EXPECTED_EVERY
        .iter()
        .map(|(name, every)| match jobs.iter().find(|j| j["name"] == *name) {
            None => json!({"name": name, "status": UNKNOWN, "detail": "has not run yet", "every_seconds": every}),
            Some(j) => {
                let age = now - j["at"].as_f64().unwrap_or(0.0);
                let (status, detail) = if j["ok"] != true {
                    (DEGRADED, format!("last run failed ({}) {} s ago", j["detail"].as_str().filter(|s| !s.is_empty()).unwrap_or("error"), age as i64))
                } else if age > every * grace {
                    (DOWN, format!("last ran {} s ago, expected every {} s", age as i64, *every as i64))
                } else {
                    (OK, format!("ran {} s ago", age as i64))
                };
                json!({"name": name, "status": status, "detail": detail, "every_seconds": every, "last_at": j["at"], "runs": j["runs"], "failures": j["failures"]})
            }
        })
        .collect()
}

// ---- probes ------------------------------------------------------------------------------------------------------------------
fn probe(id: &'static str, label: &'static str, weight: &'static str, run: impl Fn() -> (String, String, Map<String, Value>) + Send + Sync + 'static) -> Probe {
    Probe { id, label, weight, run: Box::new(run) }
}

fn res(status: &str, detail: impl Into<String>, extra: Value) -> (String, String, Map<String, Value>) {
    (status.to_string(), detail.into(), extra.as_object().cloned().unwrap_or_default())
}

/// The signed ledger over both audit logs, as the API sees it: it holds only the PUBLIC key (LEDGER_PUBLIC_KEY, or the `ledger.pub` the worker writes into the data
/// folder), so it can verify checkpoints and cut proofs but can never sign one. The result is cached for a minute: verifying hashes every row.
pub struct LedgerView {
    data_dir: PathBuf,
    logs: Vec<(String, PathBuf)>,
    cache: Mutex<Option<(std::time::Instant, Value)>>,
}

impl LedgerView {
    pub fn new(data_dir: PathBuf, logs: Vec<(String, PathBuf)>) -> LedgerView {
        LedgerView { data_dir, logs, cache: Mutex::new(None) }
    }

    pub fn public_key(&self) -> Option<cg_audit::ledger::VerifyingKey> {
        let text = std::env::var("LEDGER_PUBLIC_KEY").ok().filter(|s| !s.trim().is_empty()).or_else(|| std::fs::read_to_string(self.data_dir.join("ledger.pub")).ok())?;
        cg_audit::ledger::public_key_from(text.trim()).ok()
    }

    pub fn log_path(&self, label: &str) -> Option<&PathBuf> {
        self.logs.iter().find(|(l, _)| l == label).map(|(_, p)| p)
    }

    pub fn get(&self, max_age_seconds: f64) -> Value {
        if let Some((at, v)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed().as_secs_f64() <= max_age_seconds {
                return v.clone();
            }
        }
        let report = match self.public_key() {
            None => json!({"configured": false}),
            Some(pk) => {
                let logs: Map<String, Value> = self.logs.iter().map(|(label, path)| (label.clone(), cg_audit::ledger::verify_ledger(path, label, &pk, 3).to_json())).collect();
                json!({"configured": true, "key_id": cg_audit::ledger::key_id(&pk), "public_key": cg_audit::ledger::public_key_b64(&pk), "ok": logs.values().all(|r| r["ok"] == true), "logs": logs})
            }
        };
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), report.clone()));
        report
    }
}

pub fn access_probes(service: Arc<AccessService>, securitylog: Arc<SecurityLog>, audit_status: Arc<AuditStatus>, data_dir: PathBuf, metrics: Arc<Metrics>, clock: Clock, ledger: Arc<LedgerView>) -> Vec<Probe> {
    let (m, svc, st, sl, dd, ck) = (metrics, service, audit_status, securitylog, data_dir, clock);
    let ck_ledger = ck.clone();
    let svc2 = svc.clone();
    let sl2 = sl.clone();
    let sl3 = sl.clone();
    vec![
        probe("api", "Reviewer API", "core", move || {
            let s = m.snapshot(15);
            let (reqs, errs) = (s["requests"].as_i64().unwrap_or(0), s["server_errors"].as_i64().unwrap_or(0));
            let extra = json!({"uptime_seconds": s["uptime_seconds"]});
            if reqs >= 20 && errs as f64 / reqs as f64 > 0.10 {
                return res(DOWN, format!("{errs} of {reqs} requests failed in the last 15 minutes"), extra);
            }
            if errs > 0 {
                return res(DEGRADED, format!("{errs} server errors in the last 15 minutes ({reqs} requests)"), extra);
            }
            res(OK, format!("up {} s; {reqs} requests in the last 15 minutes, no server errors", s["uptime_seconds"].as_f64().unwrap_or(0.0) as i64), extra)
        }),
        probe("user_store", "User database", "core", move || if svc2.store.ping() { res(OK, "user database answers a ping", json!({})) } else { res(DOWN, "the user database does not answer", json!({})) }),
        probe("audit_logs", "Audit logs (hash chains)", "core", move || {
            let last = st.get(300.0);
            let (sec, rev) = (&last["security"], &last["review"]);
            let extra = json!({"security_events": sec["events"].as_i64().unwrap_or(0), "review_events": rev["events"].as_i64().unwrap_or(0), "verified_at": last["at"]});
            if sec["ok"] == true && rev["ok"] == true {
                return res(OK, format!("both logs verified: {} security and {} review events, chains intact", sec["events"].as_i64().unwrap_or(0), rev["events"].as_i64().unwrap_or(0)), extra);
            }
            let bad: Vec<&str> = [("security", sec), ("review", rev)].iter().filter(|(_, r)| r["ok"] != true).map(|(n, _)| *n).collect();
            res(DOWN, format!("the {} log failed verification: treat the history as untrusted until explained", bad.join(" and ")), extra)
        }),
        probe("disk", "Disk space for the logs", "core", move || {
            match free_disk(&dd) {
                None => res(UNKNOWN, "disk space could not be read", json!({})),
                Some((free, total)) => {
                    let ratio = free as f64 / total.max(1) as f64;
                    let extra = json!({"free_bytes": free, "total_bytes": total});
                    if ratio < 0.02 {
                        res(DOWN, format!("{:.0}% of the disk is free: audit logs can no longer be written", ratio * 100.0), extra)
                    } else if ratio < 0.10 {
                        res(DEGRADED, format!("{:.0}% of the disk is free", ratio * 100.0), extra)
                    } else {
                        res(OK, format!("{:.0}% of the disk is free ({} GB)", ratio * 100.0, free >> 30), extra)
                    }
                }
            }
        }),
        probe("anchor", "Log anchor (tamper evidence)", "core", move || {
            let s = cg_audit::anchor_status(sl.path());
            if s["key_configured"] != true {
                return res(DEGRADED, "no anchor key is configured, so tampering by whole-file replacement could go unnoticed", s);
            }
            if s["anchor_signed"] != true {
                return res(DEGRADED, "the log anchor is not yet signed", s);
            }
            res(OK, "the log head is anchored and signed", s)
        }),
        probe("timestamp", "External timestamp (RFC 3161)", "optional", move || {
            // the trusted CA file (TSA_CA_FILE) lets the probe check the token itself; without it a token is reported as unverified
            let ca = std::env::var("TSA_CA_FILE").ok().filter(|p| !p.is_empty()).and_then(|p| std::fs::read_to_string(p).ok());
            let log = sl3.path().to_path_buf();
            let st = cg_timestamp::timestamp_status(&log, &cg_audit::anchor_path_of(&log), ca.as_deref());
            match st["state"].as_str().unwrap_or("") {
                "current" => res(OK, "an RFC 3161 timestamp covers the whole log", st),
                "none" => res(UNKNOWN, "no external timestamp (optional: stamp the log with a Time-Stamp Authority)", st),
                "stale" => res(OK, &format!("timestamp valid; {} newer events are not stamped yet", st["unstamped_events"]), st),
                other => res(DEGRADED, &format!("timestamp state: {other}"), st),
            }
        }),
        probe("ledger", "Signed ledger (Ed25519 checkpoints)", "optional", move || {
            let r = ledger.get(60.0);
            if r["configured"] != true {
                return res(UNKNOWN, "no ledger key (optional: the worker signs checkpoints when it has LEDGER_SIGNING_KEY; this server verifies with the public key)", r);
            }
            if r["ok"] != true {
                let first = r["logs"].as_object().into_iter().flatten().flat_map(|(label, v)| v["problems"].as_array().into_iter().flatten().map(move |p| format!("{label} log: {}", p.as_str().unwrap_or(""))).collect::<Vec<_>>()).next().unwrap_or_default();
                return res(DOWN, format!("a signed checkpoint does not match its log ({first}): treat the history as untrusted until explained"), r);
            }
            let newest = r["logs"].as_object().into_iter().flatten().filter_map(|(_, v)| parse_time(&v["latest"]["time"])).fold(f64::MIN, f64::max);
            let count: i64 = r["logs"].as_object().into_iter().flatten().map(|(_, v)| v["checkpoints"].as_i64().unwrap_or(0)).sum();
            if count == 0 {
                return res(DEGRADED, "the ledger key is configured but no checkpoint has been signed yet", r);
            }
            let age = ck_ledger() - newest;
            if age > 3600.0 {
                return res(DEGRADED, format!("the newest signed checkpoint is {:.0} minutes old: is the worker running with LEDGER_SIGNING_KEY?", age / 60.0), r);
            }
            res(OK, format!("{count} signed checkpoint(s) verify against both logs with the public key; the newest is {:.0} minute(s) old", (age / 60.0).max(0.0)), r)
        }),
        probe("signals", "Security signals (last hour)", "optional", move || {
            let rows = sl2.scan(MAX_SCAN_ROWS).unwrap_or_default();
            let cutoff = ck() - HOUR as f64;
            let recent: Vec<&Value> = rows.iter().filter(|r| parse_time(&r["recorded_at"]).unwrap_or(0.0) >= cutoff).collect();
            let mut names = FAILURE_EVENTS;
            names.sort();
            let counts: Map<String, Value> = names.iter().map(|k| ((*k).to_string(), json!(recent.iter().filter(|r| r["event"]["event_type"] == *k).count()))).collect();
            let extra = json!({"last_hour": counts});
            let n = |k: &str| counts[k].as_i64().unwrap_or(0);
            if n("lockout") > 0 {
                return res(DEGRADED, format!("{} account lockout(s) in the last hour", n("lockout")), extra);
            }
            if n("login_failure") >= 20 {
                return res(DEGRADED, format!("{} failed logins in the last hour", n("login_failure")), extra);
            }
            res(OK, format!("{} failed logins, {} refused requests in the last hour", n("login_failure"), n("forbidden")), extra)
        }),
    ]
}

#[cfg(windows)]
fn free_disk(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }
    let dir = if path.exists() { path.to_path_buf() } else { path.parent()?.to_path_buf() };
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: the pointers are to live locals and the path is NUL-terminated.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
    (ok != 0).then_some((free, total))
}

#[cfg(unix)]
fn free_disk(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct StatVfs {
        f_bsize: u64, f_frsize: u64, f_blocks: u64, f_bfree: u64, f_bavail: u64, f_files: u64, f_ffree: u64, f_favail: u64, f_fsid: u64, f_flag: u64, f_namemax: u64, _spare: [i32; 6],
    }
    extern "C" {
        fn statvfs(path: *const std::os::raw::c_char, buf: *mut StatVfs) -> i32;
    }
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: StatVfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid C string and s is a properly sized, writable struct for this libc layout (Linux x86_64 / aarch64).
    let ok = unsafe { statvfs(c.as_ptr(), &mut s) };
    (ok == 0).then_some((s.f_bavail * s.f_frsize, s.f_blocks * s.f_frsize))
}

const STUCK_OUTBOX_SECONDS: f64 = 120.0;
const STUCK_LEASE_SECONDS: f64 = 180.0;
const OLD_WAIT_SECONDS: f64 = 1800.0;

/// The queue's share of the health report: store, broker, scheduled jobs, outbox, claim flow and the AI helper's circuit breaker.
pub fn queue_probes(queue: Arc<QueueService>, breaker: Arc<CircuitBreaker>, model_name: String, broker: Option<Arc<dyn cg_queue::broker::Broker>>, redis_url: Option<String>, demo: bool, clock: Clock) -> Vec<Probe> {
    let store = queue.store.clone();
    let (s1, s2, s3, s4, s5) = (store.clone(), store.clone(), store.clone(), store.clone(), store);
    let (c1, c2, c3, c4) = (clock.clone(), clock.clone(), clock.clone(), clock);
    let q = queue;
    vec![
        probe("queue_store", "Queue database", "core", move || if s1.ping() { res(OK, "answers a ping", json!({})) } else { res(DOWN, "the queue database does not answer", json!({})) }),
        probe("broker", "Message broker (Redis)", "core", move || {
            if demo {
                return res(OK, "in-process (demo mode, no broker)", json!({}));
            }
            if let Some(b) = &broker {
                return match b.stats() {
                    Ok(st) => {
                        let waiting = st.length.saturating_sub(st.pending);
                        let detail = format!("{} waiting, {} being processed by {} consumers, oldest in progress {} s", waiting, st.pending, st.consumers, st.oldest_pending_idle_ms / 1000);
                        let status = if st.oldest_pending_idle_ms > 300_000 { DEGRADED } else { OK };
                        res(status, detail, json!({"waiting": waiting, "in_progress": st.pending, "consumers": st.consumers, "oldest_in_progress_seconds": st.oldest_pending_idle_ms / 1000}))
                    }
                    Err(_) => res(DOWN, "the broker does not answer (claims wait in the outbox and nothing is lost)", json!({})),
                };
            }
            match &redis_url {
                None => res(UNKNOWN, "the broker is not configured in this process (claims are processed by the built-in worker)", json!({})),
                Some(url) => match tcp_ping(url) {
                    true => res(OK, "the broker accepts connections", json!({})),
                    false => res(DOWN, "the broker does not answer", json!({})),
                },
            }
        }),
        probe("scheduler", "Scheduled jobs and workers", "core", move || {
            let statuses = job_status(&s2.jobs().unwrap_or_default(), c1(), 3.0);
            let w = worst(statuses.iter().map(|s| s["status"].as_str().unwrap_or(UNKNOWN))).to_string();
            let detail = if w == OK { "all four scheduled jobs are running on time".to_string() } else { statuses.iter().filter(|s| s["status"] != OK).map(|s| format!("{}: {}", s["name"].as_str().unwrap_or(""), s["detail"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ") };
            res(&w, detail, json!({"jobs": statuses}))
        }),
        probe("audit_guard", "Audit log protection (verification, mirror, timestamps)", "core", move || {
            let env: std::collections::HashMap<String, String> = std::env::vars().collect();
            let expected = audit_guard_jobs(&env);
            let statuses = audit_guard_status(&s5.jobs().unwrap_or_default(), &expected, c4());
            let w = worst(statuses.iter().map(|s| s["status"].as_str().unwrap_or(UNKNOWN))).to_string();
            let protected = expected.len() > 1;
            let mut detail = statuses.iter().filter(|s| s["status"] != OK).map(|s| format!("{}: {}", s["name"].as_str().unwrap_or(""), s["detail"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ");
            if detail.is_empty() {
                detail = if protected { "the logs verify and every configured copy and timestamp is current".to_string() } else { "the logs verify; no mirror and no external timestamp are configured, so tampering is detectable but not provable".to_string() };
            }
            res(&w, detail, json!({"jobs": statuses, "mirror": expected.iter().any(|(n, _)| *n == "audit_mirror"), "witness": expected.iter().any(|(n, _)| *n == "audit_witness")}))
        }),
        probe("outbox", "Publishing of new claims", "core", move || {
            let pending = s3.pending_outbox(500).unwrap_or_default();
            let now = c2();
            let oldest = pending.iter().map(|d| now - d["receipt"]["created_at"].as_f64().unwrap_or(now)).fold(0.0, f64::max);
            let extra = json!({"pending": pending.len(), "oldest_seconds": (oldest * 10.0).round() / 10.0});
            if !pending.is_empty() && oldest > STUCK_OUTBOX_SECONDS {
                return res(DEGRADED, format!("{} claims wait to be published, the oldest for {} s: the relay or the broker is stuck", pending.len(), oldest as i64), extra);
            }
            res(OK, format!("{} waiting to be published", pending.len()), extra)
        }),
        probe("flow", "Claim flow", "core", {
            let q = q.clone();
            move || {
                let now = c3();
                let dead = s4.dead_letters(200).unwrap_or_default();
                let lapsed = s4.expired(now).unwrap_or_default().into_iter().filter(|d| now - d["lease"]["expires_at"].as_f64().unwrap_or(now) > STUCK_LEASE_SECONDS).count();
                let dash = q.dashboard_snapshot();
                let extra = json!({"dead_letters": dead.len(), "lapsed_leases": lapsed, "waiting": dash["waiting"], "oldest_waiting_seconds": dash["oldest_waiting_seconds"], "shortages": dash["shortages"], "counts": dash["counts"]});
                let mut problems: Vec<String> = vec![];
                if !dead.is_empty() {
                    problems.push(format!("{} claims in the dead-letter list", dead.len()));
                }
                if lapsed > 0 {
                    problems.push(format!("{lapsed} lapsed leases not yet returned"));
                }
                problems.extend(dash["shortages"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)));
                let oldest = dash["oldest_waiting_seconds"].as_object().into_iter().flat_map(|o| o.values()).filter_map(Value::as_f64).fold(0.0, f64::max);
                if oldest > OLD_WAIT_SECONDS {
                    problems.push("a claim has waited more than 30 minutes".into());
                }
                if problems.is_empty() { res(OK, "claims are flowing, nothing is stuck", extra) } else { res(DEGRADED, problems.join("; "), extra) }
            }
        }),
        probe("model", "AI explanation helper", "optional", move || {
            let state = breaker.state();
            if model_name == "none" {
                return res(OK, "no model configured: explanations use the built-in templates", json!({"model": model_name, "breaker": state}));
            }
            if state == "closed" {
                return res(OK, format!("{model_name}: breaker closed"), json!({"model": model_name, "breaker": state}));
            }
            res(DEGRADED, format!("{model_name}: breaker {state}; explanations fall back to templates, nothing is blocked"), json!({"model": model_name, "breaker": state}))
        }),
    ]
}

/// A plain TCP connect to a `redis://host:port/...` address with a one-second limit.
fn tcp_ping(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or("").rsplit('@').next().unwrap_or("");
    let addr = if host.contains(':') { host.to_string() } else { format!("{host}:6379") };
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs().ok().and_then(|mut a| a.next()).is_some_and(|a| std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(1)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn clock() -> (Clock, Arc<AtomicU64>) {
        let t = Arc::new(AtomicU64::new(1_000_000));
        let t2 = t.clone();
        (Arc::new(move || t2.load(Ordering::SeqCst) as f64), t)
    }

    #[test]
    fn identifiers_collapse_and_query_strings_are_cut() {
        assert_eq!(route_shape("/api/v1/claims/CG-123/findings/R001/decision"), "/api/v1/claims/:id/findings/:id/decision");
        assert_eq!(route_shape("/api/v1/users/CG-2002/unlock"), "/api/v1/users/:id/unlock");
        assert_eq!(route_shape("/api/v1/audit/events?badge=CG-1&claim_id=SECRET"), "/api/v1/audit/events");
    }

    #[test]
    fn metrics_count_statuses_percentiles_series_and_bounded_tables() {
        let (c, t) = clock();
        let m = Metrics::new(c);
        for _ in 0..90 {
            m.record("GET", "/api/v1/queue/dashboard", 200, 0.004);
        }
        for _ in 0..10 {
            m.record("GET", "/api/v1/queue/dashboard", 200, 0.4);
        }
        m.record("GET", "/api/v1/x", 404, 0.001);
        m.record("POST", "/api/v1/y", 500, 0.001);
        let s = m.snapshot(5);
        assert_eq!((s["requests"].as_i64(), s["client_errors"].as_i64(), s["server_errors"].as_i64(), s["p50_ms"].as_f64(), s["p99_ms"].as_f64()), (Some(102), Some(1), Some(1), Some(5.0), Some(500.0)));
        assert_eq!(s["recent_errors"][0]["status"], 500);
        assert_eq!(s["series"].as_array().unwrap().len(), 5);
        t.fetch_add(3600, Ordering::SeqCst);
        assert_eq!(m.snapshot(10)["requests"], 0, "old minutes leave the window");
        for i in 0..500 {
            m.record("GET", &format!("/scan{i}"), 404, 0.001);
        }
        assert!(m.inner.lock().unwrap().routes.len() <= 201);
        for _ in 0..200 {
            m.record("GET", "/a", 500, 0.001);
        }
        assert!(m.inner.lock().unwrap().errors.len() <= MAX_RECENT_ERRORS);
        m.record("GET", "/api/v1/claims/PAT-SECRET-77/findings/R1", 200, 0.001);
        assert!(!m.snapshot(5).to_string().contains("SECRET"));
    }

    #[test]
    fn the_headline_is_worst_of_core_and_unknown_does_not_change_it() {
        let c = |status: &str, weight: &str| json!({"status": status, "weight": weight});
        assert_eq!(overall(&[c(DOWN, "core"), c(OK, "optional")]), DOWN);
        assert_eq!(overall(&[c(OK, "core"), c(DOWN, "optional")]), DEGRADED);
        assert_eq!(overall(&[c(DEGRADED, "core")]), DEGRADED);
        assert_eq!(overall(&[c(UNKNOWN, "core"), c(OK, "core")]), OK);
        assert_eq!(overall(&[]), OK);
    }

    #[test]
    fn a_probe_that_panics_is_down_and_long_details_are_cut() {
        let (c, _) = clock();
        let bad = Probe { id: "x", label: "X", weight: "core", run: Box::new(|| panic!("boom")) };
        assert_eq!(check(&bad, &c)["status"], DOWN);
        let long = Probe { id: "y", label: "Y", weight: "core", run: Box::new(|| (OK.into(), "a".repeat(5000), Map::new())) };
        assert!(check(&long, &c)["detail"].as_str().unwrap().len() <= MAX_DETAIL);
    }

    #[test]
    fn job_status_is_unknown_ok_late_or_failed() {
        assert!(job_status(&[], 5.0, 3.0).iter().all(|j| j["status"] == UNKNOWN));
        let now = 10_000.0;
        let row = |n: &str, age: f64, ok: bool| json!({"name": n, "at": now - age, "ok": ok, "detail": if ok { "" } else { "boom" }, "runs": 5, "failures": 0});
        let by: BTreeMap<String, String> = job_status(&[row("relay", 5.0, true), row("deal", 100.0, true), row("expire", 30.0, true), row("reconcile", 100.0, true)], now, 3.0).iter().map(|j| (j["name"].as_str().unwrap().into(), j["status"].as_str().unwrap().into())).collect();
        assert_eq!((by["relay"].as_str(), by["deal"].as_str(), by["expire"].as_str(), by["reconcile"].as_str()), ("ok", "down", "ok", "ok"));
        assert_eq!(job_status(&[row("relay", 5.0, false)], now, 3.0)[0]["status"], DEGRADED);
    }

    #[test]
    fn audit_guard_jobs_follow_the_environment_and_a_failure_is_down_not_degraded() {
        let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::HashMap<_, _>>();
        let names = |e: &std::collections::HashMap<String, String>| audit_guard_jobs(e).iter().map(|(n, _)| *n).collect::<Vec<_>>();
        assert_eq!(names(&env(&[])), ["audit_verify"]);
        assert_eq!(names(&env(&[("AUDIT_MIRROR_DIR", "/m"), ("TSA_URL", "http://t")])), ["audit_verify", "audit_mirror", "audit_witness"]);
        assert_eq!(names(&env(&[("AUDIT_MIRROR_DIR", "  ")])), ["audit_verify"], "a blank value is off");
        assert_eq!(audit_guard_jobs(&env(&[("AUDIT_VERIFY_SECONDS", "2")]))[0].1, 3600.0, "an absurdly short interval is ignored");
        let now = 10_000.0;
        let row = |n: &str, age: f64, ok: bool| json!({"name": n, "at": now - age, "ok": ok, "detail": if ok { "fine" } else { "the first 12 bytes of the source differ from the mirror" }});
        let exp = [("audit_verify", 3600.0), ("audit_mirror", 300.0)];
        let s = audit_guard_status(&[row("audit_verify", 100.0, true), row("audit_mirror", 100.0, true)], &exp, now);
        assert!(s.iter().all(|j| j["status"] == OK));
        let s = audit_guard_status(&[row("audit_verify", 100.0, true), row("audit_mirror", 100.0, false)], &exp, now);
        assert_eq!(s[1]["status"], DOWN);
        assert!(s[1]["detail"].as_str().unwrap().contains("differ from the mirror"), "the reason is shown");
        let s = audit_guard_status(&[row("audit_verify", 100.0, true), row("audit_mirror", 1000.0, true)], &exp, now);
        assert_eq!(s[1]["status"], DOWN, "a mirror that has stopped running is down too");
        assert_eq!(audit_guard_status(&[], &exp, now)[0]["status"], UNKNOWN);
    }

    fn rows() -> Vec<Value> {
        let now = 1_000_000.0;
        let r = |seq: i64, ago: f64, e: Value| json!({"sequence": seq, "recorded_at": cg_audit::iso_of(std::time::Duration::from_secs_f64(now - ago)), "event": e});
        vec![
            r(1, 7200.0, json!({"event_type": "login_success", "badge_id": "CG-2002"})),
            r(2, 3000.0, json!({"event_type": "login_failure", "reason": "bad"})),
            r(3, 2000.0, json!({"event_type": "decision", "badge_id": "CG-2002", "claim_id": "C1", "rule_id": "R001", "action": "confirm_issue"})),
            r(4, 1000.0, json!({"event_type": "forbidden", "badge_id": "CG-1001", "method": "GET", "path": "/x"})),
            r(5, 10.0, json!({"event_type": "user_updated", "actor": "CG-4004", "badge_id": "CG-2002", "changes": ["level"]})),
        ]
    }

    #[test]
    fn audit_filters_are_exact_and_combine() {
        let seqs = |v: Vec<Value>| v.iter().map(|r| r["sequence"].as_i64().unwrap()).collect::<Vec<_>>();
        let now = 1_000_000.0;
        assert_eq!(seqs(matching(rows(), Some("decision"), None, None, None, None)), [3]);
        assert_eq!(seqs(matching(rows(), None, Some("CG-2002"), None, None, None)), [1, 3, 5]);
        assert_eq!(seqs(matching(rows(), None, None, Some("C1"), None, None)), [3]);
        assert_eq!(seqs(matching(rows(), None, None, Some("C"), None, None)), Vec::<i64>::new());
        assert_eq!(seqs(matching(rows(), None, None, None, Some(now - 2500.0), None)), [3, 4, 5]);
        assert_eq!(seqs(matching(rows(), None, None, None, None, Some(now - 2500.0))), [1, 2]);
        let bad = vec![json!({"sequence": 1, "recorded_at": "not a time", "event": {"event_type": "logout"}})];
        assert!(matching(bad.clone(), None, None, None, Some(0.0), None).is_empty());
        assert_eq!(audit_summary(&bad, now, 3)["total"], 1);
    }

    #[test]
    fn the_summary_counts_hours_actors_and_failures() {
        let s = audit_summary(&rows(), 1_000_000.0, 3);
        assert_eq!((s["series"].as_array().unwrap().len(), s["total"].as_i64(), s["failures"]["login_failure"].as_i64(), s["failures"]["forbidden"].as_i64(), s["by_type"]["decision"].as_i64()), (3, Some(5), Some(1), Some(1), Some(1)));
        let who: Vec<&str> = s["actors"].as_array().unwrap().iter().map(|a| a["badge"].as_str().unwrap()).collect();
        assert!(who.contains(&"CG-2002") && who.contains(&"CG-1001") && who.contains(&"CG-4004"));
        let empty = audit_summary(&[], 1_000_000.0, 24);
        assert_eq!((empty["total"].as_i64(), empty["first_at"].is_null(), empty["actors"].as_array().unwrap().len()), (Some(0), true, 0));
    }

    #[test]
    fn the_audit_status_cache_does_not_re_verify_on_every_call() {
        let (c, t) = clock();
        let calls = Arc::new(AtomicU64::new(0));
        let c2 = calls.clone();
        let st = AuditStatus::new(Box::new(move || { c2.fetch_add(1, Ordering::SeqCst); json!({"security": {"ok": true}, "review": {"ok": true}}) }), c);
        st.get(300.0);
        st.get(300.0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        t.fetch_add(301, Ordering::SeqCst);
        st.get(300.0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        st.refresh();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn disk_space_can_be_read() {
        let dir = std::env::temp_dir();
        let (free, total) = free_disk(&dir).expect("disk size");
        assert!(total > 0 && free <= total);
    }
}
