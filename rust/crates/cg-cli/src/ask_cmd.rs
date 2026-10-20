//! `cg ask`: put questions to "Ask the rulebook" from the command line, without a server (docs/60). Used to measure the three tiers.
//!
//!   cg ask "which rules can fire for SVC-IMAGE under EDU-BASIC"      one question, readable output
//!   cg ask --file questions.jsonl [--generate] [--repeat 2]           one JSON line per question: {"id":..,"q":..} or a bare string
//!   --vector-only                                                     passage search alone, as before the graph and the cache (the baseline)
//!
//! The embedding service (EMBEDDER_URL, EMBEDDER_TOKEN) and the model (ASK_LLM_PROVIDER ...) are the ones the server would use, from the environment.
//! `--repeat N` asks the whole list N times in a row, so the second round shows what the answer cache saves. Nothing here touches a claim or a database.

use cg_engine::payer::PayerRules;
use cg_engine::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub fn run(args: &[String]) -> i32 {
    let say = |s: &str| println!("{s}");
    let env: HashMap<String, String> = std::env::vars().collect();
    let root = match crate::boot::repo_root() {
        Ok(r) => r,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    let dir = std::env::temp_dir().join(format!("cg-ask-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let setup = (|| -> Result<PayerRules, String> {
        let engine = Arc::new(Engine::load(&root).map_err(|e| e.to_string())?);
        let payer = PayerRules::open(&root, &dir, engine)?;
        if let Some((embedder, threshold)) = crate::embedder::HttpEmbedder::from_env(&env)? {
            payer.set_embedder(Arc::new(embedder), threshold);
        }
        if let Some((llm, prefix, timeout)) = crate::ask_llm::from_env(&env)? {
            payer.set_llm(llm, prefix, timeout);
        }
        Ok(payer)
    })();
    let payer = match setup {
        Ok(p) => p,
        Err(e) => {
            say(&format!("error: {e}"));
            return 2;
        }
    };
    if args.iter().any(|a| a == "--context") {
        // the rulebook as the front of a prompt, and its estimated size in tokens
        let text = payer.rulebook_context();
        say(&text);
        eprintln!("{} characters, about {} tokens", text.chars().count(), cg_engine::cag::estimate_tokens(&text));
        return 0;
    }
    let generate = args.iter().any(|a| a == "--generate");
    let vector_only = args.iter().any(|a| a == "--vector-only");
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let repeat = flag("--repeat").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 20);
    let questions: Vec<(String, String)> = match flag("--file") {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .enumerate()
                .filter_map(|(i, l)| match serde_json::from_str::<Value>(l) {
                    Ok(Value::Object(o)) => Some((o.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("Q{i}")), o.get("q")?.as_str()?.to_string())),
                    Ok(Value::String(s)) => Some((format!("Q{i}"), s)),
                    _ => None,
                })
                .collect(),
            Err(e) => {
                say(&format!("error: {path}: {e}"));
                return 2;
            }
        },
        None => match args.iter().find(|a| !a.starts_with("--") && flag("--repeat").as_deref() != Some(a.as_str())) {
            Some(q) => vec![("Q0".into(), q.clone())],
            None => {
                say("usage: cg ask \"question\" [--generate]   |   cg ask --file questions.jsonl [--generate] [--repeat N]");
                return 2;
            }
        },
    };
    let single = flag("--file").is_none();
    for round in 1..=repeat {
        for (id, q) in &questions {
            let t = std::time::Instant::now();
            let mut a = if vector_only {
                let mut v = payer.ask(q, 4);
                v["tier"] = json!("vector");
                v
            } else {
                payer.ask_routed(q, 4, generate)
            };
            let wall = (t.elapsed().as_secs_f64() * 1000.0).round() as u64;
            if single {
                say(&format!("[{}] {}{}", a["tier"].as_str().unwrap_or(""), a["understood_as"].as_str().unwrap_or(a["note"].as_str().unwrap_or("")), if a["cached"] == true { " (from the cache)" } else { "" }));
                if let Some(ans) = a["answer"].as_str() {
                    say(ans);
                }
                for r in a["results"].as_array().into_iter().flatten() {
                    say(&format!("- {} [{}]", r["text"].as_str().unwrap_or(""), r["source"].as_str().unwrap_or("")));
                }
                if let Some(w) = a["written"].as_str() {
                    say(&format!("\nwritten by {}: {w}", a["written_by"].as_str().unwrap_or("")));
                }
                if let Some(w) = a["written_refused"].as_str() {
                    say(&format!("\nthe written answer was refused: {w}"));
                }
                say(&format!("{wall} ms"));
            } else {
                a["id"] = json!(id);
                a["round"] = json!(round);
                a["wall_ms"] = json!(wall);
                say(&a.to_string());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    0
}
