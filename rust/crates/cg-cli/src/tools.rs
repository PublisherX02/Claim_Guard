//! The two operator tools: `cg admin` (reviewer accounts, `scripts/access_admin.py`) and `cg queue` (the work queue,
//! `scripts/queue_admin.py`).
//!
//! `cg admin` talks to the user database directly, so it needs the same access as the server and should be run only by whoever operates
//! the system. A provisioning URI (which carries the authenticator seed) and a generated demo password are printed once and never again;
//! a password you type is never printed or taken from the environment.
//!
//! `cg queue` needs a level 4 operator who signs in with badge, password and authenticator code, and every use is written to the security
//! log. Read-only commands need queue.view; the ones that change the queue (submit, rerun --apply, deadletter-replay) need routing.manage.
//! Nothing here decides a claim: level 4 cannot.

use crate::boot::{boot, Boot, Options};
use cg_access::service::{AccessService, AuthError};
use cg_access::settings::random_token;
use cg_access::store::UserUpdate;
use cg_access::{passwords, totp};
use cg_queue::flow::process;
use cg_queue::replay;
use cg_queue::service::QueueError;
use serde_json::{json, Value};
use std::path::PathBuf;

const DEMO_USERS: [(i64, &str, &str); 4] = [(1, "CG-1001", "Demo Viewer"), (2, "CG-2002", "Demo Reviewer"), (3, "CG-3003", "Demo Senior Reviewer"), (4, "CG-4004", "Demo Administrator")];

pub(crate) fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub(crate) fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

pub(crate) fn secret_prompt(prompt: &str) -> String {
    rpassword::prompt_password(prompt).unwrap_or_default()
}

fn demo_password(badge: &str) -> String {
    loop {
        let candidate = format!("{}aA1!", random_token(14));
        if passwords::check_policy(&candidate, badge).is_empty() {
            return candidate;
        }
    }
}

pub(crate) fn open(args: &[String], queue: bool) -> Result<Boot, String> {
    boot(&Options { dev: has(args, "--dev"), host: "127.0.0.1".into(), queue, demo: false, data_dir: flag(args, "--data-dir").map(PathBuf::from) })
}

pub const ADMIN_USAGE: &str = "usage: cg admin [--dev] [--data-dir D] create-admin --badge CG-0001 --name \"Ada Admin\" | list | unlock --badge B | reset-totp --badge B | seed-demo (dev only)";

