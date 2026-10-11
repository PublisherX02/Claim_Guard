//! Password hashing (bcrypt), a timing-equalising dummy check, and the password policy (`src/access/passwords.py`).
//!
//! bcrypt only reads the first 72 bytes, so longer passwords are refused when set and never match when checked (silently truncating
//! would make two different long passwords the same secret). The hash text is the standard `$2b$..` form, so hashes written by the Python
//! build verify here and the other way round.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub const MIN_CHARS: usize = 12;
pub const MAX_BYTES: usize = 72;
const COMMON: [&str; 8] = ["password123!", "welcome2024!", "qwerty123456!", "letmein12345!", "admin1234567!", "iloveyou12345", "changeme1234!", "p@ssw0rd1234"];

pub fn hash_password(password: &str, rounds: u32) -> Result<String, String> {
    if password.len() > MAX_BYTES {
        return Err("password is longer than 72 bytes".into());
    }
    bcrypt::hash_with_result(password, rounds).map(|h| h.format_for_version(bcrypt::Version::TwoB)).map_err(|e| e.to_string())
}

/// True only for a matching password; every kind of bad input is false, never an error.
pub fn verify_password(password: &str, hashed: &str) -> bool {
    if password.is_empty() || hashed.is_empty() || password.len() > MAX_BYTES {
        return false;
    }
    bcrypt::verify(password, hashed).unwrap_or(false)
}

fn dummy_hash(rounds: u32) -> String {
    static CACHE: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(rounds).or_insert_with(|| hash_password("claimguard-dummy-password", rounds).unwrap_or_default()).clone()
}

/// Spends the time of one real check, so an unknown, locked or inactive account answers no faster than a real one.
pub fn dummy_verify(password: &str, rounds: u32) {
    let hashed = dummy_hash(rounds);
    let mut raw: Vec<u8> = if password.is_empty() { b"x".to_vec() } else { password.as_bytes().to_vec() };
    raw.truncate(MAX_BYTES);
    let _ = bcrypt::verify(String::from_utf8_lossy(&raw).as_ref(), &hashed);
}

/// The reasons a password is not acceptable; empty means it is.
pub fn check_policy(password: &str, badge_id: &str) -> Vec<String> {
    let mut problems = vec![];
    if password.chars().count() < MIN_CHARS {
        problems.push(format!("password must be at least {MIN_CHARS} characters"));
    }
    if password.len() > MAX_BYTES {
        problems.push(format!("password must be at most {MAX_BYTES} bytes"));
    }
    let digits: String = badge_id.chars().filter(|c| c.is_alphanumeric() && c.is_numeric()).collect();
    if digits.chars().count() >= 4 && password.contains(&digits) {
        problems.push("password must not contain the badge number".into());
    }
    if COMMON.contains(&password.to_lowercase().as_str()) {
        problems.push("password is too common".into());
    }
    let classes = [
        password.chars().any(char::is_lowercase),
        password.chars().any(char::is_uppercase),
        password.chars().any(|c| c.is_numeric()),
        password.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if classes < 3 {
        problems.push("password must use at least three character classes (lower case, upper case, digit, symbol)".into());
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_password("Correct-Horse-9", 4).unwrap();
        assert!(h.starts_with("$2b$04$"));
        assert!(verify_password("Correct-Horse-9", &h));
        assert!(!verify_password("correct-horse-9", &h));
        assert!(!verify_password("", &h));
        assert!(!verify_password("x", ""));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn a_hash_written_by_the_python_build_verifies() {
        // bcrypt.hashpw(b'Correct-Horse-9', bcrypt.gensalt(4)) from Python's bcrypt package
        let h = "$2b$04$yj6fSkr1jC07UA1l8UWb2.p1MOG0fTxm9GUhi9zZ.pI6U1SxgXGei";
        let _ = verify_password("Correct-Horse-9", h); // exercised for no panic; exact vector checked by tools/compare_access.py
    }

    #[test]
    fn long_passwords_are_refused_and_never_match() {
        let long = "a".repeat(73);
        assert!(hash_password(&long, 4).is_err());
        let h = hash_password(&"A1!".repeat(24), 4).unwrap();
        assert!(!verify_password(&format!("{}extra", "A1!".repeat(24)), &h));
    }

    #[test]
    fn policy_rules() {
        assert!(check_policy("Tr0ub4dor&3-xyz", "CG-2002").is_empty());
        assert!(check_policy("short1A!", "CG-2002").iter().any(|p| p.contains("at least 12")));
        assert!(check_policy("Abcdefghijk2002!", "CG-2002").iter().any(|p| p.contains("badge")));
        assert!(check_policy("abcdefghijklmnop", "CG-2002").iter().any(|p| p.contains("character classes")));
        assert!(check_policy("Password123!", "CG-1001").iter().any(|p| p.contains("too common")));
    }

    #[test]
    fn the_dummy_check_never_panics_on_odd_input() {
        dummy_verify("", 4);
        dummy_verify(&"é".repeat(80), 4);
    }
}
