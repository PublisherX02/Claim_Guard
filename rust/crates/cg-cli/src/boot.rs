//! Wiring shared by `serve`, `worker`, `admin` and `queue`: settings, stores, logs, services and the queue stack, built the same way for
//! all of them (`access/bootstrap.py`, `workqueue/bootstrap.py`).
//!
//! Dev mode keeps its generated secrets in `<data dir>/dev_secrets.json` so a second run can still verify the audit log the first run
//! signed; production reads every secret from the environment.

use cg_access::securitylog::SecurityLog;
use cg_access::service::AccessService;
use cg_access::settings::{load_settings, new_fernet_key, random_token, Settings};
use cg_access::store::{MemoryStore, UserStore};
use cg_access::Clock;
use cg_audit::AuditLog;
use cg_engine::extensions::Extensions;
use cg_engine::Engine;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::broker::Broker;
use cg_queue::consumer::{self, PoolConfig};
use cg_queue::dispatcher::Dispatcher;
use cg_queue::explain::{default_guard, ExplainStep, NoModel};
use cg_queue::flow::{self, engine_fn, process, EngineFn, Intake, Runtime};
use cg_queue::service::{agents_from_access, QueueService};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECRET_NAMES: [&str; 5] = ["JWT_SECRET", "AUDIT_ANCHOR_KEY", "PII_KEY", "FERNET_KEY", "LEDGER_SIGNING_KEY"];

pub struct QueueStack {
    pub store: Arc<dyn QueueStore>,
    pub dispatcher: Arc<Dispatcher>,
    pub service: Arc<QueueService>,
    pub intake: Arc<Intake>,
    pub runtime: Arc<Runtime>,
    pub breaker: Arc<CircuitBreaker>,
    pub engine_fn: EngineFn,
    /// The Redis Streams broker when REDIS_URL is set (never in the demo). None: claims are processed inline by the relay sweep.
    pub broker: Option<Arc<dyn Broker>>,
}

pub struct Boot {
    pub env: HashMap<String, String>,
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub settings: Settings,
    pub clock: Clock,
    pub access: Arc<AccessService>,
    pub securitylog: Arc<SecurityLog>,
    pub review_log: Arc<AuditLog>,
    pub engine: Arc<Engine>,
    /// the payer policies as an administrator has edited them (versioned, applied to `engine` at run time)
    pub payer: Arc<cg_engine::payer::PayerRules>,
    pub queue: Option<QueueStack>,
}

