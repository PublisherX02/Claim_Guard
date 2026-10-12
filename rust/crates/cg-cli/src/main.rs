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

mod boot;
mod ingest_cmd;
mod selfcheck;
mod serve;
mod tools;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("evaluate") => evaluate(&args),
        Some("audit-verify") => audit_verify(&args),
        Some("audit-append") => audit_append(&args),
        Some("xcheck") => xcheck(),
        Some("ingest") => std::process::exit(ingest_cmd::ingest(&args[2..])),
        Some("ingest-doc") => std::process::exit(ingest_cmd::ingest_doc(&args[2..])),
        Some("extensions") => extensions(&args),
        Some("model-check") => std::process::exit(model_check()),
        Some("timestamp") => std::process::exit(timestamp(&args[2..])),
        Some("selfcheck") => std::process::exit(selfcheck::run()),
        Some("health") => std::process::exit(serve::health(&args[2..])),
        Some("worker") => std::process::exit(serve::worker(&args[2..])),
        Some("admin") => std::process::exit(tools::admin(&args[2..])),
        Some("rotate-key") => std::process::exit(tools::rotate_key(&args[2..])),
        Some("rotate-fernet-key") => std::process::exit(tools::rotate_fernet_key(&args[2..])),
        Some("queue") => std::process::exit(tools::queue(&args[2..])),
        Some("serve") =>std::process::exit(serve::run(&args[2..])),
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

/// The standalone log commands open an encrypted log when the deployment's key is in the environment (DATA_KEY or FERNET_KEY).
fn audit_key_from_env() {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let secret = env.get("DATA_KEY").or_else(|| env.get("FERNET_KEY")).map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if let Some(s) = secret {
        if let Err(e) = crate::boot::set_audit_encryption(&env, &s) {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

fn audit_verify(args: &[String]) {
    audit_key_from_env();
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
    audit_key_from_env();
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

/// `cg xcheck`: reads one JSON request per line on stdin and answers one JSON line each. It exposes the primitives of the access layer
/// (password hashes, tokens, one-time codes, encrypted seeds, pseudonyms) so tools/compare_access.py can check, value by value, that the
/// Python and Rust builds read and write each other's data.
fn xcheck() {
    use cg_access::{masking, passwords, settings::Settings, tokens, totp};
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let s = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let out = match s("op").as_str() {
            "verify_password" => json!(passwords::verify_password(&s("password"), &s("hash"))),
            "hash_password" => json!(passwords::hash_password(&s("password"), req["rounds"].as_u64().unwrap_or(4) as u32).ok()),
            "issue_token" => {
                let mut env = std::collections::HashMap::new();
                env.insert("JWT_SECRET".to_string(), s("secret"));
                env.insert("BCRYPT_ROUNDS".to_string(), "4".to_string());
                let st: Settings = cg_access::settings::load_settings(&env, true, "127.0.0.1").expect("settings");
                let t = tokens::issue(&st, &s("badge"), req["now"].as_f64().unwrap_or(0.0));
                json!({"token": t.token, "jti": t.jti, "csrf": t.csrf, "expires_at": t.expires_at})
            }
            "decode_token" => {
                let mut env = std::collections::HashMap::new();
                env.insert("JWT_SECRET".to_string(), s("secret"));
                let st = cg_access::settings::load_settings(&env, true, "127.0.0.1").expect("settings");
                tokens::decode(&st, &s("token"), req["now"].as_f64().unwrap_or(0.0)).map(|p| json!({"sub": p["sub"], "jti": p["jti"], "csrf": p["csrf"]})).unwrap_or(Value::Null)
            }
            "fernet_encrypt" => json!(totp::encrypt_secret(&s("plain"), &s("key")).ok()),
            "fernet_decrypt" => json!(totp::decrypt_secret(&s("token"), &s("key")).ok()),
            "totp_code" => json!(totp::code_at(&s("secret"), req["step"].as_i64().unwrap_or(0))),
            "pass1_prompt" => {
                let rules = cg_queue::model::rulebook_rules(std::path::Path::new(&s("root"))).unwrap_or_default();
                let m = cg_queue::model::TemplateModel::new(std::sync::Arc::new(|_, _| Ok(String::new())), rules, "x", false);
                json!(m.pass1_prompt(&req["request"]).ok())
            }
            "pseudonym" => json!(masking::pseudonym(&s("key"), &s("prefix"), &s("value")).ok()),
            "provisioning_uri" => json!(totp::provisioning_uri(&s("secret"), &s("badge"), "ClaimGuard")),
            _ => Value::Null,
        };
        println!("{out}");
    }
}

/// `cg extensions <claims.jsonl> [--root DIR]`: the advisory (extension) results of every claim, with the earlier claims taken from the
/// same file (what `claim_history.InMemoryHistory` does). One JSON line per claim.
fn extensions(args: &[String]) {
    use cg_engine::extensions::{Extensions, InMemoryHistory};
    let Some(input) = args.get(2) else {
        eprintln!("usage: cg extensions <claims.jsonl> [--root DIR]");
        std::process::exit(2);
    };
    let root = PathBuf::from(arg(args, "--root").unwrap_or_else(|| ".".into()));
    let ext = Extensions::load(&root).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2)
    });
    let text = std::fs::read_to_string(input).unwrap_or_else(|e| {
        eprintln!("error: cannot read {input}: {e}");
        std::process::exit(2)
    });
    let claims: Vec<Value> = text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    let history = InMemoryHistory::new(&claims);
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for claim in &claims {
        let mut errors = vec![];
        match ext.evaluate(claim, Some(&history), &mut errors) {
            Ok(results) => {
                let _ = writeln!(out, "{}", json!({"claim_id": claim.get("claim_id"), "advisory": results, "tool_errors": errors}));
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(3);
            }
        }
    }
}

