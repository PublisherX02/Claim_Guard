//! `cg serve` and `cg worker` (`scripts/serve_access.py`, `workqueue/demo.py`).
//!
//! * `cg serve` runs the reviewer API (and, with `--queue`, the queue routes). `--demo` also seeds sample accounts and claims and runs the
//!   queue workers in the same process; `--dev` generates secrets and keeps users in memory unless MONGO_URI is set.
//! * `cg worker` runs only the queue workers (relay, deal, expire, reconcile) against the database: the production counterpart of the
//!   Celery worker and its scheduler. Run exactly one.

use crate::boot::{boot, shutdown_signal, start_workers, Boot, Options};
use cg_access::passwords;
use cg_access::service::AccessService;
use cg_access::settings::random_token;
use cg_api::claimstore::FileClaimStore;
use cg_api::ops::queue_probes;
use cg_api::server;
use cg_api::{Api, ApiParts};
use cg_queue::routing_config::{self, RoutingConfig};
use cg_queue::store::QueueStore;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const LOCAL: [&str; 3] = ["127.0.0.1", "localhost", "::1"];
const DEMO_ACCOUNTS: [(i64, &str, &str); 6] = [
    (1, "CG-1001", "Demo Viewer"),
    (2, "CG-2002", "Demo Reviewer Amal"),
    (2, "CG-2003", "Demo Reviewer Omar"),
    (3, "CG-3003", "Demo Senior Layla"),
    (3, "CG-3004", "Demo Senior Karim"),
    (4, "CG-4004", "Demo Administrator"),
];

pub const USAGE: &str = "usage: cg serve [--host H] [--port N] [--dev] [--demo] [--queue] [--behind-proxy] [--data-dir D] [--claims FILE]\n       cg worker [--dev] [--data-dir D]";

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

/// Seeds accounts, puts the reviewers on shift and loads sample claims through the real intake.
fn seed_demo(b: &Boot) -> Result<(), String> {
    let q = b.queue.as_ref().ok_or("the demo needs the queue")?;
    let accounts = seed_accounts(&b.access)?;
    configure_shift(&*q.store, accounts.as_object().into_iter().flatten().filter(|(_, v)| matches!(v["level"].as_i64(), Some(2 | 3))).map(|(k, _)| k.clone()).collect())?;
    let text = std::fs::read_to_string(b.root.join("data/development/claims.jsonl")).map_err(|e| e.to_string())?;
    let claims: Vec<Value> = text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    let accepted = claims.iter().take(60).filter(|c| q.intake.submit(c).is_ok()).count();
    let path = b.data_dir.join("demo_accounts.json");
    std::fs::write(&path, serde_json::to_string_pretty(&accounts).unwrap_or_default()).map_err(|e| e.to_string())?;
    println!("DEMO ACCOUNTS (generated now, kept in memory and in {}; never use real data):", path.display());
    for (badge, acc) in accounts.as_object().into_iter().flatten() {
        println!("  {badge}  level {}  {:<22} password: {}", acc["level"], acc["name"].as_str().unwrap_or(""), acc["password"].as_str().unwrap_or(""));
    }
    println!("{accepted} sample claims loaded. The authenticator seeds are in the file above.");
    Ok(())
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
    let dev = a.dev || a.demo;
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
    match start(a, dev) {
        Ok(()) => 0,
        Err(e) => {
            say(&format!("error: {e}"));
            2
        }
    }
}

fn start(a: Args, dev: bool) -> Result<(), String> {
    let b = boot(&Options { dev, host: a.host.clone(), queue: a.queue || a.demo, demo: a.demo, data_dir: a.data_dir.clone() })?;
    let claims_path = a.claims.clone().or_else(|| b.env.get("CLAIMS_PATH").map(PathBuf::from)).unwrap_or_else(|| b.root.join("data/development/claims.jsonl"));
    let claims = Arc::new(FileClaimStore::from_jsonl(&claims_path, &b.engine)?);
    let stop = Arc::new(AtomicBool::new(false));
    let (mut queue_service, mut probes, mut worker) = (None, vec![], None);
    if let Some(q) = &b.queue {
        probes = queue_probes(q.service.clone(), q.breaker.clone(), q.runtime.explain.model_name(), b.env.get("REDIS_URL").cloned().filter(|s| !s.is_empty()), a.demo, b.clock.clone());
        if a.demo {
            seed_demo(&b)?;
        }
        // In a real deployment the worker is its own process (`cg worker`); only the demo runs it here.
        if a.demo {
            worker = Some(start_workers(q, stop.clone()));
        }
        queue_service = Some(q.service.clone());
    }
    if dev {
        println!("DEV MODE: users live {}; generated secrets are kept in {}. Do not use real data.", if b.settings.mongo_uri.is_some() { "in the configured database" } else { "in memory (lost on exit)" }, b.data_dir.join("dev_secrets.json").display());
    }
    let api = Arc::new(Api::new(ApiParts {
        access: b.access.clone(),
        claims,
        review_log: b.review_log.clone(),
        securitylog: b.securitylog.clone(),
        settings: b.settings.clone(),
        clock: b.clock.clone(),
        queue: queue_service,
        ui: true,
        extra_probes: probes,
        data_dir: b.data_dir.clone(),
    }));
    let addr: SocketAddr = format!("{}:{}", if a.host == "localhost" { "127.0.0.1" } else { &a.host }, a.port).parse().map_err(|_| "bad --host".to_string())?;
    println!("listening on {}:{}", a.host, a.port);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
    let result = rt.block_on(server::serve(api, addr, a.behind_proxy, shutdown_signal()));
    stop.store(true, Ordering::SeqCst);
    if let Some(w) = worker {
        let _ = w.join();
    }
    result.map_err(|e| format!("server: {e}"))
}

/// `cg worker`: the queue workers alone.
pub fn worker(args: &[String]) -> i32 {
    let data_dir = args.iter().position(|a| a == "--data-dir").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let b = match boot(&Options { dev: args.iter().any(|a| a == "--dev"), host: "127.0.0.1".into(), queue: true, demo: false, data_dir }) {
        Ok(b) => b,
        Err(e) => {
            println!("error: {e}");
            return 2;
        }
    };
    let Some(q) = &b.queue else { return 2 };
    let stop = Arc::new(AtomicBool::new(false));
    let handle = start_workers(q, stop.clone());
    println!("worker running (relay, deal, expire, reconcile)");
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(shutdown_signal()),
        Err(_) => return 2,
    }
    stop.store(true, Ordering::SeqCst);
    let _ = handle.join();
    0
}

/// `cg health --port N` asks the local server for /healthz; `cg health --file F --max-age S` checks that the worker touched F recently.
/// Exit 0 when healthy. For container health checks (the image has no curl).
pub fn health(args: &[String]) -> i32 {
    use std::io::{Read, Write};
    let flag = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    if let Some(file) = flag("--file") {
        let max: u64 = flag("--max-age").and_then(|v| v.parse().ok()).unwrap_or(60);
        let age = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
        return if age.is_some_and(|a| a <= max) { 0 } else { 1 };
    }
    let port: u16 = flag("--port").and_then(|v| v.parse().ok()).unwrap_or(8443);
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_secs(3)) else { return 1 };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let _ = s.write_all(b"GET /healthz HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n");
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    if out.starts_with("HTTP/1.1 200") { 0 } else { 1 }
}
