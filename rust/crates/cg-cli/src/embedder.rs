//! The client of the embedding sidecar (`deploy/embed/service.py`), which turns text into vectors for the meaning-based "Ask the rulebook" (docs/52).
//!
//! It only ever sends the text of the rulebook passages and the reviewer's question; no claim is read here. The sidecar is on the private network, takes a bearer
//! token, and has no route out. If it is not configured, or does not answer, the server keeps answering with the keyword search.

use cg_engine::rag::Embedder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub struct HttpEmbedder {
    url: String,
    token: String,
    timeout: Duration,
}

impl HttpEmbedder {
    /// EMBEDDER_URL (http://host:port), EMBEDDER_TOKEN, EMBEDDER_TIMEOUT (seconds, 1 to 60, default 8), EMBEDDER_THRESHOLD (0.05 to 0.95, default 0.415: the value
    /// chosen on the held-out half in docs/52 for paraphrase-multilingual-MiniLM-L12-v2; another model needs its own threshold).
    /// Ok(None) when no URL is set: meaning-based search is then off.
    pub fn from_env(env: &HashMap<String, String>) -> Result<Option<(HttpEmbedder, f64)>, String> {
        let Some(url) = env.get("EMBEDDER_URL").map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()) else { return Ok(None) };
        if !url.starts_with("http://") {
            return Err("EMBEDDER_URL must start with http:// (the embedding service runs on the private network)".into());
        }
        let token = env.get("EMBEDDER_TOKEN").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("EMBEDDER_TOKEN is required with EMBEDDER_URL")?;
        let timeout = env.get("EMBEDDER_TIMEOUT").and_then(|s| s.trim().parse::<u64>().ok()).filter(|n| (1..=60).contains(n)).unwrap_or(8);
        let threshold = match env.get("EMBEDDER_THRESHOLD").map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => 0.415,
            Some(s) => s.parse::<f64>().ok().filter(|t| (0.05..=0.95).contains(t)).ok_or("EMBEDDER_THRESHOLD must be a number between 0.05 and 0.95")?,
        };
        Ok(Some((HttpEmbedder { url, token, timeout: Duration::from_secs(timeout) }, threshold)))
    }
}

impl Embedder for HttpEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let resp = agent.post(&format!("{}/embed", self.url)).set("authorization", &format!("Bearer {}", self.token)).send_json(json!({"texts": texts}));
        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|_| "the embedding service answered with something that is not JSON".to_string())?,
            Err(ureq::Error::Status(401, _)) => return Err("the embedding service refused our token".into()),
            Err(ureq::Error::Status(code, _)) => return Err(format!("the embedding service failed (HTTP {code})")),
            Err(ureq::Error::Transport(t)) => return Err(format!("the embedding service could not be reached: {t}")),
        };
        let rows = v["vectors"].as_array().ok_or("the embedding service reply has no vectors")?;
        rows.iter()
            .map(|row| row.as_array().ok_or_else(|| "a vector is not a list".to_string()).and_then(|a| a.iter().map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| "a vector holds a non-number".to_string())).collect()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn off_unless_a_url_is_set_and_strict_when_it_is() {
        assert!(HttpEmbedder::from_env(&env(&[])).unwrap().is_none());
        assert!(HttpEmbedder::from_env(&env(&[("EMBEDDER_URL", "https://x")])).is_err(), "http only: private network");
        assert!(HttpEmbedder::from_env(&env(&[("EMBEDDER_URL", "http://x:1")])).is_err(), "a token is required");
        assert!(HttpEmbedder::from_env(&env(&[("EMBEDDER_URL", "http://x:1"), ("EMBEDDER_TOKEN", "t"), ("EMBEDDER_THRESHOLD", "7")])).is_err());
        let (_, t) = HttpEmbedder::from_env(&env(&[("EMBEDDER_URL", "http://x:1/"), ("EMBEDDER_TOKEN", "t")])).unwrap().unwrap();
        assert_eq!(t, 0.415);
    }

    #[test]
    fn an_unreachable_service_is_an_error_not_a_panic() {
        let (e, _) = HttpEmbedder::from_env(&env(&[("EMBEDDER_URL", "http://127.0.0.1:9"), ("EMBEDDER_TOKEN", "t"), ("EMBEDDER_TIMEOUT", "1")])).unwrap().unwrap();
        assert!(e.embed(&["hello".to_string()]).unwrap_err().contains("could not be reached"));
    }
}
