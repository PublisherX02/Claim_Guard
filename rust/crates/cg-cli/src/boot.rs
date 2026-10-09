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

const SECRET_NAMES: [&str; 4] = ["JWT_SECRET", "AUDIT_ANCHOR_KEY", "PII_KEY", "FERNET_KEY"];

pub struct QueueStack {
    pub store: Arc<dyn QueueStore>,
    pub dispatcher: Arc<Dispatcher>,
    pub service: Arc<QueueService>,
    pub intake: Arc<Intake>,
    pub runtime: Arc<Runtime>,
    pub breaker: Arc<CircuitBreaker>,
    pub engine_fn: EngineFn,
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
                saved.insert(name.into(), json!(if name == "FERNET_KEY" { new_fernet_key() } else { random_token(48) }));
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

fn build_stores(settings: &Settings, env: &HashMap<String, String>, dev: bool, queue: bool) -> Result<(Arc<dyn UserStore>, Option<Arc<dyn QueueStore>>), String> {
    let down = || "the database cannot be reached".to_string();
    if let Some(uri) = &settings.mongo_uri {
        let client = cg_mongo::connect(uri, 3000).map_err(|e| format!("{} ({})", down(), e.to_string().chars().take(120).collect::<String>()))?;
        let users = cg_mongo::MongoUserStore::new(client.clone(), &settings.mongo_db);
        users.ensure_indexes().map_err(|e| format!("{} (users: {e})", down()))?;
        let q: Option<Arc<dyn QueueStore>> = if queue {
            let db = env.get("QUEUE_DB").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{}_queue", settings.mongo_db));
            let qs = cg_mongo::MongoQueueStore::new(client, &db);
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
    let (users, queue_store) = build_stores(&settings, &env, o.dev, o.queue)?;
    let clock = system_clock();
    let securitylog = Arc::new(SecurityLog::open(data_dir.join("security_audit.jsonl"), &settings.audit_anchor_key).map_err(|e| e.to_string())?);
    let access = Arc::new(AccessService::new(users, settings.clone(), securitylog.clone(), clock.clone()));
    let review_log = Arc::new(AuditLog::open(data_dir.join("review_audit.jsonl")).map_err(|e| e.to_string())?);
    let engine = Arc::new(Engine::load(&root).map_err(|e| format!("rule engine: {e}"))?);
    let queue = match queue_store {
        None => None,
        Some(store) => Some(build_queue(store, &env, &root, &engine, &access, &securitylog, &review_log, &clock)?),
    };
    Ok(Boot { env, root, data_dir, settings, clock, access, securitylog, review_log, engine, queue })
}

#[allow(clippy::too_many_arguments)]
fn build_queue(store: Arc<dyn QueueStore>, env: &HashMap<String, String>, root: &Path, engine: &Arc<Engine>, access: &Arc<AccessService>, securitylog: &Arc<SecurityLog>, review_log: &Arc<AuditLog>, clock: &Clock) -> Result<QueueStack, String> {
    let extensions = Arc::new(Extensions::load(root).map_err(|e| format!("extension rules: {e}"))?);
    let dispatcher = Arc::new(Dispatcher { store: store.clone(), agents: agents_from_access(access.clone()), clock: clock.clone(), rng_seed: None, log: Some(securitylog.clone()) });
    let service = Arc::new(QueueService::new(store.clone(), dispatcher.clone(), access.clone(), securitylog.clone(), review_log.clone(), clock.clone()));
    let efn = engine_fn(engine.clone());
    let intake = Arc::new(Intake {
        store: store.clone(),
        engine: efn.clone(),
        log: securitylog.clone(),
        clock: clock.clone(),
        rule_pack_hash: engine.pack.hash(),
        engine_version: engine.code_hash(),
        advisory: Some(flow::advisory_fn(extensions, store.clone())),
    });
    service.set_intake(intake.clone());
    let sc = store.clone();
    let breaker = Arc::new(CircuitBreaker::new(clock.clone()));
    let model: Arc<dyn cg_queue::explain::Model> = cg_queue::model::model_from_env(env, cg_queue::model::rulebook_rules(root)?)?.unwrap_or_else(|| Arc::new(NoModel));
    let explain = Arc::new(ExplainStep::new(store.clone(), model, default_guard(), breaker.clone(), clock.clone(), Arc::new(move || flow::routing_config(&*sc))));
    let runtime = Arc::new(Runtime { store: store.clone(), explain, clock: clock.clone(), max_retries: 3 });
    Ok(QueueStack { store, dispatcher, service, intake, runtime, breaker, engine_fn: efn })
}

/// What the Celery worker and its scheduler do in production, on a thread: publish the outbox (processing runs inline), deal claims into
/// inboxes, return lapsed leases and reconcile. The same functions and stores; only the clock tick is different. Exactly one instance
/// should run against a database.
pub fn start_workers(q: &QueueStack, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let (rt, dispatcher) = (q.runtime.clone(), q.dispatcher.clone());
    std::thread::Builder::new()
        .name("claimguard-worker".into())
        .spawn(move || {
            use cg_api::ops::run_job;
            let every = [("relay", 1.5), ("deal", 2.0), ("expire", 20.0), ("reconcile", 120.0)];
            let mut last = [f64::MIN; 4];
            let mono = std::time::Instant::now();
            let alive = std::env::var("CG_WORKER_ALIVE_FILE").ok().filter(|s| !s.is_empty());
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
                                process(&rt, id, v, h);
                                Ok(())
                            };
                            run_job(store, clock, "relay", || flow::sweep(store, &publish, clock(), 300.0), |r| r.to_string());
                        }
                        "deal" => {
                            run_job(store, clock, "deal", || dispatcher.deal(None, false).assigned.len(), |n| format!("{n} assigned"));
                        }
                        "expire" => {
                            run_job(store, clock, "expire", || dispatcher.expire(None), |n| format!("{n} returned"));
                        }
                        _ => {
                            run_job(store, clock, "reconcile", || flow::reconcile(store, clock(), "system:worker").findings.len(), |n| format!("{n} findings"));
                        }
                    }));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })
        .expect("the worker thread starts")
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
