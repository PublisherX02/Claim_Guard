//! The language model behind "Ask the rulebook" (docs/60): it proposes which graph query a question means, and writes the facts up as a short answer.
//! The same OpenAI-compatible endpoint the explanation step uses (vLLM on the server, Ollama on a laptop), chosen by ASK_LLM_PROVIDER (`vllm` or
//! `ollama`; unset: the model is off and the rulebook answers with graph facts and quoted passages only).
//!
//! * ASK_LLM_PROVIDER=ollama: OLLAMA_BASE_URL (default http://localhost:11434), OLLAMA_MODEL (default gemma3:4b), OLLAMA_API_KEY.
//! * ASK_LLM_PROVIDER=vllm: VLLM_BASE_URL, VLLM_MODEL (default google/gemma-3-4b-it), VLLM_API_KEY.
//! * ASK_LLM_PREFIX=1 puts the whole rulebook first in every prompt (the CAG prefix); ASK_LLM_TIMEOUT is the seconds one call may take (default 120);
//!   ASK_LLM_MAX_TOKENS bounds a reply (default 400).
//! It sees the rulebook, the policies and the question; never a claim.

use cg_engine::payer::Completer;
use cg_queue::model::openai_http;
use std::collections::HashMap;
use std::sync::Arc;

struct Chat {
    complete: cg_queue::model::Complete,
    model: String,
}

impl Completer for Chat {
    fn complete(&self, prompt: &str, timeout_secs: f64) -> Result<String, String> {
        (self.complete)(prompt, timeout_secs).map_err(|e| format!("{e:?}"))
    }
    fn model(&self) -> String {
        self.model.clone()
    }
}

/// (model, put the rulebook first, timeout in seconds), or None when ASK_LLM_PROVIDER is unset.
pub fn from_env(env: &HashMap<String, String>) -> Result<Option<(Arc<dyn Completer>, bool, f64)>, String> {
    let which = env.get("ASK_LLM_PROVIDER").map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if which.is_empty() {
        return Ok(None);
    }
    let (prefix, port, default_model, default_key) = match which.as_str() {
        "vllm" => ("VLLM", 8000, "google/gemma-3-4b-it", "vllm-local"),
        "ollama" => ("OLLAMA", 11434, "gemma3:4b", "ollama-local"),
        _ => return Err(format!("ASK_LLM_PROVIDER must be vllm or ollama, not '{which}'")),
    };
    let get = |name: &str| env.get(&format!("{prefix}_{name}")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let base = get("BASE_URL").map(|s| s.trim_end_matches('/').to_string()).unwrap_or_else(|| format!("http://localhost:{port}"));
    if !base.starts_with("http://") {
        return Err(format!("{prefix}_BASE_URL must start with http:// (the model runs on the private network)"));
    }
    let base = if base.ends_with("/v1") { base } else { format!("{base}/v1") };
    let model = get("MODEL").unwrap_or_else(|| default_model.into());
    let key = get("API_KEY").unwrap_or_else(|| default_key.into());
    let max_tokens = env.get("ASK_LLM_MAX_TOKENS").and_then(|s| s.trim().parse::<u32>().ok()).filter(|n| (50..=2000).contains(n)).unwrap_or(400);
    let timeout = env.get("ASK_LLM_TIMEOUT").and_then(|s| s.trim().parse::<f64>().ok()).filter(|t| (1.0..=600.0).contains(t)).unwrap_or(120.0);
    let prefix_on = env.get("ASK_LLM_PREFIX").map(|s| s.trim() == "1").unwrap_or(false);
    Ok(Some((Arc::new(Chat { complete: openai_http(&base, &model, &key, max_tokens), model }), prefix_on, timeout)))
}
