//! Settings for the reviewer API, read from the environment and refused when unsafe (`src/access/config.py`).
//!
//! Secrets never get defaults. `dev` generates ephemeral ones for a local demo, is refused off localhost, and is the only way to use cheap
//! password hashing or insecure cookies.

use rand::RngCore;
use std::collections::HashMap;

pub const MIN_SECRET_CHARS: usize = 32;
const LOCAL_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

fn err<T>(m: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(m.into()))
}

#[derive(Clone)]
pub struct Settings {
    pub jwt_secret: String,
    pub fernet_key: String,
    pub audit_anchor_key: String,
    pub pii_key: String,
    pub mongo_uri: Option<String>,
    pub mongo_db: String,
    pub token_ttl_seconds: i64,
    pub bcrypt_rounds: u32,
    pub max_failed_logins: i64,
    pub lockout_seconds: f64,
    pub totp_step: i64,
    pub cookie_secure: bool,
    pub dev: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Settings {{ dev: {}, mongo_db: {:?}, secrets: <hidden> }}", self.dev, self.mongo_db)
    }
}

pub fn random_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, b)
}

/// A new Fernet key (32 random bytes, URL-safe base64 with padding), the way `Fernet.generate_key()` writes it.
pub fn new_fernet_key() -> String {
    fernet::Fernet::generate_key()
}

fn secret(env: &HashMap<String, String>, name: &str, dev: bool) -> Result<String, ConfigError> {
    let value = env.get(name).map(|v| v.trim().to_string()).unwrap_or_default();
    if value.is_empty() {
        return if dev { Ok(random_token(48)) } else { err(format!("{name} is required")) };
    }
    if value.chars().count() < MIN_SECRET_CHARS {
        return err(format!("{name} must be at least {MIN_SECRET_CHARS} characters"));
    }
    Ok(value)
}

fn int(env: &HashMap<String, String>, name: &str, default: i64, low: i64, high: i64) -> Result<i64, ConfigError> {
    let raw = match env.get(name).map(|r| r.trim()) {
        None | Some("") => return Ok(default),
        Some(r) => r,
    };
    let value: i64 = raw.parse().map_err(|_| ConfigError(format!("{name} must be a whole number")))?;
    if !(low..=high).contains(&value) {
        return err(format!("{name} must be between {low} and {high}"));
    }
    Ok(value)
}

pub fn load_settings(env: &HashMap<String, String>, dev: bool, bind_host: &str) -> Result<Settings, ConfigError> {
    if dev && !LOCAL_HOSTS.contains(&bind_host) {
        return err("dev mode is only allowed on localhost");
    }
    let jwt = secret(env, "JWT_SECRET", dev)?;
    let anchor = secret(env, "AUDIT_ANCHOR_KEY", dev)?;
    let pii = secret(env, "PII_KEY", dev)?;
    let mut fernet_raw = env.get("FERNET_KEY").map(|v| v.trim().to_string()).unwrap_or_default();
    if fernet_raw.is_empty() {
        if !dev {
            return err("FERNET_KEY is required");
        }
        fernet_raw = new_fernet_key();
    }
    if fernet::Fernet::new(&fernet_raw).is_none() {
        return err("FERNET_KEY is not a valid Fernet key");
    }
    if jwt == anchor || jwt == pii || anchor == pii {
        return err("JWT_SECRET, AUDIT_ANCHOR_KEY and PII_KEY must be different from one another");
    }
    let mongo = env.get("MONGO_URI").map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if mongo.is_none() && !dev {
        return err("MONGO_URI is required");
    }
    let rounds = int(env, "BCRYPT_ROUNDS", 12, if dev { 4 } else { 12 }, 16)? as u32;
    let secure = !(dev && env.get("COOKIE_SECURE").map(|v| v.trim().to_lowercase()).as_deref() == Some("false"));
    Ok(Settings {
        jwt_secret: jwt,
        fernet_key: fernet_raw,
        audit_anchor_key: anchor,
        pii_key: pii,
        mongo_uri: mongo,
        mongo_db: env.get("MONGO_DB").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).unwrap_or_else(|| "claimguard".into()),
        token_ttl_seconds: int(env, "TOKEN_TTL_SECONDS", 3600, 60, 86400)?,
        bcrypt_rounds: rounds,
        max_failed_logins: int(env, "MAX_FAILED_LOGINS", 5, 1, 20)?,
        lockout_seconds: int(env, "LOCKOUT_SECONDS", 900, 1, 86400)? as f64,
        totp_step: 30,
        cookie_secure: secure,
        dev,
    })
}

