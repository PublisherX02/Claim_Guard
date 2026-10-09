//! `cg serve`: builds the whole stack (settings, stores, logs, services, queue, workers) and runs the reviewer API
//! (`scripts/serve_access.py`, `access/bootstrap.py`, `workqueue/demo.py`).

use cg_access::passwords;
use cg_access::securitylog::SecurityLog;
use cg_access::service::AccessService;
use cg_access::settings::{load_settings, new_fernet_key, random_token, Settings};
use cg_access::store::{MemoryStore, UserStore};
use cg_access::Clock;
use cg_api::claimstore::FileClaimStore;
use cg_api::ops::{queue_probes, run_job};
use cg_api::server;
use cg_api::{Api, ApiParts};
use cg_audit::AuditLog;
use cg_engine::Engine;
use cg_queue::breaker::CircuitBreaker;
use cg_queue::dispatcher::Dispatcher;
use cg_queue::explain::{default_guard, ExplainStep, NoModel};
use cg_queue::flow::{self, engine_fn, process, Intake, Runtime};
use cg_queue::routing_config::{self, RoutingConfig};
use cg_queue::service::{agents_from_access, QueueService};
use cg_queue::store::{MemoryQueueStore, QueueStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOCAL: [&str; 3] = ["127.0.0.1", "localhost", "::1"];
const SECRET_NAMES: [&str; 4] = ["JWT_SECRET", "AUDIT_ANCHOR_KEY", "PII_KEY", "FERNET_KEY"];
const DEMO_ACCOUNTS: [(i64, &str, &str); 6] = [
    (1, "CG-1001", "Demo Viewer"),
    (2, "CG-2002", "Demo Reviewer Amal"),
    (2, "CG-2003", "Demo Reviewer Omar"),
    (3, "CG-3003", "Demo Senior Layla"),
    (3, "CG-3004", "Demo Senior Karim"),
    (4, "CG-4004", "Demo Administrator"),
];

pub const USAGE: &str = "usage: cg serve [--host H] [--port N] [--dev] [--demo] [--queue] [--behind-proxy] [--data-dir D] [--claims FILE]";

struct Args {
    host: String,
    port: u16,
    dev: bool,
    demo: bool,
    queue: bool,
    behind_proxy: bool,
    data_dir: Option<PathBuf>,
    claims: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { host: "127.0.0.1".into(), port: 8443, dev: false, demo: false, queue: false, behind_proxy: false, data_dir: None, claims: None };
    let mut it = args.iter();
    while let Some(x) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match x.as_str() {
            "--host" => a.host = value("--host")?,
            "--port" => a.port = value("--port")?.parse().map_err(|_| "--port must be a number".to_string())?,
            "--dev" => a.dev = true,
            "--demo" => a.demo = true,
            "--queue" => a.queue = true,
            "--behind-proxy" => a.behind_proxy = true,
            "--data-dir" => a.data_dir = Some(value("--data-dir")?.into()),
            "--claims" => a.claims = Some(value("--claims")?.into()),
            "--ssl-keyfile" | "--ssl-certfile" => return Err("TLS is terminated by the reverse proxy: run with --behind-proxy (see docs)".into()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(a)
}

fn system_clock() -> Clock {
    Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

fn repo_root() -> Result<PathBuf, String> {
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

fn build_stores(settings: &Settings, dev: bool, queue: bool) -> Result<(Arc<dyn UserStore>, Option<Arc<dyn QueueStore>>), String> {
    if let Some(uri) = &settings.mongo_uri {
        let client = cg_mongo::connect(uri, 3000).map_err(|_| "the database cannot be reached".to_string())?;
        let users = cg_mongo::MongoUserStore::new(client.clone(), &settings.mongo_db);
        users.ensure_indexes().map_err(|_| "the database cannot be reached".to_string())?;
        let q: Option<Arc<dyn QueueStore>> = if queue {
            let qs = cg_mongo::MongoQueueStore::new(client, &settings.mongo_db);
            qs.ensure_indexes().map_err(|_| "the database cannot be reached".to_string())?;
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

fn demo_password(badge: &str) -> String {
    loop {
        let candidate = format!("{}aA1!", random_token(14));
        if passwords::check_policy(&candidate, badge).is_empty() {
            return candidate;
        }
    }
}

fn seed_accounts(access: &AccessService) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for (level, badge, name) in DEMO_ACCOUNTS {
        let password = demo_password(badge);
        let (_, uri) = access.provision_user(badge, name, &password, level, "demo", &[], &[], false).map_err(|e| format!("{e:?}"))?;
        let secret = uri.split("secret=").nth(1).and_then(|s| s.split('&').next()).unwrap_or("").to_string();
        out.insert(badge.into(), json!({"level": level, "name": name, "password": password, "secret": secret}));
    }
    Ok(Value::Object(out))
}

fn configure_shift(store: &dyn QueueStore, badges: Vec<String>) -> Result<(), String> {
    let latest = store.latest_config().map_err(|e| e.to_string())?;
    let mut cfg = latest.as_ref().map(routing_config::from_doc).transpose()?.unwrap_or_default();
    let expected = latest.as_ref().and_then(|l| l["version"].as_i64()).unwrap_or(0);
    cfg = RoutingConfig { version: expected + 1, on_shift: badges, slice_size: 6, low_water: 3, ..cfg };
    routing_config::validate(&cfg)?;
    if !store.put_config(&routing_config::to_doc(&cfg)?, expected).map_err(|e| e.to_string())? {
        return Err("the routing configuration changed while the demo was starting".into());
    }
    Ok(())
}

/// What the Celery worker and its scheduler do in production, on a thread: publish the outbox (processing runs inline), deal claims into
/// inboxes, return lapsed leases and reconcile. The same functions and stores; only the clock tick is different.
fn start_workers(rt: Arc<Runtime>, dispatcher: Arc<Dispatcher>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("claimguard-worker".into())
        .spawn(move || {
            let every = [("relay", 1.5), ("deal", 2.0), ("expire", 20.0), ("reconcile", 120.0)];
            let mut last = [f64::MIN; 4];
            let mono = std::time::Instant::now();
            while !stop.load(Ordering::SeqCst) {
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

pub fn run(args: &[String]) -> i32 {
    let say = |s: &str| println!("{s}");
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            say(&format!("error: {e}\n{USAGE}"));
            return 2;
        }
    };
    let (dev, queue) = (a.dev || a.demo, a.queue || a.demo);
    if dev && !LOCAL.contains(&a.host.as_str()) {
        say("error: dev mode is only allowed on localhost");
        return 2;
    }
    if a.behind_proxy && dev {
        say("error: --behind-proxy is for real deployments, not dev mode");
        return 2;
    }
    if !LOCAL.contains(&a.host.as_str()) && !a.behind_proxy {
        say("error: refusing to listen on a non-local address without a TLS proxy (use --behind-proxy)");
        return 2;
    }
    match start(a, dev, queue) {
        Ok(()) => 0,
        Err(e) => {
            say(&format!("error: {e}"));
            2
        }
    }
}

fn start(a: Args, dev: bool, queue: bool) -> Result<(), String> {
    let root = repo_root()?;
    let mut env: HashMap<String, String> = std::env::vars().collect();
    if a.demo {
        env.entry("COOKIE_SECURE".into()).or_insert_with(|| "false".into());
    }
    let data_dir = a.data_dir.clone().or_else(|| env.get("CLAIMGUARD_DATA_DIR").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("instance"));
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    if dev {
        with_dev_secrets(&mut env, &data_dir)?;
    }
    let settings = load_settings(&env, dev, &a.host).map_err(|e| format!("configuration error: {}", e.0))?;
    let (users, queue_store) = build_stores(&settings, dev, queue)?;
    let clock = system_clock();
    let securitylog = Arc::new(SecurityLog::open(data_dir.join("security_audit.jsonl"), &settings.audit_anchor_key).map_err(|e| e.to_string())?);
    let access = Arc::new(AccessService::new(users, settings.clone(), securitylog.clone(), clock.clone()));

    let review_log = Arc::new(AuditLog::open(data_dir.join("review_audit.jsonl")).map_err(|e| e.to_string())?);
    let engine = Arc::new(Engine::load(&root).map_err(|e| format!("rule engine: {e}"))?);
    let claims_path = a.claims.clone().or_else(|| env.get("CLAIMS_PATH").map(PathBuf::from)).unwrap_or_else(|| root.join("data/development/claims.jsonl"));
    let claims = Arc::new(FileClaimStore::from_jsonl(&claims_path, &engine)?);

    let stop = Arc::new(AtomicBool::new(false));
    let (mut queue_service, mut probes, mut worker) = (None, vec![], None);
    let mut demo_claims: Vec<Value> = vec![];
    if let Some(store) = queue_store {
        let extensions = Arc::new(cg_engine::extensions::Extensions::load(&root).map_err(|e| format!("extension rules: {e}"))?);
        let dispatcher = Arc::new(Dispatcher { store: store.clone(), agents: agents_from_access(access.clone()), clock: clock.clone(), rng_seed: None, log: Some(securitylog.clone()) });
        let svc = Arc::new(QueueService::new(store.clone(), dispatcher.clone(), access.clone(), securitylog.clone(), review_log.clone(), clock.clone()));
        let intake = Arc::new(Intake { store: store.clone(), engine: engine_fn(engine.clone()), log: securitylog.clone(), clock: clock.clone(), rule_pack_hash: engine.pack.hash(), engine_version: engine.code_hash(), advisory: Some(flow::advisory_fn(extensions.clone(), store.clone())) });
        svc.set_intake(intake.clone());
        let sc = store.clone();
        let breaker = Arc::new(CircuitBreaker::new(clock.clone()));
        let model: Arc<dyn cg_queue::explain::Model> = cg_queue::model::model_from_env(&env, cg_queue::model::rulebook_rules(&root)?)?.unwrap_or_else(|| Arc::new(NoModel));
        let explain = Arc::new(ExplainStep::new(store.clone(), model, default_guard(), breaker.clone(), clock.clone(), Arc::new(move || flow::routing_config(&*sc))));
        probes = queue_probes(svc.clone(), breaker, explain.model_name(), env.get("REDIS_URL").cloned().filter(|s| !s.is_empty()), a.demo, clock.clone());
        let rt = Arc::new(Runtime { store: store.clone(), explain, clock: clock.clone(), max_retries: 3 });
        if a.demo {
            let accounts = seed_accounts(&access)?;
            configure_shift(&*store, accounts.as_object().into_iter().flatten().filter(|(_, v)| matches!(v["level"].as_i64(), Some(2 | 3))).map(|(k, _)| k.clone()).collect())?;
            let text = std::fs::read_to_string(root.join("data/development/claims.jsonl")).map_err(|e| e.to_string())?;
            demo_claims = text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
            let accepted = demo_claims.iter().take(60).filter(|c| intake.submit(c).is_ok()).count();
            let path = data_dir.join("demo_accounts.json");
            std::fs::write(&path, serde_json::to_string_pretty(&accounts).unwrap_or_default()).map_err(|e| e.to_string())?;
            println!("DEMO ACCOUNTS (generated now, kept in memory and in {}; never use real data):", path.display());
            for (badge, acc) in accounts.as_object().into_iter().flatten() {
                println!("  {badge}  level {}  {:<22} password: {}", acc["level"], acc["name"].as_str().unwrap_or(""), acc["password"].as_str().unwrap_or(""));
            }
            println!("{accepted} sample claims loaded. Authenticator code: cg xcheck-code (or scripts/demo_code.py) with the secret in the file above.");
        }
        worker = Some(start_workers(rt, dispatcher, stop.clone()));
        queue_service = Some(svc);
    }
    if dev {
        println!("DEV MODE: users live {}; generated secrets are kept in {}. Do not use real data.", if settings.mongo_uri.is_some() { "in the configured database" } else { "in memory (lost on exit)" }, data_dir.join("dev_secrets.json").display());
    }
    let api = Arc::new(Api::new(ApiParts {
        access,
        claims,
        review_log,
        securitylog,
        settings,
        clock,
        queue: queue_service,
        ui: true,
        extra_probes: probes,
        data_dir: data_dir.clone(),
    }));
    let addr: SocketAddr = format!("{}:{}", if a.host == "localhost" { "127.0.0.1" } else { &a.host }, a.port).parse().map_err(|_| "bad --host".to_string())?;
    println!("listening on {}:{}", a.host, a.port);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
    let result = rt.block_on(server::serve(api, addr, a.behind_proxy, async {
        let _ = tokio::signal::ctrl_c().await;
    }));
    stop.store(true, Ordering::SeqCst);
    if let Some(w) = worker {
        let _ = w.join();
    }
    result.map_err(|e| format!("server: {e}"))
}