pub fn admin(args: &[String]) -> i32 {
    let say = |s: &str| println!("{s}");
    let Some(command) = args.iter().find(|a| ["create-admin", "list", "unlock", "reset-totp", "seed-demo"].contains(&a.as_str())) else {
        say(ADMIN_USAGE);
        return 2;
    };
    if command == "seed-demo" && !has(args, "--dev") {
        say("error: seed-demo is only allowed with --dev");
        return 2;
    }
    let b = match open(args, false) {
        Ok(b) => b,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    let (svc, db, log): (&AccessService, _, _) = (&b.access, b.access.store.clone(), b.securitylog.clone());
    let badge = flag(args, "--badge");
    let result: Result<i32, String> = (|| {
        match command.as_str() {
            "create-admin" => {
                let (Some(badge), Some(name)) = (badge.as_deref(), flag(args, "--name")) else { return Err("create-admin needs --badge and --name".into()) };
                // --password-stdin: one line on standard input, for scripts and installers that have no terminal (never an argument)
                let (first, again) = if has(args, "--password-stdin") {
                    let mut line = String::new();
                    let _ = std::io::stdin().read_line(&mut line);
                    let p = line.trim_end_matches(['\r', '\n']).to_string();
                    (p.clone(), p)
                } else {
                    (secret_prompt("New password: "), secret_prompt("Repeat password: "))
                };
                if first != again {
                    return Err("the passwords do not match".into());
                }
                let (_, uri) = svc.provision_user(badge, &name, &first, 4, "cli", &[], &[], false).map_err(|e| format!("{e}"))?;
                log.record("user_created", json!({"actor": "cli", "badge_id": badge, "level": 4})).map_err(|e| e.to_string())?;
                say(&format!("created {badge} (administrator)\nAdd this to an authenticator app now; it is shown once:\n{uri}"));
            }
            "seed-demo" => {
                if DEMO_USERS.iter().any(|(_, b, _)| db.get_user(b).ok().flatten().is_some()) {
                    return Err("a demo account already exists; nothing was created".into());
                }
                for (level, badge, name) in DEMO_USERS {
                    let password = demo_password(badge);
                    let (_, uri) = svc.provision_user(badge, name, &password, level, "cli", &[], &[], false).map_err(|e| format!("{e}"))?;
                    log.record("user_created", json!({"actor": "cli", "badge_id": badge, "level": level})).map_err(|e| e.to_string())?;
                    say(&format!("{badge} level {level} {name} password: {password}\n{uri}"));
                }
            }
            "list" => {
                let mut users = db.list_users().map_err(|e| e.to_string())?;
                users.sort_by(|a, b| a.badge_id.cmp(&b.badge_id));
                for u in users {
                    say(&format!("{}  level {}  {}{}  {}", u.badge_id, u.level, if u.active { "active" } else { "inactive" }, if u.locked_until.is_some() { " locked" } else { "" }, u.name));
                }
            }
            _ => {
                let badge = badge.as_deref().ok_or("this command needs --badge")?;
                if db.get_user(badge).map_err(|e| e.to_string())?.is_none() {
                    return Err("no such user".into());
                }
                if command == "unlock" {
                    db.update_user(badge, &UserUpdate { failed_attempts: Some(0), locked_until: Some(None), ..Default::default() }).map_err(|e| e.to_string())?;
                    log.record("user_unlocked", json!({"actor": "cli", "badge_id": badge})).map_err(|e| e.to_string())?;
                    say(&format!("unlocked {badge}"));
                } else {
                    let secret = totp::new_secret();
                    let enc = totp::encrypt_secret(&secret, &b.settings.fernet_key)?;
                    db.update_user(badge, &UserUpdate { totp_secret_enc: Some(enc), ..Default::default() }).map_err(|e| e.to_string())?;
                    log.record("totp_reset", json!({"actor": "cli", "badge_id": badge})).map_err(|e| e.to_string())?;
                    say(&format!("Add this to an authenticator app now; it is shown once:\n{}", totp::provisioning_uri(&secret, badge, "ClaimGuard")));
                }
            }
        }
        Ok(0)
    })();
    result.unwrap_or_else(|e| {
        say(&format!("error: {e}"));
        2
    })
}

/// `cg rotate-key [--dry-run]`: re-encrypts every stored claim from the current data key (DATA_KEY, or the key derived from FERNET_KEY) to
/// NEW_DATA_KEY (a Fernet key in the environment, never an argument). Stop the API and the worker first. Afterwards set DATA_KEY to the
/// new key and start them again. Safe to run twice.
pub fn rotate_key(args: &[String]) -> i32 {
    use cg_access::settings::load_settings;
    use cg_mongo::crypt::FieldCrypt;
    let say = |s: &str| println!("{s}");
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let settings = match load_settings(&env, false, "127.0.0.1") {
        Ok(s) => s,
        Err(e) => {
            say(&format!("configuration error: {}", e.0));
            return 2;
        }
    };
    let Some(uri) = settings.mongo_uri.clone() else {
        say("error: MONGO_URI is required");
        return 2;
    };
    let Some(new_key) = env.get("NEW_DATA_KEY").map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) else {
        say("error: set NEW_DATA_KEY to the new Fernet key (python -c \"from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())\")");
        return 2;
    };
    if env.get("DATA_KEY").map(|k| k.trim()) == Some(new_key.as_str()) {
        say("error: NEW_DATA_KEY is the key already in use");
        return 2;
    }
    let (old, new) = match (FieldCrypt::new(&settings.fernet_key, env.get("DATA_KEY").map(String::as_str)), FieldCrypt::new(&settings.fernet_key, Some(&new_key))) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(e), _) | (_, Err(e)) => {
            say(&format!("error: {}", e.0));
            return 2;
        }
    };
    let client = match cg_mongo::connect(&uri, 3000) {
        Ok(c) => c,
        Err(_) => {
            say("error: the database cannot be reached");
            return 2;
        }
    };
    let dry = has(args, "--dry-run");
    let old_users = match FieldCrypt::new(&settings.fernet_key, env.get("DATA_KEY").map(String::as_str)) {
        Ok(o) => o,
        Err(e) => {
            say(&format!("error: {}", e.0));
            return 2;
        }
    };
    let users = cg_mongo::MongoUserStore::new(client.clone(), &settings.mongo_db).with_encryption(old_users);
    let store = cg_mongo::MongoQueueStore::new(client, &crate::boot::queue_db_name(&settings, &env)).with_encryption(old);
    let label = if dry { "dry run" } else { "done" };
    let mut problems = 0;
    match store.reencrypt_all(&new, dry) {
        Ok((rewritten, current, skipped)) => {
            say(&format!("{label}: claims: {rewritten} re-encrypted, {current} already on the new key, {skipped} skipped because they changed meanwhile"));
            problems += skipped;
        }
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    }
    match users.reencrypt_names(&new, dry) {
        Ok((rewritten, current, skipped)) => {
            say(&format!("{label}: user names: {rewritten} re-encrypted, {current} already on the new key, {skipped} skipped because they changed meanwhile"));
            problems += skipped;
        }
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    }
    if !dry && problems == 0 {
        let old_secret = env.get("DATA_KEY").map(|k| k.trim()).filter(|k| !k.is_empty()).unwrap_or(settings.fernet_key.as_str());
        say("Now set DATA_KEY to the new key in the environment and start the API and the worker. Keep the old key until you have checked the console.");
        say("The audit logs are not rewritten (that would break their chain, mirror and stamps): new rows use the new key and the old rows stay readable if you also set");
        say(&format!("DATA_KEY_PREVIOUS={old_secret}   (keep it for as long as the old rows exist; add to the list with commas at the next rotation)"));
    }
    if problems > 0 { 1 } else { 0 }
}

