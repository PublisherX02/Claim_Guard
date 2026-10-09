//! `cg`: the ClaimGuard command line (Rust build).
//!
//!     cg evaluate <claims.jsonl> [--root DIR]     one JSON line per claim: {"claim_id", "results": [15 results]}
//!
//!     cg audit-verify <log> [--strict]            checks the hash chain and its anchor (exit 0 when intact)
//!     cg audit-append <log> <events.jsonl> [--review]   appends system events (or review decisions) to a log
//!
//! The first sub-command exists so the Rust engine can be compared, line by line, with the Python engine's output.

use cg_engine::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("evaluate") => evaluate(&args),
        Some("audit-verify") => audit_verify(&args),
        Some("audit-append") => audit_append(&args),
        _ => {
            eprintln!("usage: cg evaluate <claims.jsonl> [--root DIR] | cg audit-verify <log> [--strict] | cg audit-append <log> <events.jsonl> [--review]");
            std::process::exit(2);
        }
    }
}

fn evaluate(args: &[String]) {
    let Some(input) = args.get(2) else {
        eprintln!("usage: cg evaluate <claims.jsonl> [--root DIR]");
        std::process::exit(2);
    };
    let root = PathBuf::from(arg(args, "--root").unwrap_or_else(|| ".".into()));
    let engine = match Engine::load(&root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let file = match std::fs::File::open(input) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: cannot open {input}: {e}");
            std::process::exit(2);
        }
    };
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let mut problems = 0;
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { problems += 1; continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(claim) = serde_json::from_str::<Value>(&line) else {
            eprintln!("line {}: not JSON", n + 1);
            problems += 1;
            continue;
        };
        let mut errors = vec![];
        match engine.review(&claim, &mut errors) {
            Ok(results) => {
                let record = json!({"claim_id": claim.get("claim_id"), "results": results, "tool_errors": errors});
                let _ = writeln!(out, "{record}");
            }
            Err(e) => {
                eprintln!("line {}: {e}", n + 1);
                std::process::exit(3);
            }
        }
    }
    if problems > 0 {
        std::process::exit(2);
    }
}

fn audit_verify(args: &[String]) {
    let Some(log) = args.get(2) else { std::process::exit(2) };
    let strict = args.iter().any(|a| a == "--strict");
    match cg_audit::verify_with_anchor(std::path::Path::new(log), None, strict) {
        Ok((head, count)) => println!("Chain valid: {count} events; head {head}."),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn audit_append(args: &[String]) {
    let (Some(log), Some(events)) = (args.get(2), args.get(3)) else { std::process::exit(2) };
    let review = args.iter().any(|a| a == "--review");
    let text = std::fs::read_to_string(events).unwrap_or_default();
    let list: Vec<Value> = text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    let result = cg_audit::AuditLog::open(log).and_then(|a| if review { a.append_review_decisions(&list) } else { a.append_system_events(&list) });
    match result {
        Ok((head, count)) => println!("{count} events; head {head}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
