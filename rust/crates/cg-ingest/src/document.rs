//! The client of the document reader sidecar (`deploy/ocr/service.py`): hands it a PDF or a picture, gets back the text of every page and how each page
//! was read (`text_layer`: the PDF already carried its text; `ocr`: it was read from the picture), with per-word confidence where the engine gives one.
//!
//! The main server never parses a PDF or decodes a picture; this client only sends the bytes the gate has passed to the reader's own container.

use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ReaderConfig {
    pub url: String,
    pub token: String,
    pub engine: String,
    pub langs: String,
    pub timeout: Duration,
}

impl ReaderConfig {
    /// DOCUMENT_READER_URL (http://host:port), DOCUMENT_READER_TOKEN, DOCUMENT_READER_ENGINE (auto|tesseract|vlm), DOCUMENT_READER_LANGS (eng+ara).
    /// None when no reader is configured: uploading documents is then refused with a clear message, and the other formats still work.
    pub fn from_env(env: &std::collections::HashMap<String, String>) -> Result<Option<ReaderConfig>, String> {
        let url = env.get("DOCUMENT_READER_URL").map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty());
        let Some(url) = url else { return Ok(None) };
        if !url.starts_with("http://") {
            return Err("DOCUMENT_READER_URL must start with http:// (the reader runs on the private network)".into());
        }
        let token = env.get("DOCUMENT_READER_TOKEN").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("DOCUMENT_READER_TOKEN is required with DOCUMENT_READER_URL")?;
        let engine = env.get("DOCUMENT_READER_ENGINE").map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).unwrap_or_else(|| "auto".into());
        if !["auto", "tesseract", "vlm"].contains(&engine.as_str()) {
            return Err("DOCUMENT_READER_ENGINE must be auto, tesseract or vlm".into());
        }
        let langs = env.get("DOCUMENT_READER_LANGS").map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).unwrap_or_else(|| "eng".into());
        let timeout = env.get("DOCUMENT_READER_TIMEOUT").and_then(|s| s.trim().parse::<u64>().ok()).filter(|n| (5..=900).contains(n)).unwrap_or(300);
        Ok(Some(ReaderConfig { url, token, engine, langs, timeout: Duration::from_secs(timeout) }))
    }
}

#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    pub conf: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub page: usize,
    pub text: String,
    /// a second, independent reading of the same page (the sidecar's `both` mode), used to cross-check the first
    pub second_text: Option<String>,
    pub source: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone)]
pub struct Read {
    pub engine: String,
    pub pages: Vec<Page>,
}

/// `Refused`: the reader looked at the file and will not read it (hostile, broken, too many pages): a property of the file. `Unavailable`: the reader
/// could not be reached or failed: a property of the system.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadError {
    Refused(String),
    Unavailable(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Refused(m) | ReadError::Unavailable(m) => write!(f, "{m}"),
        }
    }
}

pub fn parse_reply(v: &Value) -> Result<Read, ReadError> {
    let pages = v["pages"].as_array().ok_or_else(|| ReadError::Unavailable("the reader's answer has no pages".into()))?;
    let mut out = vec![];
    for p in pages {
        let words = p["words"]
            .as_array()
            .map(|a| a.iter().filter_map(|w| Some(Word { text: w["text"].as_str()?.to_string(), conf: w["conf"].as_f64() })).collect())
            .unwrap_or_default();
        out.push(Page { page: p["page"].as_u64().unwrap_or(0) as usize, text: p["text"].as_str().unwrap_or("").to_string(), second_text: p["second_text"].as_str().map(str::to_string), source: p["source"].as_str().unwrap_or("ocr").to_string(), words });
    }
    Ok(Read { engine: v["engine"].as_str().unwrap_or("unknown").to_string(), pages: out })
}

pub fn read_document(cfg: &ReaderConfig, bytes: &[u8], mime: &str) -> Result<Read, ReadError> {
    let agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
    let resp = agent
        .post(&format!("{}/read", cfg.url))
        .set("authorization", &format!("Bearer {}", cfg.token))
        .set("content-type", mime)
        .set("x-engine", &cfg.engine)
        .set("x-langs", &cfg.langs)
        .send_bytes(bytes);
    match resp {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|_| ReadError::Unavailable("the reader answered with something that is not JSON".into()))?;
            parse_reply(&v)
        }
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_json::<Value>().ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_default();
            match code {
                400 | 413 | 415 | 422 => Err(ReadError::Refused(if msg.is_empty() { format!("the reader refused the file (HTTP {code})") } else { msg })),
                401 => Err(ReadError::Unavailable("the reader refused our token (check DOCUMENT_READER_TOKEN)".into())),
                _ => Err(ReadError::Unavailable(format!("the reader failed (HTTP {code}{})", if msg.is_empty() { String::new() } else { format!(": {msg}") }))),
            }
        }
        Err(ureq::Error::Transport(t)) => Err(ReadError::Unavailable(format!("the reader could not be reached: {t}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn configuration_is_strict() {
        assert!(ReaderConfig::from_env(&env(&[])).unwrap().is_none());
        assert!(ReaderConfig::from_env(&env(&[("DOCUMENT_READER_URL", "https://x")])).is_err());
        assert!(ReaderConfig::from_env(&env(&[("DOCUMENT_READER_URL", "http://ocr:8090")])).is_err());
        assert!(ReaderConfig::from_env(&env(&[("DOCUMENT_READER_URL", "http://ocr:8090"), ("DOCUMENT_READER_TOKEN", "t"), ("DOCUMENT_READER_ENGINE", "magic")])).is_err());
        let c = ReaderConfig::from_env(&env(&[("DOCUMENT_READER_URL", "http://ocr:8090/"), ("DOCUMENT_READER_TOKEN", "t")])).unwrap().unwrap();
        assert_eq!((c.url.as_str(), c.engine.as_str(), c.langs.as_str()), ("http://ocr:8090", "auto", "eng"));
    }

    #[test]
    fn the_reply_is_parsed_and_a_bad_one_is_an_error() {
        let r = parse_reply(&json!({"engine": "tesseract", "pages": [{"page": 1, "text": "hello", "source": "ocr", "words": [{"text": "hello", "conf": 0.9, "box": [1, 2, 3, 4]}]},
                                                                      {"page": 2, "text": "x", "source": "text_layer", "words": []}]})).unwrap();
        assert_eq!(r.pages.len(), 2);
        assert_eq!(r.pages[0].words[0].conf, Some(0.9));
        assert_eq!(r.pages[1].source, "text_layer");
        assert!(parse_reply(&json!({"engine": "x"})).is_err());
    }

    #[test]
    fn an_unreachable_reader_is_unavailable_not_a_refusal() {
        let cfg = ReaderConfig { url: "http://127.0.0.1:9".into(), token: "t".into(), engine: "auto".into(), langs: "eng".into(), timeout: Duration::from_secs(2) };
        assert!(matches!(read_document(&cfg, b"x", "image/png"), Err(ReadError::Unavailable(_))));
    }
}