pub fn system_clock() -> Clock {
    Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

pub fn repo_root() -> Result<PathBuf, String> {
    if let Ok(r) = std::env::var("CLAIMGUARD_ROOT") {
        return Ok(r.into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    cwd.ancestors().find(|p| p.join("data/development").is_dir() && p.join("src/access/ui").is_dir()).map(Path::to_path_buf).ok_or_else(|| "cannot find the ClaimGuard folder: run from inside it or set CLAIMGUARD_ROOT".into())
}

/// Secrets the environment does not provide come from a persisted dev file (same format as the Python server's), created on first use.
fn with_dev_secrets(env: &mut HashMap<String, String>, dir: &Path) -> Result<(), String> {
    let path = dir.join("dev_secrets.json");
    let mut saved: serde_json::Map<String, Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut changed = false;
    for name in SECRET_NAMES {
        if env.get(name).is_none_or(|v| v.trim().is_empty()) {
            if !saved.contains_key(name) {
                saved.insert(name.into(), json!(if name == "FERNET_KEY" { new_fernet_key() } else if name == "LEDGER_SIGNING_KEY" { cg_audit::ledger::keygen().0 } else { random_token(48) }));
                changed = true;
            }
            env.insert(name.into(), saved[name].as_str().unwrap_or("").to_string());
        }
    }
    if changed {
        std::fs::write(&path, Value::Object(saved).to_string()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Switches on encryption of the audit logs on disk: a key derived from DATA_KEY (or FERNET_KEY), unless CG_ENCRYPT_AT_REST=false.
pub fn set_audit_encryption(env: &HashMap<String, String>, fernet_key: &str) -> Result<(), String> {
    let at_rest = env.get("CG_ENCRYPT_AT_REST").map(|v| v.trim().to_lowercase()).as_deref() != Some("false");
    let secret = env.get("DATA_KEY").map(|k| k.trim()).filter(|k| !k.is_empty()).unwrap_or(fernet_key);
    // DATA_KEY_PREVIOUS: comma-separated older secrets (old DATA_KEY values, or the old FERNET_KEY if no DATA_KEY was set), so rows written
    // under them stay readable after a rotation.
    let previous: Vec<String> = env.get("DATA_KEY_PREVIOUS").map(|v| v.split(',').map(|s| cg_audit::atrest::derive_key(s.trim())).collect()).unwrap_or_default();
    cg_audit::atrest::set_keys(at_rest.then(|| cg_audit::atrest::derive_key(secret)).as_deref(), &previous).map_err(|e| e.to_string())
}

/// The queue database's name: QUEUE_DB, or the main database's name plus `_queue`.
pub fn queue_db_name(settings: &Settings, env: &HashMap<String, String>) -> String {
    env.get("QUEUE_DB").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{}_queue", settings.mongo_db))
}

fn build_stores(settings: &Settings, env: &HashMap<String, String>, dev: bool, queue: bool) -> Result<(Arc<dyn UserStore>, Option<Arc<dyn QueueStore>>), String> {
    let down = || "the database cannot be reached".to_string();
    if let Some(uri) = &settings.mongo_uri {
        let client = cg_mongo::connect(uri, 3000).map_err(|e| format!("{} ({})", down(), e.to_string().chars().take(120).collect::<String>()))?;
        let mut users = cg_mongo::MongoUserStore::new(client.clone(), &settings.mongo_db);
        // People's names are encrypted like the claim data (same switch, same key).
        if env.get("CG_ENCRYPT_AT_REST").map(|v| v.trim().to_lowercase()).as_deref() != Some("false") {
            users = users.with_encryption(cg_mongo::crypt::FieldCrypt::new(&settings.fernet_key, env.get("DATA_KEY").map(String::as_str)).map_err(|e| format!("configuration error: {}", e.0))?);
        }
        users.ensure_indexes().map_err(|e| format!("{} (users: {e})", down()))?;
        let q: Option<Arc<dyn QueueStore>> = if queue {
            let db = queue_db_name(settings, env);
            let mut qs = cg_mongo::MongoQueueStore::new(client, &db);
            // Claim data is encrypted before it reaches the database unless this is switched off on purpose (CG_ENCRYPT_AT_REST=false).
            // The key is DATA_KEY if given, otherwise derived from FERNET_KEY, so no deployment is left unprotected by a missing setting.
            if env.get("CG_ENCRYPT_AT_REST").map(|v| v.trim().to_lowercase()).as_deref() != Some("false") {
                let crypt = cg_mongo::crypt::FieldCrypt::new(&settings.fernet_key, env.get("DATA_KEY").map(String::as_str)).map_err(|e| format!("configuration error: {}", e.0))?;
                qs = qs.with_encryption(crypt);
            }
            qs.ensure_indexes().map_err(|e| format!("{} (queue: {e})", down()))?;
            Some(Arc::new(qs))
        } else {
            None
        };
        Ok((Arc::new(users), q))
    } else if dev {
        Ok((Arc::new(MemoryStore::new()), queue.then(|| Arc::new(MemoryQueueStore::new()) as Arc<dyn QueueStore>)))
    } else {
        Err("MONGO_URI is required".into())
    }
}

pub struct Options {
    pub dev: bool,
    pub host: String,
    pub queue: bool,
    pub demo: bool,
    pub data_dir: Option<PathBuf>,
}

pub fn boot(o: &Options) -> Result<Boot, String> {
    let root = repo_root()?;
    let mut env: HashMap<String, String> = std::env::vars().collect();
    if o.demo {
        env.entry("COOKIE_SECURE".into()).or_insert_with(|| "false".into()); // the demo is plain http on this PC
    }
    let data_dir = o.data_dir.clone().or_else(|| env.get("CLAIMGUARD_DATA_DIR").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("instance"));
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    if o.dev {
        with_dev_secrets(&mut env, &data_dir)?;
    }
    let settings = load_settings(&env, o.dev, &o.host).map_err(|e| format!("configuration error: {}", e.0))?;
    set_audit_encryption(&env, &settings.fernet_key)?;
    let (users, queue_store) = build_stores(&settings, &env, o.dev, o.queue)?;
    let clock = system_clock();
    let securitylog = Arc::new(SecurityLog::open(data_dir.join("security_audit.jsonl"), &settings.audit_anchor_key).map_err(|e| e.to_string())?);
    let access = Arc::new(AccessService::new(users, settings.clone(), securitylog.clone(), clock.clone()));
    let review_log = Arc::new(AuditLog::open(data_dir.join("review_audit.jsonl")).map_err(|e| e.to_string())?);
    let engine = Arc::new(Engine::load(&root).map_err(|e| format!("rule engine: {e}"))?);
    let payer = Arc::new(cg_engine::payer::PayerRules::open(&root, &data_dir, engine.clone()).map_err(|e| format!("payer rules: {e}"))?);
    let learning_store = queue_store.clone();
    {
        // an approved change of the payer rules, or an approved use of a learned reliability snapshot, is carried out here; other processes
        // (the worker, the API) notice a payer change within seconds
        let pr = payer.clone();
        let log_access = access.clone();
        access.set_action_hook(Arc::new(move |a, requester, approver| match a.action.as_str() {
            "payer_rules" => {
                let p: Value = serde_json::from_str(&a.payload).map_err(|e| e.to_string())?;
                pr.apply(&p["policies"], requester, Some(approver), p["reason"].as_str().unwrap_or("")).map(|_| ())
            }
            "learning_snapshot" => {
                let store = learning_store.as_ref().ok_or("the queue is not running in this process")?;
                let p: Value = serde_json::from_str(&a.payload).map_err(|e| e.to_string())?;
                let version = p["version"].as_i64().ok_or("the request has no version")?;
                cg_queue::reliability::activate(&**store, version, requester, Some(approver), log_access.now())?;
                log_access.record("reliability_activated", serde_json::json!({"actor": requester, "version": version, "approver": approver})).map_err(|e| e.to_string())
            }
            _ => Err("unknown action".into()),
        }));
        let pr = payer.clone();
        let _ = std::thread::Builder::new().name("claimguard-payer-refresh".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_secs(3));
            let _ = pr.refresh();
        });
    }
    let queue = match queue_store {
        None => None,
        Some(store) => Some(build_queue(store, &env, &root, &engine, &access, &securitylog, &review_log, &clock, !o.demo)?),
    };
    Ok(Boot { env, root, data_dir, settings, clock, access, securitylog, review_log, engine, payer, queue })
}

#[allow(clippy::too_many_arguments)]
fn build_queue(store: Arc<dyn QueueStore>, env: &HashMap<String, String>, root: &Path, engine: &Arc<Engine>, access: &Arc<AccessService>, securitylog: &Arc<SecurityLog>, review_log: &Arc<AuditLog>, clock: &Clock, use_broker: bool) -> Result<QueueStack, String> {
    let extensions = Arc::new(Extensions::load(root).map_err(|e| format!("extension rules: {e}"))?);
    let dispatcher = Arc::new(Dispatcher { store: store.clone(), agents: agents_from_access(access.clone()), clock: clock.clone(), rng_seed: None, log: Some(securitylog.clone()) });
    let service = Arc::new(QueueService::new(store.clone(), dispatcher.clone(), access.clone(), securitylog.clone(), review_log.clone(), clock.clone()));
    let efn = engine_fn(engine.clone());
    let intake = Arc::new(Intake {
        store: store.clone(),
        engine: efn.clone(),
        log: securitylog.clone(),
        clock: clock.clone(),
        rule_pack_hash: { let e = engine.clone(); Arc::new(move || e.rulebook_hash()) },
        engine_version: engine.code_hash(),
        advisory: Some(flow::advisory_fn(extensions, store.clone())),
    });
    service.set_intake(intake.clone());
    let sc = store.clone();
    let breaker = Arc::new(CircuitBreaker::new(clock.clone()));
    let model: Arc<dyn cg_queue::explain::Model> = cg_queue::model::model_from_env(env, cg_queue::model::rulebook_rules(root)?)?.unwrap_or_else(|| Arc::new(NoModel));
    let explain = Arc::new(ExplainStep::new(store.clone(), model, default_guard(), breaker.clone(), clock.clone(), Arc::new(move || flow::routing_config(&*sc))));
    let runtime = Arc::new(Runtime { store: store.clone(), explain, clock: clock.clone(), max_retries: 3 });
    let broker: Option<Arc<dyn Broker>> = match env.get("REDIS_URL").map(String::as_str).filter(|s| use_broker && !s.is_empty()) {
        Some(url) => {
            let stream = env.get("CG_BROKER_STREAM").map(String::as_str).filter(|s| !s.is_empty()).unwrap_or(cg_redis::DEFAULT_STREAM);
            Some(Arc::new(cg_redis::RedisBroker::lazy(url, stream, cg_redis::DEFAULT_GROUP)?))
        }
        None => None,
    };
    Ok(QueueStack { store, dispatcher, service, intake, runtime, breaker, engine_fn: efn, broker })
}

/// The consumer pool's settings from the environment. CG_WORKER_CONCURRENCY (default 4, 1 to 64) is how many claims are processed at the
/// same moment, so also the most the AI model is ever asked at once; set it to the number of parallel slots the model server has.
/// CG_BROKER_MIN_IDLE_SECONDS (default: the explanation ceiling plus 30) is how long a message may stay unacknowledged before another
/// consumer takes it over; it is never allowed below the ceiling plus 10, or a slow but live consumer would have its message taken.
pub fn pool_config(env: &HashMap<String, String>, deadline_seconds: f64, max_attempts: u32) -> PoolConfig {
    let mut cfg = PoolConfig::production(deadline_seconds);
    if let Some(n) = env.get("CG_WORKER_CONCURRENCY").and_then(|v| v.trim().parse::<usize>().ok()) {
        cfg.concurrency = n.clamp(1, 64);
    }
    if let Some(s) = env.get("CG_BROKER_MIN_IDLE_SECONDS").and_then(|v| v.trim().parse::<f64>().ok()) {
        cfg.min_idle_ms = (s.max(deadline_seconds + 10.0) * 1000.0) as u64;
    }
    cfg.max_attempts = max_attempts.max(1);
    cfg
}

/// What the Celery worker and its scheduler do in production, on a thread: publish the outbox (processing runs inline), deal claims into
/// inboxes, return lapsed leases and reconcile. The same functions and stores; only the clock tick is different. Exactly one instance
/// should run against a database.
pub fn start_workers(q: &QueueStack, stop: Arc<AtomicBool>, env: &HashMap<String, String>) -> std::thread::JoinHandle<()> {
    let (rt, dispatcher, broker) = (q.runtime.clone(), q.dispatcher.clone(), q.broker.clone());
    let cfg = pool_config(env, q.runtime.explain.deadline_seconds, q.runtime.max_retries);
    // a claim unfinished this long with an empty broker is a lost message (never less than two minutes, so a healthy claim is never re-sent)
    let republish_after = env.get("CG_REPUBLISH_AFTER_SECONDS").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(900.0).max(120.0);
    std::thread::Builder::new()
        .name("claimguard-worker".into())
        .spawn(move || {
            use cg_api::ops::run_job;
            let every = [("relay", 1.5), ("deal", 2.0), ("expire", 20.0), ("reconcile", 120.0), ("republish", 300.0), ("learn", 600.0)];
            let mut last = [f64::MIN; 6];
            let mono = std::time::Instant::now();
            let alive = std::env::var("CG_WORKER_ALIVE_FILE").ok().filter(|s| !s.is_empty());
            // with a broker the claims are processed by the pool (at most `concurrency` at once); without one, inline in the relay sweep
            let pool = broker.clone().map(|b| consumer::start(b, rt.clone(), cfg.clone(), stop.clone(), &format!("w{}-{}", std::process::id(), random_token(4))));
            while !stop.load(Ordering::SeqCst) {
                if let Some(f) = &alive {
                    let _ = std::fs::write(f, b"alive"); // touched every tick so a container health check can tell the loop is running
                }
                let now = mono.elapsed().as_secs_f64();
                for (i, (name, gap)) in every.iter().enumerate() {
                    if now - last[i] < *gap {
                        continue;
                    }
                    last[i] = now;
                    let (store, clock) = (&*rt.store, &rt.clock);
                    // a failing tick must not stop the loop: the next one retries
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match *name {
                        "relay" => {
                            let publish = |id: &str, v: i64, h: &str| -> Result<(), String> {
                                match &broker {
                                    Some(b) => b.publish(id, v, h),
                                    None => {
                                        process(&rt, id, v, h);
                                        Ok(())
                                    }
                                }
                            };
                            run_job(store, clock, "relay", || flow::sweep(store, &publish, clock(), 300.0), |r| r.to_string());
                        }
                        "deal" => {
                            run_job(store, clock, "deal", || dispatcher.deal(None, false).assigned.len(), |n| format!("{n} assigned"));
                        }
                        "expire" => {
                            run_job(store, clock, "expire", || dispatcher.expire(None), |n| format!("{n} returned"));
                        }
                        "republish" => {
                            if let Some(b) = &broker {
                                run_job(store, clock, "republish", || flow::republish_lost(store, &**b, clock(), republish_after), |r| r.to_string());
                            }
                        }
                        "learn" => {
                            // proposes a new reliability snapshot from the decisions made so far; routing changes only when two administrators activate it
                            let cfg = flow::routing_config(store);
                            if cfg.learn_enabled {
                                let _ = cg_api::ops::run_job_checked(store, clock, "learn", || cg_queue::reliability::learn(store, &cfg, clock()), |p| format!("{}", p.as_ref().map_or("no change".to_string(), |s| format!("proposed v{}", s["version"]))));
                            }
                        }
                        _ => {
                            run_job(store, clock, "reconcile", || flow::reconcile(store, clock(), "system:worker").findings.len(), |n| format!("{n} findings"));
                        }
                    }));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if let Some(p) = pool {
                p.join();
            }
        })
        .expect("the worker thread starts")
}

/// Protects the audit logs from a quiet rewrite, on its own thread so a slow check never delays the queue:
/// * `audit_verify`: both hash chains and their anchors, every AUDIT_VERIFY_SECONDS (default one hour);
/// * `audit_mirror`: when AUDIT_MIRROR_DIR is set, an append-only copy on another volume that refuses to follow a log that was edited,
///   every AUDIT_MIRROR_SECONDS (default five minutes);
/// * `audit_witness`: when TSA_URL is set, an RFC 3161 timestamp of each anchor, every AUDIT_WITNESS_SECONDS (default one hour).
/// Each run is recorded as a job, so the health panel shows a failure (a chain that no longer verifies, a log that no longer matches its
/// mirror, an unreachable authority) instead of leaving it in a log nobody reads.
pub fn start_audit_guard(b: &Boot, q: &QueueStack, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    use cg_api::ops::{audit_guard_jobs, run_job_checked};
    let store = q.store.clone();
    let clock = b.clock.clone();
    let env = b.env.clone();
    let logs: Vec<(String, std::path::PathBuf)> = vec![("security".into(), b.securitylog.path().to_path_buf()), ("review".into(), b.review_log.path().to_path_buf())];
    let security = b.securitylog.clone();
    let data_dir = b.data_dir.clone();
    std::thread::Builder::new()
        .name("claimguard-audit-guard".into())
        .spawn(move || {
            let jobs = audit_guard_jobs(&env);
            let mut last: Vec<f64> = vec![f64::MIN; jobs.len()];
            let mono = std::time::Instant::now();
            while !stop.load(Ordering::SeqCst) {
                let now = mono.elapsed().as_secs_f64();
                for (i, (name, every)) in jobs.iter().enumerate() {
                    if now - last[i] < *every {
                        continue;
                    }
                    last[i] = now;
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match *name {
                        "audit_verify" => {
                            let _ = run_job_checked(&*store, &clock, name, || {
                                let s = security.verify();
                                if s["ok"] != true {
                                    return Err(format!("security log: {}", s["error"].as_str().unwrap_or("does not verify")));
                                }
                                let review = &logs[1].1;
                                if review.exists() {
                                    cg_audit::verify_with_anchor(review, None, false).map_err(|e| format!("review log: {e}"))?;
                                }
                                Ok(format!("{} security events verified", s["events"]))
                            }, |d| d.clone());
                        }
                        "audit_mirror" => {
                            let dir = std::path::PathBuf::from(env.get("AUDIT_MIRROR_DIR").cloned().unwrap_or_default());
                            let _ = run_job_checked(&*store, &clock, name, || {
                                let mut copied = 0;
                                for (label, path) in &logs {
                                    let target = dir.join(path.file_name().unwrap_or_default());
                                    copied += cg_audit::mirror::sync_mirror(path, &target).map_err(|e| format!("{label} log: {e}"))?;
                                }
                                Ok(format!("{copied} bytes copied"))
                            }, |d| d.clone());
                        }
                        "audit_ledger" => {
                            // signed checkpoints over both logs: the seed is in this process only; the API verifies with the public key written beside the data
                            let seed = env.get("LEDGER_SIGNING_KEY").cloned().unwrap_or_default();
                            let dir = data_dir.clone();
                            let _ = run_job_checked(&*store, &clock, name, || {
                                let key = cg_audit::ledger::signing_key_from(&seed)?;
                                let public = cg_audit::ledger::public_of_seed(&seed)?;
                                if std::fs::read_to_string(dir.join("ledger.pub")).map(|t| t.trim() != public).unwrap_or(true) {
                                    std::fs::write(dir.join("ledger.pub"), &public).map_err(|e| e.to_string())?;
                                }
                                let mut signed = 0;
                                for (label, path) in &logs {
                                    if path.exists() && cg_audit::ledger::sign_if_new(path, label, &key).map_err(|e| format!("{label} log: {e}"))?.is_some() {
                                        signed += 1;
                                    }
                                }
                                Ok(format!("{signed} new checkpoint(s)"))
                            }, |d| d.clone());
                        }
                        _ => {
                            let url = env.get("TSA_URL").cloned().unwrap_or_default();
                            let _ = run_job_checked(&*store, &clock, name, || {
                                let mut stamped = 0;
                                for (label, path) in &logs {
                                    if !path.exists() {
                                        continue;
                                    }
                                    cg_timestamp::stamp_log(path, &cg_audit::anchor_path_of(path), &url, 20.0).map_err(|e| format!("{label} log: {e}"))?;
                                    stamped += 1;
                                }
                                Ok(format!("{stamped} anchors stamped"))
                            }, |d| d.clone());
                        }
                    }));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .expect("the audit guard thread starts")
}

/// Resolves when the process is asked to stop (Ctrl-C, or SIGTERM from a container runtime).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn pool_settings_have_safe_defaults_and_limits() {
        let d = pool_config(&env(&[]), 90.0, 3);
        assert_eq!((d.concurrency, d.max_attempts, d.min_idle_ms), (4, 3, 120_000), "four at once, three tries, ceiling plus 30 s");
        assert_eq!(pool_config(&env(&[("CG_WORKER_CONCURRENCY", "8")]), 90.0, 3).concurrency, 8);
        assert_eq!(pool_config(&env(&[("CG_WORKER_CONCURRENCY", "0")]), 90.0, 3).concurrency, 1, "never zero threads");
        assert_eq!(pool_config(&env(&[("CG_WORKER_CONCURRENCY", "100000")]), 90.0, 3).concurrency, 64, "never an absurd number");
        assert_eq!(pool_config(&env(&[("CG_WORKER_CONCURRENCY", "lots")]), 90.0, 3).concurrency, 4, "nonsense is ignored");
        assert_eq!(pool_config(&env(&[]), 90.0, 0).max_attempts, 1);
    }

    #[test]
    fn a_takeover_can_never_come_before_the_explanation_ceiling_has_passed() {
        let low = pool_config(&env(&[("CG_BROKER_MIN_IDLE_SECONDS", "1")]), 90.0, 3);
        assert!(low.min_idle_ms >= 100_000, "a live consumer inside the 90 s ceiling must keep its message, got {}", low.min_idle_ms);
        assert_eq!(pool_config(&env(&[("CG_BROKER_MIN_IDLE_SECONDS", "300")]), 90.0, 3).min_idle_ms, 300_000);
    }
}
