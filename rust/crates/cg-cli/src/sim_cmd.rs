//! `cg sim ...`: the simulated review database and its experiments (docs/47). Nothing here touches a real queue unless `fast-forward` is aimed at one on purpose.

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
        _ => {
            println!("usage: cg sim make-reviews [--out DIR] [--seed N] [--decisions N] [--adversary] | experiments [--out FILE] [--seed N] | fast-forward ...");
            2
        }
    }
}
