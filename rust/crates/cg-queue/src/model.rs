//! The production model for the queue's AI step (`src/workqueue/model_adapter.py`): a real language model behind the `Model` contract.
//!
//! ExplainStep hands a model a request (rule id, failure shape, prompt version, the placeholders) and expects a template with `{value}`
//! and `{line}` where claim values go. This module turns that request into a prompt, calls a model, and returns the template.
//!
//! * No claim value ever leaves the process: the prompt is built from the rulebook's own text and the failure shape only.
//! * Every call has a deadline: the time left on the claim's ceiling is the call's timeout; a call that starts with none left is refused.
//! * Errors map onto the breaker's classes: timeouts, rate limits, connection failures and 5xx are transient (they count and are retried),
//!   rejected requests and missing credentials are fatal (they never count).
//! * Two-pass mode (off by default): pass 1 asks for plain text, pass 2 only puts that exact text into a JSON object, and is accepted only
//!   if its text equals pass 1 apart from white space.
//!
//! The transport is plain HTTP to an OpenAI-compatible endpoint. Production is vLLM (`http://host:8000/v1`, `QUEUE_AI_PROVIDER=vllm`); Ollama
//! (`http://host:11434/v1`, `QUEUE_AI_PROVIDER=ollama`) speaks the same protocol and is kept for laptops, where vLLM does not run natively.

use crate::breaker::ModelError;
use crate::explain::{Model, MAX_TEMPLATE_CHARS, PLACEHOLDERS};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PROMPT_VERSION: &str = "2.1.0-template";
const PASS1_INSTRUCTIONS: &str = "You help a human reviewer of a synthetic, educational health-insurance claims exercise. A deterministic rule engine has flagged a claim. Write a short explanation (two or three sentences) of why the rule below flagged it, in plain English, as a TEMPLATE.\nRules for the template:\n- Where the claim's own value belongs, write {value}. Where the affected line id belongs, write {line}. Use no other braces.\n- You are not shown any claim data. Never invent a value, a date, an amount, an identifier or a name.\n- Do not say a claim is approved, accepted, paid or payable; do not judge medical necessity; do not suggest fraud.\n- End with one sentence telling the reviewer what to check or request, based on the rule's corrective action.\nReply with the template text only.";
const EXAMPLE: &str = "This claim was flagged because {value} is higher than the limit the rulebook allows for line {line}. A person should compare the billed amount with the price list before anything is paid or rejected. Please check the price list and ask the provider to confirm the amount.";
const PASS2_INSTRUCTIONS: &str = "Put the text below, word for word, into one JSON object of the form {\"template\": \"<the text>\"}. Do not change, add or remove any word. Reply with the JSON object only.";

/// complete(prompt, timeout seconds) -> text. Injected so everything above runs offline in tests.
pub type Complete = Arc<dyn Fn(&str, f64) -> Result<String, ModelError> + Send + Sync>;

pub struct TemplateModel {
    complete: Complete,
    rules: BTreeMap<String, Value>,
    two_pass: bool,
    name: String,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub calls: AtomicU64,
}