#[cfg(test)]
pub fn test_settings() -> Settings {
    let mut env = HashMap::new();
    env.insert("COOKIE_SECURE".into(), "false".into());
    env.insert("BCRYPT_ROUNDS".into(), "4".into());
    let mut s = load_settings(&env, true, "127.0.0.1").expect("dev settings");
    s.audit_anchor_key = "t".repeat(40); // one fixed key: the audit writer's key is process-wide
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn full() -> HashMap<String, String> {
        let mut e = env(&[("JWT_SECRET", &"j".repeat(40)), ("AUDIT_ANCHOR_KEY", &"a".repeat(40)), ("PII_KEY", &"p".repeat(40)), ("MONGO_URI", "mongodb://x")]);
        e.insert("FERNET_KEY".into(), new_fernet_key());
        e
    }

    #[test]
    fn production_needs_every_secret_and_a_database() {
        assert!(load_settings(&full(), false, "0.0.0.0").is_ok());
        for missing in ["JWT_SECRET", "AUDIT_ANCHOR_KEY", "PII_KEY", "FERNET_KEY", "MONGO_URI"] {
            let mut e = full();
            e.remove(missing);
            assert!(load_settings(&e, false, "0.0.0.0").is_err(), "{missing}");
        }
    }

    #[test]
    fn weak_equal_or_malformed_secrets_are_refused() {
        let mut e = full();
        e.insert("JWT_SECRET".into(), "short".into());
        assert!(load_settings(&e, false, "x").is_err());
        let mut e = full();
        e.insert("PII_KEY".into(), "j".repeat(40));
        assert!(load_settings(&e, false, "x").unwrap_err().0.contains("different"));
        let mut e = full();
        e.insert("FERNET_KEY".into(), "not a key".into());
        assert!(load_settings(&e, false, "x").is_err());
    }

    #[test]
    fn dev_mode_is_local_only_and_the_only_way_to_cheap_hashing() {
        assert!(load_settings(&HashMap::new(), true, "0.0.0.0").unwrap_err().0.contains("localhost"));
        assert!(load_settings(&env(&[("BCRYPT_ROUNDS", "4")]), false, "x").is_err());
        assert_eq!(load_settings(&env(&[("BCRYPT_ROUNDS", "4")]), true, "localhost").unwrap().bcrypt_rounds, 4);
        assert!(!load_settings(&env(&[("COOKIE_SECURE", "false")]), true, "::1").unwrap().cookie_secure);
        assert!(load_settings(&full(), false, "x").unwrap().cookie_secure);
    }

    #[test]
    fn numbers_are_range_checked() {
        for (k, v) in [("TOKEN_TTL_SECONDS", "5"), ("MAX_FAILED_LOGINS", "0"), ("LOCKOUT_SECONDS", "abc")] {
            let mut e = full();
            e.insert(k.into(), v.into());
            assert!(load_settings(&e, false, "x").is_err(), "{k}");
        }
    }

    #[test]
    fn debug_output_hides_the_secrets() {
        let text = format!("{:?}", load_settings(&full(), false, "x").unwrap());
        assert!(!text.contains(&"j".repeat(40)) && !text.contains("mongodb://x"));
    }
}
