//! Signed session tokens (`src/access/tokens.py`): HS256 JWTs that prove only that this server issued a session to a badge. A token
//! carries no level and no permissions: those are read from the database on every request, so a demotion or deactivation applies at
//! once and a forged claim has nothing to forge. Tokens written by the Python build verify here and the other way round.

use crate::settings::{random_token, Settings};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const MAX_TOKEN_CHARS: usize = 4096;

#[derive(Debug, thiserror::Error)]
#[error("invalid token")]
pub struct TokenError;

#[derive(Debug, Clone)]
pub struct Issued {
    pub token: String,
    pub jti: String,
    pub csrf: String,
    pub expires_at: i64,
}

pub fn issue(settings: &Settings, badge_id: &str, now: f64) -> Issued {
    let issued_at = now as i64;
    let (jti, csrf) = (random_token(16), random_token(24));
    let expires_at = issued_at + settings.token_ttl_seconds;
    let claims = json!({"sub": badge_id, "jti": jti, "csrf": csrf, "iat": issued_at, "exp": expires_at});
    let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(settings.jwt_secret.as_bytes()))
        .expect("an HS256 token with a text secret can always be signed");
    Issued { token, jti, csrf, expires_at }
}

pub fn decode(settings: &Settings, token: &str, now: f64) -> Result<Value, TokenError> {
    if token.is_empty() || token.len() > MAX_TOKEN_CHARS {
        return Err(TokenError);
    }
    // The algorithm is pinned (a token claiming "none" or another algorithm is refused first); time checks are done below against
    // `now` so they are exact and testable.
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = false;
    v.validate_nbf = false;
    v.required_spec_claims = HashSet::new();
    let data = jsonwebtoken::decode::<Value>(token, &DecodingKey::from_secret(settings.jwt_secret.as_bytes()), &v).map_err(|_| TokenError)?;
    let p = data.claims;
    for name in ["sub", "jti", "csrf"] {
        if !p.get(name).and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
            return Err(TokenError);
        }
    }
    let int = |name: &str| p.get(name).filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_i64);
    let (iat, exp) = (int("iat").ok_or(TokenError)?, int("exp").ok_or(TokenError)?);
    if exp as f64 <= now || iat as f64 > now {
        return Err(TokenError);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_settings;
    use base64::Engine;

    fn b64(v: &Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    }

    #[test]
    fn issue_then_decode() {
        let s = test_settings();
        let t = issue(&s, "CG-2002", 1000.0);
        let p = decode(&s, &t.token, 1500.0).unwrap();
        assert_eq!((p["sub"].as_str(), p["jti"].as_str(), p["csrf"].as_str()), (Some("CG-2002"), Some(t.jti.as_str()), Some(t.csrf.as_str())));
        assert_eq!(t.expires_at, 1000 + s.token_ttl_seconds);
    }

    #[test]
    fn expiry_and_future_tokens_are_refused_exactly_at_the_edge() {
        let s = test_settings();
        let t = issue(&s, "CG-2002", 1000.0);
        assert!(decode(&s, &t.token, t.expires_at as f64 - 0.5).is_ok());
        assert!(decode(&s, &t.token, t.expires_at as f64).is_err());
        assert!(decode(&s, &t.token, 999.0).is_err(), "issued in the future");
    }

    #[test]
    fn forgeries_are_refused() {
        let s = test_settings();
        let t = issue(&s, "CG-2002", 1000.0);
        let mut other = test_settings();
        other.jwt_secret = "x".repeat(48);
        assert!(decode(&other, &t.token, 1100.0).is_err(), "signed with another secret");
        let parts: Vec<&str> = t.token.split('.').collect();
        let none = format!("{}.{}.", b64(&json!({"alg": "none", "typ": "JWT"})), parts[1]);
        assert!(decode(&s, &none, 1100.0).is_err(), "alg none");
        let tampered = format!("{}.{}.{}", parts[0], b64(&json!({"sub": "CG-4004", "jti": "j", "csrf": "c", "iat": 1000, "exp": 99999})), parts[2]);
        assert!(decode(&s, &tampered, 1100.0).is_err(), "payload edited, signature kept");
        for bad in ["", "a.b.c", "x", &"a".repeat(5000)] {
            assert!(decode(&s, bad, 1100.0).is_err());
        }
    }

    #[test]
    fn a_token_without_the_required_claims_is_refused() {
        let s = test_settings();
        for payload in [
            json!({"sub": "a", "jti": "j", "csrf": "c", "iat": 1000}),
            json!({"sub": "", "jti": "j", "csrf": "c", "iat": 1000, "exp": 2000}),
            json!({"sub": "a", "jti": "j", "csrf": "c", "iat": "1000", "exp": 2000}),
            json!({"sub": "a", "jti": "j", "csrf": "c", "iat": 1000.5, "exp": 2000}),
        ] {
            let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &payload, &EncodingKey::from_secret(s.jwt_secret.as_bytes())).unwrap();
            assert!(decode(&s, &token, 1500.0).is_err(), "{payload}");
        }
    }
}