fn collapse_ws(s: &str) -> String {
    s.split(|c: char| cg_core::text::py_isspace(c)).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

fn wall() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl TemplateModel {
    pub fn new(complete: Complete, rules: BTreeMap<String, Value>, name: &str, two_pass: bool) -> TemplateModel {
        TemplateModel { complete, rules, two_pass, name: if two_pass { format!("{name}+2pass") } else { name.into() }, clock: Arc::new(wall), calls: AtomicU64::new(0) }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> TemplateModel {
        self.clock = clock;
        self
    }

    pub fn prompt_version(&self) -> String {
        format!("{PROMPT_VERSION}{}", if self.two_pass { "-2pass" } else { "" })
    }

    pub fn pass1_prompt(&self, request: &Value) -> Result<String, ModelError> {
        let rid = request["rule_id"].as_str().unwrap_or("");
        let rule = self.rules.get(rid).ok_or_else(|| ModelError::Fatal(format!("unknown rule '{rid}'")))?;
        let shape = request["failure_shape"].as_str().unwrap_or("");
        let mut parts = shape.split('/');
        let (status, severity, lines) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let s = |k: &str| rule[k].as_str().unwrap_or("").to_string();
        let facts = [("Rule", format!("{}: {}", s("rule_id"), s("title"))), ("What it checks", s("logic")), ("What the rulebook says to do", s("corrective_action"))];
        let rule_text = facts.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
        let shape_word = match lines {
            "0" => "none",
            "1" => "one",
            _ => "several",
        };
        let placeholders: Vec<String> = match request["placeholders"].as_array().filter(|a| !a.is_empty()) {
            Some(a) => a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect(),
            None => PLACEHOLDERS.iter().map(|p| p.to_string()).collect(),
        };
        Ok(format!(
            "{PASS1_INSTRUCTIONS}\n\n## Example of the kind of answer wanted (a different rule)\n{EXAMPLE}\n\n## The rule to explain\n{rule_text}\n\nOutcome: status {status}, severity {severity}, affected lines: {shape_word}\nAllowed placeholders: {}\nNow write the explanation of this rule's outcome in two or three sentences, as plain sentences, not JSON.\n",
            placeholders.join(", ")
        ))
    }

    pub fn pass2_prompt(text: &str) -> String {
        format!("{PASS2_INSTRUCTIONS}\n\n## Text\n{text}\n")
    }

    fn call(&self, prompt: &str, deadline: f64) -> Result<String, ModelError> {
        let left = deadline - (self.clock)();
        if left <= 0.0 {
            return Err(ModelError::Timeout("no time left before the claim deadline".into()));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let out = (self.complete)(prompt, left)?;
        if out.trim().is_empty() {
            return Err(ModelError::Transient("the model returned no text".into()));
        }
        Ok(out.trim().to_string())
    }

    fn check_template(text: &str) -> Result<String, ModelError> {
        if text.chars().count() > MAX_TEMPLATE_CHARS {
            return Err(ModelError::Transient("the template is too long".into()));
        }
        let rest = text.replace("{value}", "").replace("{line}", "");
        if rest.contains('{') || rest.contains('}') {
            return Err(ModelError::Transient("the template uses braces other than {value} and {line}".into()));
        }
        Ok(text.to_string())
    }
}

impl Model for TemplateModel {
    fn ask(&self, request: &Value, deadline: f64) -> Result<String, ModelError> {
        let first = Self::check_template(&self.call(&self.pass1_prompt(request)?, deadline)?)?;
        if !self.two_pass {
            return Ok(first);
        }
        let raw = self.call(&Self::pass2_prompt(&first), deadline)?;
        let (a, b) = (raw.find('{'), raw.rfind('}'));
        let obj: Value = match (a, b) {
            (Some(a), Some(b)) if b > a => serde_json::from_str(&raw[a..=b]).map_err(|_| ModelError::Transient("pass 2 did not return JSON".into()))?,
            _ => return Err(ModelError::Transient("pass 2 did not return JSON".into())),
        };
        let second = match obj.as_object() {
            Some(m) if m.len() == 1 => m.get("template").and_then(Value::as_str),
            _ => None,
        }
        .ok_or_else(|| ModelError::Transient("pass 2 JSON has the wrong shape".into()))?;
        if collapse_ws(second) != collapse_ws(&first) {
            return Err(ModelError::Transient("pass 2 changed the text".into()));
        }
        Self::check_template(&first)
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

/// An OpenAI-compatible chat endpoint over plain HTTP (vLLM or Ollama on the private network).
pub fn openai_http(base_url: &str, model: &str, api_key: &str, max_tokens: u32) -> Complete {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let (model, key) = (model.to_string(), api_key.to_string());
    Arc::new(move |prompt: &str, timeout: f64| {
        let body = json!({"model": model, "messages": [{"role": "user", "content": prompt}], "temperature": 0, "top_p": 1, "max_tokens": max_tokens, "stream": false});
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs_f64(timeout.clamp(0.1, 600.0))).build();
        match agent.post(&url).set("authorization", &format!("Bearer {key}")).send_json(body) {
            Ok(resp) => {
                let v: Value = resp.into_json().map_err(|_| ModelError::Transient("the model answered with something that is not JSON".into()))?;
                v["choices"][0]["message"]["content"].as_str().map(|t| strip_fences(t.trim())).ok_or_else(|| ModelError::Transient("Provider returned no message content".into()))
            }
            Err(ureq::Error::Status(code, _)) if code == 429 || code >= 500 => Err(ModelError::Transient(format!("HTTP {code}"))),
            Err(ureq::Error::Status(code, _)) => Err(ModelError::Fatal(format!("HTTP {code}"))),
            Err(ureq::Error::Transport(t)) => {
                let msg = t.to_string();
                if msg.to_lowercase().contains("timed out") || msg.to_lowercase().contains("timeout") { Err(ModelError::Timeout(msg)) } else { Err(ModelError::Transient(msg)) }
            }
        }
    })
}

fn strip_fences(text: &str) -> String {
    if !text.starts_with("```") {
        return text.to_string();
    }
    let t = text.trim_matches('`');
    t.strip_prefix("json").unwrap_or(t).to_string()
}

/// The model the queue should use, chosen by environment variables; None keeps the default (no model: every claim keeps the engine's own
/// explanation). QUEUE_AI_PROVIDER turns it on:
/// * `vllm` (production): VLLM_BASE_URL (default http://localhost:8000), VLLM_MODEL (the served model name; default google/gemma-3-4b-it) and
///   VLLM_API_KEY (the key the server was started with; the server refuses a wrong one);
/// * `ollama` (laptops): OLLAMA_BASE_URL (default http://localhost:11434), OLLAMA_MODEL (default gemma3:4b), OLLAMA_API_KEY.
/// QUEUE_AI_TWO_PASS=1 enables two-pass mode. Both servers speak the same OpenAI-compatible protocol, so the rest of the system cannot tell them apart.
pub fn model_from_env(env: &std::collections::HashMap<String, String>, rules: BTreeMap<String, Value>) -> Result<Option<Arc<dyn Model>>, String> {
    let which = env.get("QUEUE_AI_PROVIDER").map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if which.is_empty() {
        return Ok(None);
    }
    let (prefix, port, default_model, default_key) = match which.as_str() {
        "vllm" => ("VLLM", 8000, "google/gemma-3-4b-it", "vllm-local"),
        "ollama" => ("OLLAMA", 11434, "gemma3:4b", "ollama-local"),
        _ => return Err(format!("QUEUE_AI_PROVIDER must be vllm (or ollama on a laptop): the Rust build serves an on-premises model only, not '{which}'")),
    };
    let get = |name: &str| env.get(&format!("{prefix}_{name}")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let base = get("BASE_URL").map(|s| s.trim_end_matches('/').to_string()).unwrap_or_else(|| format!("http://localhost:{port}"));
    if !base.starts_with("http://") {
        return Err(format!("{prefix}_BASE_URL must start with http:// (the model runs on the private network)"));
    }
    let base = if base.ends_with("/v1") { base } else { format!("{base}/v1") };
    let model = get("MODEL").unwrap_or_else(|| default_model.into());
    let key = env.get(&format!("{prefix}_API_KEY")).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| default_key.into());
    let two = env.get("QUEUE_AI_TWO_PASS").map(String::as_str) == Some("1");
    Ok(Some(Arc::new(TemplateModel::new(openai_http(&base, &model, &key, 500), rules, &model, two))))
}

/// {rule_id: rule} from `rules/rules.json`.
pub fn rulebook_rules(root: &std::path::Path) -> Result<BTreeMap<String, Value>, String> {
    let text = std::fs::read_to_string(root.join("rules/rules.json")).map_err(|e| e.to_string())?;
    let rows: Vec<Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().filter_map(|r| r["rule_id"].as_str().map(|id| (id.to_string(), r.clone()))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn rules() -> BTreeMap<String, Value> {
        rulebook_rules(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).unwrap()
    }

    fn scripted(answers: Vec<Result<String, ModelError>>) -> (TemplateModel, Arc<Mutex<Vec<(String, f64)>>>) {
        let seen = Arc::new(Mutex::new(vec![]));
        let (s2, queue) = (seen.clone(), Mutex::new(answers.into_iter().rev().collect::<Vec<_>>()));
        let complete: Complete = Arc::new(move |p, t| {
            s2.lock().unwrap().push((p.to_string(), t));
            queue.lock().unwrap().pop().unwrap_or(Err(ModelError::Fatal("script exhausted".into())))
        });
        (TemplateModel::new(complete, rules(), "m", false).with_clock(Arc::new(|| 100.0)), seen)
    }

    fn req() -> Value {
        json!({"rule_id": "R001", "failure_shape": "FAIL/high/1", "prompt_version": PROMPT_VERSION, "placeholders": ["{value}", "{line}"]})
    }

    #[test]
    fn the_prompt_holds_the_rulebook_text_and_no_claim_data() {
        let (m, _) = scripted(vec![]);
        let p = m.pass1_prompt(&req()).unwrap();
        assert!(p.contains("Rule: R001:") && p.contains("affected lines: one") && p.contains("Allowed placeholders: {value}, {line}"));
        assert!(m.pass1_prompt(&json!({"rule_id": "R999", "failure_shape": "FAIL/high/1"})).is_err());
    }

    #[test]
    fn the_time_left_is_the_timeout_and_none_left_is_refused_without_calling() {
        let (m, seen) = scripted(vec![Ok("Flagged: {value} on {line}. Please check.".into())]);
        assert!(m.ask(&req(), 130.0).is_ok());
        assert_eq!(seen.lock().unwrap()[0].1, 30.0);
        assert!(matches!(m.ask(&req(), 99.0), Err(ModelError::Timeout(_))));
        assert_eq!(seen.lock().unwrap().len(), 1, "no call was made after the deadline");
    }

    #[test]
    fn templates_with_other_braces_or_empty_text_are_rejected() {
        let (m, _) = scripted(vec![Ok("see {other}".into()), Ok("   ".into()), Ok("x".repeat(MAX_TEMPLATE_CHARS + 1))]);
        for _ in 0..3 {
            assert!(matches!(m.ask(&req(), 200.0), Err(ModelError::Transient(_))));
        }
    }

    #[test]
    fn two_pass_accepts_only_the_same_words() {
        let seen = Arc::new(Mutex::new(vec!["Check {value}  now.".to_string(), r#"x {"template": "Check {value} now."} y"#.to_string()]));
        let q = seen.clone();
        let complete: Complete = Arc::new(move |_, _| Ok(q.lock().unwrap().remove(0)));
        let m = TemplateModel::new(complete, rules(), "m", true).with_clock(Arc::new(|| 0.0));
        assert_eq!(m.ask(&req(), 50.0).unwrap(), "Check {value}  now.");
        assert_eq!(m.name(), "m+2pass");
        let answers = Arc::new(Mutex::new(vec!["Check {value} now.".to_string(), r#"{"template": "Check {value} later."}"#.to_string()]));
        let q = answers.clone();
        let complete: Complete = Arc::new(move |_, _| Ok(q.lock().unwrap().remove(0)));
        let m = TemplateModel::new(complete, rules(), "m", true).with_clock(Arc::new(|| 0.0));
        assert!(matches!(m.ask(&req(), 50.0), Err(ModelError::Transient(_))), "a changed word is refused");
    }

    #[test]
    fn environment_selects_vllm_or_ollama_only_over_plain_http() {
        let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::HashMap<_, _>>();
        assert!(model_from_env(&env(&[]), rules()).unwrap().is_none());
        assert!(model_from_env(&env(&[("QUEUE_AI_PROVIDER", "nvidia")]), rules()).is_err());
        assert!(model_from_env(&env(&[("QUEUE_AI_PROVIDER", "ollama"), ("OLLAMA_BASE_URL", "https://example.com")]), rules()).is_err());
        assert!(model_from_env(&env(&[("QUEUE_AI_PROVIDER", "vllm"), ("VLLM_BASE_URL", "https://example.com")]), rules()).is_err());
        let m = model_from_env(&env(&[("QUEUE_AI_PROVIDER", "ollama"), ("OLLAMA_BASE_URL", "http://ollama:11434"), ("OLLAMA_MODEL", "gemma3:4b")]), rules()).unwrap().unwrap();
        assert_eq!(m.name(), "gemma3:4b");
        let v = model_from_env(&env(&[("QUEUE_AI_PROVIDER", "vllm"), ("VLLM_BASE_URL", "http://vllm:8000"), ("VLLM_MODEL", "google/gemma-3-4b-it")]), rules()).unwrap().unwrap();
        assert_eq!(v.name(), "google/gemma-3-4b-it");
        let defaults = model_from_env(&env(&[("QUEUE_AI_PROVIDER", " VLLM ")]), rules()).unwrap().unwrap();
        assert_eq!(defaults.name(), "google/gemma-3-4b-it", "the provider name is case- and space-insensitive and the model has a default");
        let other = model_from_env(&env(&[("QUEUE_AI_PROVIDER", "vllm"), ("OLLAMA_MODEL", "x")]), rules()).unwrap().unwrap();
        assert_eq!(other.name(), "google/gemma-3-4b-it", "the other provider's variables are not read");
    }

    #[test]
    fn the_client_sends_the_servers_api_key_as_a_bearer_token_and_the_served_model_name() {
        let (url, seen) = fake_server(200, r#"{"choices":[{"message":{"content":"Flagged: {value} on {line}. Please check."}}]}"#);
        let m = TemplateModel::new(openai_http(&url, "google/gemma-3-4b-it", "secret-key-123", 100), rules(), "google/gemma-3-4b-it", false).with_clock(Arc::new(|| 0.0));
        m.ask(&req(), 30.0).unwrap();
        let sent = seen.lock().unwrap().to_lowercase();
        assert!(sent.contains("authorization: bearer secret-key-123"), "vLLM started with --api-key refuses a request without it");
        assert!(sent.contains("\"model\":\"google/gemma-3-4b-it\""));
    }

    #[test]
    fn a_wrong_api_key_or_model_name_is_a_fatal_error_that_never_opens_the_breaker() {
        // vLLM answers 401 for a wrong key and 404 for an unknown model name: both are configuration mistakes, not an outage
        for status in [401u16, 404] {
            let (url, _) = fake_server(status, "{}");
            let m = TemplateModel::new(openai_http(&url, "m", "k", 10), rules(), "m", false).with_clock(Arc::new(|| 0.0));
            assert!(matches!(m.ask(&req(), 30.0).unwrap_err(), ModelError::Fatal(_)), "HTTP {status}");
        }
        // 503 while the server loads the model is transient: retried, counted
        let (url, _) = fake_server(503, "{}");
        let m = TemplateModel::new(openai_http(&url, "m", "k", 10), rules(), "m", false).with_clock(Arc::new(|| 0.0));
        assert!(matches!(m.ask(&req(), 30.0).unwrap_err(), ModelError::Transient(_)));
    }

    /// A throwaway server that answers every request with `status` and `body`, and remembers the first request it saw.
    fn fake_server(status: u16, body: &'static str) -> (String, Arc<Mutex<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(String::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                // read the whole request: headers first, then as many body bytes as Content-Length announces
                let mut got: Vec<u8> = vec![];
                let mut chunk = [0u8; 4096];
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    got.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&got).to_string();
                    let Some(split) = text.find("\r\n\r\n") else {
                        if n == 0 { break } else { continue }
                    };
                    let want = text[..split].to_lowercase().split("content-length:").nth(1).and_then(|r| r.split_whitespace().next().and_then(|v| v.parse::<usize>().ok())).unwrap_or(0);
                    if got.len() >= split + 4 + want || n == 0 {
                        break;
                    }
                }
                s2.lock().unwrap().push_str(&String::from_utf8_lossy(&got));
                let _ = write!(stream, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            }
        });
        (url, seen)
    }

    #[test]
    fn the_http_client_sends_the_prompt_and_reads_the_message() {
        let (url, seen) = fake_server(200, r#"{"choices":[{"message":{"content":"  Flagged: {value} on {line}. Please check.  "}}]}"#);
        let m = TemplateModel::new(openai_http(&url, "gemma3:4b", "k", 100), rules(), "gemma3:4b", false).with_clock(Arc::new(|| 0.0));
        assert_eq!(m.ask(&req(), 30.0).unwrap(), "Flagged: {value} on {line}. Please check.");
        let sent = seen.lock().unwrap().clone();
        assert!(sent.starts_with("POST /v1/chat/completions") && sent.contains("gemma3:4b") && sent.contains("Rule: R001"));
        assert!(!sent.contains("PAT-"), "no claim data is in the request");
    }

    #[test]
    fn http_errors_map_onto_the_breaker_classes() {
        for (status, transient) in [(500u16, true), (429, true), (400, false), (401, false)] {
            let (url, _) = fake_server(status, "{}");
            let m = TemplateModel::new(openai_http(&url, "m", "k", 10), rules(), "m", false).with_clock(Arc::new(|| 0.0));
            let e = m.ask(&req(), 30.0).unwrap_err();
            assert_eq!(matches!(e, ModelError::Transient(_)), transient, "HTTP {status} gave {e:?}");
            assert_eq!(matches!(e, ModelError::Fatal(_)), !transient);
        }
        let m = TemplateModel::new(openai_http("http://127.0.0.1:1/v1", "m", "k", 10), rules(), "m", false).with_clock(Arc::new(|| 0.0));
        assert!(m.ask(&req(), 30.0).unwrap_err().counts(), "a refused connection counts toward opening the breaker");
        let (url, _) = fake_server(200, r#"{"choices":[]}"#);
        let m = TemplateModel::new(openai_http(&url, "m", "k", 10), rules(), "m", false).with_clock(Arc::new(|| 0.0));
        assert!(matches!(m.ask(&req(), 30.0), Err(ModelError::Transient(_))));
    }
}
