//! `cg ledger ...`: the signed ledger over the audit logs (see cg-audit/src/ledger.rs).
//!
//!   cg ledger keygen                                  a fresh Ed25519 key: the SEED (secret: the worker only) and the PUBLIC key (give it to anyone)
//!   cg ledger pubkey                                  the public key of the seed in LEDGER_SIGNING_KEY
//!   cg ledger checkpoint <log> [--label L]            sign the log as it is now (needs LEDGER_SIGNING_KEY)
//!   cg ledger verify <log> [--label L] [--public K]   check the log against every checkpoint; exit 0 when all verify
//!   cg ledger prove <log> <sequence> [--label L] [--public K] [--output file]   a proof that one event was in the log, checkable without the log
//!   cg ledger verify-proof <proof.json> [--public K]  check a proof from its own contents and the public key
//! The public key is `--public`, or LEDGER_PUBLIC_KEY. Nothing here needs a secret except `checkpoint`, with one exception that is a property of the logs and not
//! of the ledger: the logs are encrypted on disk, so reading a whole log (`verify`, `prove`) needs the same DATA_KEY / FERNET_KEY the server uses. A proof, once made,
//! carries no secret and no other event: `verify-proof` needs only the public key.

use cg_audit::ledger;
use serde_json::Value;
use std::path::Path;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn label_of(args: &[String], log: &str) -> String {
    arg(args, "--label").unwrap_or_else(|| if Path::new(log).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("review")) { "review".into() } else { "security".into() })
}

fn public(args: &[String]) -> Result<ledger::VerifyingKey, String> {
    let text = arg(args, "--public").or_else(|| std::env::var("LEDGER_PUBLIC_KEY").ok()).filter(|s| !s.trim().is_empty()).ok_or("give the public key with --public or LEDGER_PUBLIC_KEY")?;
    ledger::public_key_from(&text)
}

/// Lets this process read encrypted audit logs, with the same variables the server uses (DATA_KEY or FERNET_KEY, DATA_KEY_PREVIOUS, CG_ENCRYPT_AT_REST).
fn unlock_logs() {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    if let Some(key) = env.get("DATA_KEY").or_else(|| env.get("FERNET_KEY")).filter(|k| !k.trim().is_empty()) {
        let _ = crate::boot::set_audit_encryption(&env, key);
    }
}

fn positional(args: &[String]) -> Vec<&String> {
    let mut v = vec![];
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            i += 2;
        } else {
            v.push(&args[i]);
            i += 1;
        }
    }
    v
}

pub fn run(args: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("error: {m}");
        2
    };
    let pos = positional(args.get(1..).unwrap_or(&[]));
    if matches!(args.first().map(String::as_str), Some("checkpoint" | "verify" | "prove")) {
        unlock_logs();
    }
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let (seed, public) = ledger::keygen();
            println!("LEDGER_SIGNING_KEY={seed}   (secret: put it in the worker's environment only; anyone holding it can sign checkpoints)");
            println!("LEDGER_PUBLIC_KEY={public}   (public: for the API, for auditors, for the offline copy)");
            0
        }
        Some("pubkey") => match std::env::var("LEDGER_SIGNING_KEY").map_err(|_| "LEDGER_SIGNING_KEY is not set".to_string()).and_then(|s| ledger::public_of_seed(&s)) {
            Ok(p) => {
                println!("{p}");
                0
            }
            Err(e) => fail(&e),
        },
        Some("checkpoint") => {
            let Some(log) = pos.first() else { return fail("usage: cg ledger checkpoint <log> [--label L]") };
            let key = match std::env::var("LEDGER_SIGNING_KEY").map_err(|_| "LEDGER_SIGNING_KEY is not set".to_string()).and_then(|s| ledger::signing_key_from(&s)) {
                Ok(k) => k,
                Err(e) => return fail(&e),
            };
            match ledger::sign_checkpoint(Path::new(log.as_str()), &label_of(args, log), &key) {
                Ok(c) => {
                    println!("{}", serde_json::to_string_pretty(&c.to_json()).unwrap_or_default());
                    0
                }
                Err(e) => fail(&e.to_string()),
            }
        }
        Some("verify") => {
            let Some(log) = pos.first() else { return fail("usage: cg ledger verify <log> [--label L] [--public K]") };
            let pk = match public(args) {
                Ok(k) => k,
                Err(e) => return fail(&e),
            };
            let r = ledger::verify_ledger(Path::new(log.as_str()), &label_of(args, log), &pk, 3);
            println!("{}", serde_json::to_string_pretty(&r.to_json()).unwrap_or_default());
            if r.ok { 0 } else { 1 }
        }
        Some("prove") => {
            let (Some(log), Some(seq)) = (pos.first(), pos.get(1).and_then(|s| s.parse::<u64>().ok())) else { return fail("usage: cg ledger prove <log> <sequence> [--label L] [--public K] [--output file]") };
            let pk = match public(args) {
                Ok(k) => k,
                Err(e) => return fail(&e),
            };
            let label = label_of(args, log);
            let path = Path::new(log.as_str());
            let Some(cp) = ledger::latest_valid_checkpoint(path, &label, &pk).filter(|c| c.count >= seq) else { return fail("no signed checkpoint covers that event yet") };
            match ledger::prove(path, seq, &cp) {
                Ok(p) => {
                    let text = serde_json::to_string_pretty(&p).unwrap_or_default();
                    match arg(args, "--output") {
                        Some(o) => {
                            if let Err(e) = std::fs::write(&o, text) {
                                return fail(&e.to_string());
                            }
                            println!("proof for event {seq} written to {o} (it holds no other event)");
                        }
                        None => println!("{text}"),
                    }
                    0
                }
                Err(e) => fail(&e),
            }
        }
        Some("verify-proof") => {
            let Some(file) = pos.first() else { return fail("usage: cg ledger verify-proof <proof.json> [--public K]") };
            let pk = match public(args) {
                Ok(k) => k,
                Err(e) => return fail(&e),
            };
            let proof: Value = match std::fs::read_to_string(file.as_str()).ok().and_then(|t| serde_json::from_str(&t).ok()) {
                Some(v) => v,
                None => return fail("the proof file cannot be read"),
            };
            match ledger::verify_proof(&proof, &pk) {
                Ok(()) => {
                    println!("VALID: event {} (hash {}) was in the {} log when it held {} events, as signed at {}", proof["sequence"], proof["row_hash"].as_str().unwrap_or(""), proof["checkpoint"]["log"].as_str().unwrap_or(""), proof["tree_size"], proof["checkpoint"]["time"].as_str().unwrap_or(""));
                    0
                }
                Err(e) => {
                    println!("INVALID: {e}");
                    1
                }
            }
        }
        _ => fail("usage: cg ledger keygen | pubkey | checkpoint <log> | verify <log> | prove <log> <sequence> | verify-proof <proof.json>"),
    }
}
