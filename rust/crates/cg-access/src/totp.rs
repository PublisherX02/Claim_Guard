//! Authenticator-app one-time codes (RFC 6238) and the encryption of each user's seed at rest (`src/access/totp.py`).
//! The code is HMAC-SHA1 over the 30-second step, six digits, exactly what Google Authenticator and pyotp compute.

use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rand::RngCore;
use sha1::Sha1;
use subtle::ConstantTimeEq;

pub fn new_secret() -> String {
    let mut b = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &b)
}

const KEEP: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'.').remove(b'-').remove(b'~');

/// `pyotp.TOTP(secret).provisioning_uri(name=badge_id, issuer_name='ClaimGuard')`
pub fn provisioning_uri(secret: &str, badge_id: &str, issuer: &str) -> String {
    let q = |s: &str| utf8_percent_encode(s, KEEP).to_string();
    format!("otpauth://totp/{}:{}?secret={}&issuer={}", q(issuer), q(badge_id), secret, q(issuer))
}

pub fn encrypt_secret(secret: &str, fernet_key: &str) -> Result<String, String> {
    fernet::Fernet::new(fernet_key).map(|f| f.encrypt(secret.as_bytes())).ok_or_else(|| "bad fernet key".to_string())
}

pub fn decrypt_secret(token: &str, fernet_key: &str) -> Result<String, String> {
    let f = fernet::Fernet::new(fernet_key).ok_or("the stored secret cannot be decrypted")?;
    let bytes = f.decrypt(token).map_err(|_| "the stored secret cannot be decrypted".to_string())?;
    String::from_utf8(bytes).map_err(|_| "the stored secret cannot be decrypted".to_string())
}

pub fn current_step(now: f64, step: i64) -> i64 {
    (now / step as f64).floor() as i64
}

/// The six-digit code for one step of this secret.
pub fn code_at(secret: &str, step_number: i64) -> Option<String> {
    let key = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &secret.trim_end_matches('=').to_uppercase())?;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&(step_number as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[19] & 0x0f) as usize;
    let bin = ((h[offset] as u32 & 0x7f) << 24) | ((h[offset + 1] as u32) << 16) | ((h[offset + 2] as u32) << 8) | h[offset + 3] as u32;
    Some(format!("{:06}", bin % 1_000_000))
}

/// The step number if `code` is the code for exactly the current step, otherwise None. The caller must then consume the step
/// (`UserStore::mark_totp_used`) so the same code cannot be used twice.
pub fn verify_code(secret: &str, code: &str, now: f64, step: i64) -> Option<i64> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = current_step(now, step);
    let expected = code_at(secret, current)?;
    bool::from(expected.as_bytes().ct_eq(code.as_bytes())).then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_test_vectors() {
        // RFC 6238 appendix B, SHA-1, secret "12345678901234567890", 8 digits: we use 6, so the last six digits
        let secret = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, b"12345678901234567890");
        for (time, want) in [(59_i64, "287082"), (1_111_111_109, "081804"), (1_111_111_111, "050471"), (1_234_567_890, "005924"), (2_000_000_000, "279037")] {
            assert_eq!(code_at(&secret, time / 30).unwrap(), want, "t={time}");
        }
    }

    #[test]
    fn only_the_exact_current_step_verifies() {
        let secret = new_secret();
        let now = 1_700_000_015.0;
        let good = code_at(&secret, current_step(now, 30)).unwrap();
        assert_eq!(verify_code(&secret, &good, now, 30), Some(current_step(now, 30)));
        assert_eq!(verify_code(&secret, &good, now + 30.0, 30), None, "the next step is a different code");
        for bad in ["", "12345", "1234567", "12345a", "１２３４５６", " 12345", &good.replace(good.chars().next().unwrap(), "x")] {
            assert_eq!(verify_code(&secret, bad, now, 30), None, "{bad:?}");
        }
    }

    #[test]
    fn seeds_round_trip_through_encryption_and_wrong_keys_fail() {
        let key = crate::settings::new_fernet_key();
        let token = encrypt_secret("JBSWY3DPEHPK3PXP", &key).unwrap();
        assert_eq!(decrypt_secret(&token, &key).unwrap(), "JBSWY3DPEHPK3PXP");
        assert!(decrypt_secret(&token, &crate::settings::new_fernet_key()).is_err());
        assert!(decrypt_secret("garbage", &key).is_err());
    }

    #[test]
    fn the_provisioning_uri_matches_the_authenticator_format() {
        assert_eq!(provisioning_uri("ABC234", "CG-2002", "ClaimGuard"), "otpauth://totp/ClaimGuard:CG-2002?secret=ABC234&issuer=ClaimGuard");
    }
}