/// `cg rotate-fernet-key [--dry-run]`: re-encrypts every authenticator seed from FERNET_KEY to NEW_FERNET_KEY (a Fernet key in the
/// environment). Refuses to run while DATA_KEY is empty: then the claim data, the names and the audit logs would be using a key derived from
/// FERNET_KEY and would become unreadable. (Set DATA_KEY first with `cg rotate-key`.) Stop the API and the worker first. Afterwards set
/// FERNET_KEY to the new key and start them again. Safe to run twice.
pub fn rotate_fernet_key(args: &[String]) -> i32 {
    use cg_access::settings::load_settings;
    let say = |s: &str| println!("{s}");
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let settings = match load_settings(&env, false, "127.0.0.1") {
        Ok(s) => s,
        Err(e) => {
            say(&format!("configuration error: {}", e.0));
            return 2;
        }
    };
    let Some(uri) = settings.mongo_uri.clone() else {
        say("error: MONGO_URI is required");
        return 2;
    };
    if env.get("DATA_KEY").map(|k| k.trim().is_empty()).unwrap_or(true) {
        say("error: DATA_KEY is empty, so the claim data, the names and the audit logs are protected by a key derived from FERNET_KEY, which this would change.");
        say("First give them their own key: NEW_DATA_KEY=<new Fernet key> cg rotate-key, then set DATA_KEY (and DATA_KEY_PREVIOUS as it tells you).");
        return 2;
    }
    let Some(new_key) = env.get("NEW_FERNET_KEY").map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) else {
        say("error: set NEW_FERNET_KEY to the new Fernet key (python -c \"from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())\")");
        return 2;
    };
    if new_key == settings.fernet_key {
        say("error: NEW_FERNET_KEY is the key already in use");
        return 2;
    }
    let client = match cg_mongo::connect(&uri, 3000) {
        Ok(c) => c,
        Err(_) => {
            say("error: the database cannot be reached");
            return 2;
        }
    };
    let dry = has(args, "--dry-run");
    let users = cg_mongo::MongoUserStore::new(client, &settings.mongo_db);
    match users.reencrypt_seeds(&settings.fernet_key, &new_key, dry) {
        Ok((rewritten, current, skipped)) => {
            say(&format!("{}: authenticator seeds: {rewritten} re-encrypted, {current} already on the new key, {skipped} skipped because they changed meanwhile", if dry { "dry run" } else { "done" }));
            if !dry && skipped == 0 {
                say("Now set FERNET_KEY to the new key in the environment and start the API and the worker. Nobody has to re-enrol an authenticator. Keep the old key until you have checked a sign-in, then destroy it.");
            }
            if skipped > 0 { 1 } else { 0 }
        }
        Err(e) => {
            say(&format!("error: {e}"));
            2
        }
    }
}