/// `cg timestamp stamp --log L --tsa-url URL [--timeout S]` / `cg timestamp verify --log L [--tsa-ca PEM]`: stamp the audit log's anchor with
/// an RFC 3161 Time-Stamp Authority, or check an existing stamp. `verify` exits 1 only for "invalid"; `stamp` exits 2 when the authority
/// cannot be reached (the log and the previous token are untouched).
fn timestamp(args: &[String]) -> i32 {
    let a = |n: &str| args.iter().position(|x| x == n).and_then(|i| args.get(i + 1)).cloned();
    let (Some(cmd), Some(log)) = (args.first().cloned(), a("--log")) else {
        println!("usage: cg timestamp stamp --log L --tsa-url URL [--timeout S] | cg timestamp verify --log L [--tsa-ca PEM]");
        return 2;
    };
    let log = PathBuf::from(log);
    let anchor = cg_audit::anchor_path_of(&log);
    if cmd == "stamp" {
        let Some(url) = a("--tsa-url") else {
            println!("error: stamp needs --tsa-url");
            return 2;
        };
        let timeout: f64 = a("--timeout").and_then(|v| v.parse().ok()).unwrap_or(10.0);
        return match cg_timestamp::stamp_log(&log, &anchor, &url, timeout) {
            Ok(side) => {
                println!("Stamped head {}... at {} events; token saved to {}.", side["head"].as_str().unwrap_or("").chars().take(16).collect::<String>(), side["count"], cg_timestamp::sidecar_path(&log).display());
                println!("Run `verify` with the TSA CA certificate to check it; stamping does not verify the token.");
                0
            }
            Err(e) => {
                println!("NOT timestamped: {e}. The log is unaffected; the previous token (if any) is kept.");
                2
            }
        };
    }
    let ca = a("--tsa-ca").and_then(|p| std::fs::read_to_string(p).ok());
    let st = cg_timestamp::timestamp_status(&log, &anchor, ca.as_deref());
    println!("Timestamp: {st}");
    if st["state"] == "invalid" { 1 } else { 0 }
}

/// `cg model-check`: asks the configured model (QUEUE_AI_PROVIDER=ollama, OLLAMA_BASE_URL, OLLAMA_MODEL) for the explanation template of
/// each rule and reports how many came back in the required shape and how long each took. Exit 0 only if all were accepted. No claim data
/// is involved: the model sees the rulebook's own text.
fn model_check() -> i32 {
    let root = serve_root();
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let rules = match cg_queue::model::rulebook_rules(&root) {
        Ok(r) => r,
        Err(e) => {
            println!("error: {e}");
            return 2;
        }
    };
    let model = match cg_queue::model::model_from_env(&env, rules.clone()) {
        Ok(Some(m)) => m,
        Ok(None) => {
            println!("QUEUE_AI_PROVIDER is not set: the queue would use no model. Set it to ollama.");
            return 2;
        }
        Err(e) => {
            println!("error: {e}");
            return 2;
        }
    };
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let mut accepted = 0;
    for (id, rule) in &rules {
        let shape = format!("FAIL/{}/1", rule["severity"].as_str().unwrap_or("high"));
        let start = std::time::Instant::now();
        match model.ask(&json!({"rule_id": id, "failure_shape": shape}), now() + 90.0) {
            Ok(text) => {
                accepted += 1;
                println!("{id} ok {:5.1}s  {:?}", start.elapsed().as_secs_f64(), text.chars().take(110).collect::<String>());
            }
            Err(e) => println!("{id} REJECTED {:5.1}s  {}", start.elapsed().as_secs_f64(), format!("{e:?}").chars().take(100).collect::<String>()),
        }
    }
    println!("accepted {accepted} of {} with {}", rules.len(), model.name());
    if accepted == rules.len() { 0 } else { 1 }
}

fn serve_root() -> PathBuf {
    std::env::var("CLAIMGUARD_ROOT").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}
