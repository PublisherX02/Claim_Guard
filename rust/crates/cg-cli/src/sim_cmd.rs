//! `cg sim ...`: the simulated review database and its experiments (docs/48). Nothing here touches a real queue unless `fast-forward` is aimed at one on purpose.

use cg_queue::store::QueueStore;
use serde_json::json;
use std::path::PathBuf;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn run(args: &[String]) -> i32 {
    let root = match crate::boot::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let seed: u64 = flag(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(1);
    match args.first().map(String::as_str) {
        Some("make-reviews") => {
            let out = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "outputs/sim".into()));
            let target: usize = flag(args, "--decisions").and_then(|s| s.parse().ok()).unwrap_or(5000);
            let corpus = match cg_sim::generate::load_corpus(&root) { Ok(c) => c, Err(e) => { eprintln!("error: {e}"); return 2; } };
            let store = cg_queue::store::MemoryQueueStore::new();
            let summary = match cg_sim::generate::Lab::new(seed, args.iter().any(|a| a == "--adversary")).run(&store, &corpus, target, &cg_queue::routing_config::RoutingConfig::default(), None) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); return 2; }
            };
            if std::fs::create_dir_all(&out).is_err() { return 2; }
            let docs = store.by_state("decided", 1_000_000).unwrap_or_default();
            let lines: Vec<String> = docs.iter().map(|d| d.to_string()).collect();
            if std::fs::write(out.join("decided.jsonl"), lines.join("\n") + "\n").is_err() { return 2; }
            println!("{}", json!({"claims": summary.claims, "decisions": summary.decisions, "fingerprint": summary.fingerprint, "simulated": true, "file": out.join("decided.jsonl").display().to_string()}));
            0
        }
        Some("experiments") => {
            let out = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "outputs/defense/learning.json".into()));
            match cg_sim::experiments::all(&root, seed) {
                Ok(v) => {
                    if let Some(parent) = out.parent() { let _ = std::fs::create_dir_all(parent); }
                    if std::fs::write(&out, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n").is_err() { return 2; }
                    println!("wrote {}", out.display());
                    0
                }
                Err(e) => { eprintln!("error: {e}"); 2 }
            }
        }
        Some("fast-forward") => fast_forward(args, &root, seed),
        Some("workload") => {
            let out = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "outputs/defense/workload.json".into()));
            let seeds: u64 = flag(args, "--seeds").and_then(|s| s.parse().ok()).unwrap_or(20);
            match cg_sim::experiments::workload_study(&root, seed, seeds) {
                Ok(v) => {
                    if let Some(parent) = out.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if std::fs::write(&out, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n").is_err() {
                        return 2;
                    }
                    println!("wrote {}", out.display());
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    2
                }
            }
        }
        _ => {
            println!("usage: cg sim make-reviews [--out DIR] [--seed N] [--decisions N] [--adversary] | experiments [--out FILE] [--seed N] | fast-forward ...");
            2
        }
    }
}

/// Feeds simulated, already decided claims into a demo queue and then proposes a snapshot, so the Learning panel has something to show.
/// Refuses to touch a queue that holds real decided claims unless `--force` is given.
fn fast_forward(args: &[String], root: &std::path::Path, seed: u64) -> i32 {
    use crate::tools::{has, open, secret_prompt};
    use cg_access::service::AccessService;
    let say = |s: &str| println!("{s}");
    let Some(badge) = flag(args, "--badge") else {
        say("usage: cg sim fast-forward --badge CG-4004 [--dev] [--decisions N] [--seed N] [--pace-ms M] [--force]");
        return 2;
    };
    if !has(args, "--dev") && !has(args, "--i-know-this-is-a-demo") {
        say("error: this fills the queue with invented reviews; pass --dev or --i-know-this-is-a-demo");
        return 2;
    }
    let b = match open(args, true) {
        Ok(b) => b,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    let Some(q) = &b.queue else { return 2 };
    let (password, code) = (secret_prompt("Password: "), secret_prompt("Authenticator code: "));
    let operator = match b.access.login(&badge, &password, &code, "queue_admin").and_then(|s| b.access.authenticate(&s.token)) {
        Ok(p) => p,
        Err(_) => {
            say("error: sign-in failed");
            return 2;
        }
    };
    if AccessService::require(&operator, "routing.manage").is_err() {
        say("error: this operator may not do that");
        return 2;
    }
    let target: usize = flag(args, "--decisions").and_then(|s| s.parse().ok()).unwrap_or(1500);
    let pace: u64 = flag(args, "--pace-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let store = &*q.store;
    let existing = store.by_state("decided", 1_000_000).unwrap_or_default();
    let real = existing.iter().filter(|d| !d["claim_id"].as_str().unwrap_or("").starts_with("SIM-")).count();
    if real > 0 && !has(args, "--force") {
        say(&format!("error: the queue holds {real} real decided claim(s); refusing to mix simulated ones in (use --force to override)"));
        return 2;
    }
    if b.access.record("queue_admin", json!({"actor": operator.badge(), "command": "sim_fast_forward"})).is_err() {
        say("error: the security log cannot be written; nothing was done");
        return 2;
    }
    let corpus = match cg_sim::generate::load_corpus(root) {
        Ok(c) => c,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    let cfg = cg_queue::flow::routing_config(store);
    let lab = cg_sim::generate::Lab::new(seed, false);
    let (mut claims, mut done) = (existing.len(), 0usize);
    while done < target {
        match lab.run_from(store, &corpus, claims, 50.min(target - done).max(1), &cfg, None) {
            Ok(s) => {
                claims += s.claims;
                done += s.decisions;
                say(&format!("simulated decisions so far: {done} of {target}"));
            }
            Err(e) => {
                say(&format!("error: {e}"));
                return 2;
            }
        }
        if pace > 0 {
            std::thread::sleep(std::time::Duration::from_millis(pace));
        }
    }
    match cg_queue::reliability::learn(store, &cfg, (b.clock)()) {
        Ok(Some(p)) => say(&format!("proposed reliability v{} from {} decisions; open the Learning panel", p["version"], p["decisions"])),
        Ok(None) => say("no new proposal (nothing changed)"),
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    }
    0
}