pub const QUEUE_USAGE: &str = "usage: cg queue --badge CG-4004 [--dev] submit --claims FILE [--process] | reconcile | replay ID [--version N] | rerun --rule-pack HASH [--apply] | deadletters | deadletter-replay ID | verify-deal ID | workload [--skip-lower X] | audit | privacy-export PATIENT_ID | privacy-erase PATIENT_ID [--confirm N] | privacy-retention --days N [--apply]";

pub fn queue(args: &[String]) -> i32 {
    let say = |s: &str| println!("{s}");
    let Some(badge) = flag(args, "--badge") else {
        say(QUEUE_USAGE);
        return 2;
    };
    let Some(ci) = args.iter().position(|a| ["submit", "reconcile", "replay", "rerun", "deadletters", "deadletter-replay", "verify-deal", "workload", "audit", "batches", "privacy-export", "privacy-erase", "privacy-retention"].contains(&a.as_str())) else {
        say(QUEUE_USAGE);
        return 2;
    };
    let command = args[ci].as_str();
    let rest = &args[ci + 1..];
    let b = match open(args, true) {
        Ok(b) => b,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    let Some(q) = &b.queue else { return 2 };

    // sign in as the operator
    let (password, code) = (secret_prompt("Password: "), secret_prompt("Authenticator code: "));
    let operator = match b.access.login(&badge, &password, &code, "queue_admin").and_then(|s| b.access.authenticate(&s.token)) {
        Ok(p) => p,
        Err(_) => {
            say("error: sign-in failed");
            return 2;
        }
    };
    let apply = has(rest, "--apply");
    let mutating = matches!(command, "submit" | "deadletter-replay" | "privacy-erase") || (command == "privacy-retention" && apply) || (command == "rerun" && apply);
    if AccessService::require(&operator, if mutating { "routing.manage" } else { "queue.view" }).is_err() {
        say("error: this operator may not do that");
        return 2;
    }
    if b.access.record("queue_admin", json!({"actor": operator.badge(), "command": command})).is_err() {
        say("error: the security log cannot be written; nothing was done");
        return 2;
    }
    let (store, now) = (&*q.store, b.clock.clone());
    let positional = rest.iter().find(|a| !a.starts_with("--") && flag(rest, "--version").as_deref() != Some(a.as_str()) && flag(rest, "--claims").as_deref() != Some(a.as_str()) && flag(rest, "--rule-pack").as_deref() != Some(a.as_str()) && flag(rest, "--skip-lower").as_deref() != Some(a.as_str()) && flag(rest, "--confirm").as_deref() != Some(a.as_str()) && flag(rest, "--days").as_deref() != Some(a.as_str()));
    let out: Result<i32, QueueError> = (|| {
        match command {
            "replay" => {
                let id = positional.ok_or_else(|| QueueError::Invalid("replay needs a claim id".into()))?;
                let version = flag(rest, "--version").and_then(|v| v.parse().ok());
                let r = replay::replay_claim(store, &q.engine_fn, id, version)?;
                say(&sorted(&r));
                Ok(if r["same"] == true && r["claim_intact"] == true && r["results_intact"] == true { 0 } else { 1 })
            }
            "rerun" => {
                let pack = flag(rest, "--rule-pack").ok_or_else(|| QueueError::Invalid("rerun needs --rule-pack".into()))?;
                say(&sorted(&replay::rerun(store, &q.engine_fn, &pack, &q.intake, !apply)?));
                Ok(0)
            }
            "deadletters" => {
                for d in store.dead_letters(1000).map_err(|e| QueueError::Invalid(e.to_string()))? {
                    say(&format!("{}  {} v{}  {}  attempts={}", d["dead_id"].as_str().unwrap_or(""), d["claim_id"].as_str().unwrap_or(""), d["version"], d["reason"].as_str().unwrap_or(""), d["attempts"]));
                }
                Ok(0)
            }
            "deadletter-replay" => {
                let id = positional.ok_or_else(|| QueueError::Invalid("deadletter-replay needs a dead letter id".into()))?;
                let m = replay::replay_dead_letter(store, id, now(), operator.badge())?;
                say(&format!("{} v{} is back in {}", m["claim_id"].as_str().unwrap_or(""), m["version"], m["state"].as_str().unwrap_or("")));
                Ok(0)
            }
            "reconcile" => {
                let report = cg_queue::flow::reconcile(store, now(), operator.badge());
                for f in &report.findings {
                    say(&sorted(f));
                }
                say(&format!("ok={} findings={}", if report.ok() { "True" } else { "False" }, report.findings.len()));
                Ok(if report.ok() { 0 } else { 1 })
            }
            "workload" => {
                // read-only: which rules cost reviewers time for nothing, which are almost always upheld, and the measured time per finding
                let cfg = cg_queue::flow::routing_config(store);
                let skip = flag(rest, "--skip-lower").and_then(|v| v.parse::<f64>().ok()).filter(|v| (0.5..1.0).contains(v)).unwrap_or(cg_queue::workload::DEFAULT_SKIP_LOWER);
                let decided = store.by_state("decided", 1_000_000).map_err(|e| QueueError::Invalid(e.to_string()))?;
                say(&sorted(&cg_queue::workload::report(&decided, &cg_queue::reliability::Params::from_config(&cfg), skip, cfg.lease_seconds as f64)));
                Ok(0)
            }
            "audit" => {
                // read-only: how auto-clear is doing, from the random sample of green claims a person still looked at
                let mut docs = vec![];
                for state in ["decided", "ready", "leased", "awaiting_countersign"] {
                    docs.extend(store.by_state(state, 1_000_000).map_err(|e| QueueError::Invalid(e.to_string()))?);
                }
                let cfg = cg_queue::flow::routing_config(store);
                let mut report = cg_queue::workload::audit_report(&docs);
                report["auto_clear_green"] = serde_json::json!(cfg.auto_clear_green);
                report["audit_percent"] = serde_json::json!(cfg.audit_percent);
                say(&sorted(&report));
                Ok(0)
            }
            "batches" => {
                // read-only: every batch with its audit sample, flags and status (the administrator's view, without claim ids)
                let events = store.batch_events(None, 100_000).map_err(|e| QueueError::Invalid(e.to_string()))?;
                let mut ids: Vec<String> = vec![];
                for e in &events {
                    if let Some(b) = e["batch_id"].as_str().filter(|b| !ids.iter().any(|i| i == b)) {
                        ids.push(b.to_string());
                    }
                }
                for id in &ids {
                    let mine: Vec<Value> = events.iter().filter(|e| e["batch_id"] == id.as_str()).cloned().collect();
                    if let Some(v) = cg_queue::batch::fold(&mine) {
                        say(&sorted(&v.to_json(None)));
                    }
                }
                Ok(0)
            }
            "privacy-export" => {
                // subject access: everything held about one patient, printed to the operator's terminal and nowhere else
                let patient = positional.ok_or_else(|| QueueError::Invalid("privacy-export needs a patient identifier".into()))?;
                let e = cg_queue::privacy::export_subject(store, patient, now()).map_err(|e| QueueError::Invalid(e.to_string()))?;
                let _ = b.access.record("privacy_action", json!({"actor": operator.badge(), "action": "export", "claims": e["totals"]["claims"], "versions": e["totals"]["versions"]}));
                say(&serde_json::to_string_pretty(&e).unwrap_or_default());
                Ok(0)
            }
            "privacy-erase" => {
                // erasure: without --confirm it only counts; with --confirm N it deletes, and only if N is the number counted
                let patient = positional.ok_or_else(|| QueueError::Invalid("privacy-erase needs a patient identifier".into()))?;
                let ids = cg_queue::privacy::claim_ids_of(store, patient).map_err(|e| QueueError::Invalid(e.to_string()))?;
                let preview = cg_queue::privacy::erase_preview(store, &ids).map_err(|e| QueueError::Invalid(e.to_string()))?;
                match flag(rest, "--confirm").and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) if n == ids.len() && n > 0 => {
                        let r = cg_queue::privacy::erase_claims(store, &ids).map_err(|e| QueueError::Invalid(e.to_string()))?;
                        let _ = b.access.record("privacy_action", json!({"actor": operator.badge(), "action": "erase", "claims": r["claims_erased"], "versions": r["versions_erased"]}));
                        say(&sorted(&r));
                        Ok(0)
                    }
                    _ => {
                        say(&format!("{} claim(s) would be erased, {} version(s) at most. Nothing was deleted. Run again with --confirm {} to delete them for good.", ids.len(), preview["versions_at_most"], ids.len()));
                        Ok(1)
                    }
                }
            }
            "privacy-retention" => {
                let days = flag(rest, "--days").and_then(|v| v.parse::<u32>().ok()).filter(|d| *d > 0).ok_or_else(|| QueueError::Invalid("privacy-retention needs --days N (N > 0)".into()))?;
                let r = cg_queue::privacy::run_retention(store, now(), days, 10_000, !apply).map_err(|e| QueueError::Invalid(e.to_string()))?;
                if apply {
                    let _ = b.access.record("privacy_action", json!({"actor": operator.badge(), "action": "retention", "claims": r["claims"], "versions": r["versions"]}));
                }
                say(&sorted(&r));
                Ok(0)
            }
            "verify-deal" => {
                let id = positional.ok_or_else(|| QueueError::Invalid("verify-deal needs a deal id".into()))?;
                let same = q.dispatcher.verify(id).map_err(QueueError::Invalid)?;
                say(&format!("deal {id}: {}", if same { "matches the algorithm" } else { "DOES NOT match the algorithm (the stored seed or inputs were changed)" }));
                Ok(if same { 0 } else { 1 })
            }
            _ => {
                let path = flag(rest, "--claims").ok_or_else(|| QueueError::Invalid("submit needs --claims".into()))?;
                let text = std::fs::read_to_string(&path).map_err(|e| QueueError::Invalid(e.to_string()))?;
                let (mut submitted, mut refused, mut ready) = (0, 0, 0);
                let mut claims: Vec<Value> = vec![];
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    match serde_json::from_str::<Value>(line) {
                        Ok(claim) => claims.push(claim),
                        Err(_) => refused += 1,
                    }
                }
                // a file is submitted as batches of at most MAX_BATCH claims; each batch draws its own audit sample (docs/53)
                for chunk in claims.chunks(cg_queue::batch::MAX_BATCH) {
                    let Ok((batch_id, rows)) = q.intake.submit_batch(chunk, operator.badge()) else {
                        refused += chunk.len();
                        continue;
                    };
                    if let Some(id) = &batch_id {
                        say(&format!("batch {id}: {} claims", chunk.len()));
                    }
                    for (claim, row) in chunk.iter().zip(rows.iter()) {
                        if row["accepted"] != true {
                            refused += 1;
                            continue;
                        }
                        submitted += 1;
                        if has(rest, "--process") {
                            if let Some(doc) = store.get(claim["claim_id"].as_str().unwrap_or(""), None).map_err(|e| QueueError::Invalid(e.to_string()))? {
                                let (id, v) = (doc["claim_id"].as_str().unwrap_or(""), doc["version"].as_i64().unwrap_or(0));
                                if process(&q.runtime, id, v, doc["input_hash"].as_str().unwrap_or("")) == "ready" {
                                    let _ = store.clear_outbox(id, v);
                                    ready += 1;
                                }
                            }
                        }
                    }
                }
                say(&format!("submitted={submitted} refused={refused} ready={ready}"));
                Ok(0)
            }
        }
    })();
    match out {
        Ok(code) => code,
        Err(QueueError::NotFound(_)) => {
            say("error: not found");
            2
        }
        Err(QueueError::Conflict(m)) => {
            say(&format!("error: conflict ({m})"));
            2
        }
        Err(QueueError::Auth(AuthError::Unavailable)) => {
            say("error: the database cannot be reached");
            2
        }
        Err(e) => {
            say(&format!("error: {}", e.to_string().chars().take(200).collect::<String>()));
            2
        }
    }
}

/// JSON with keys sorted, like `json.dumps(sort_keys=True)`.
fn sorted(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                Value::Object(keys.into_iter().map(|k| (k.clone(), sort(&m[k]))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    sort(v).to_string()
}
