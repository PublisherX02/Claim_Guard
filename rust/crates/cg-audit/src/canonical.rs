//! The digest of an audit row, byte for byte what Python computes:
//! `sha256(json.dumps(obj, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode())`.
//!
//! Compatibility matters in both directions: a log written by the Rust build must verify under the Python verifier and a log written by
//! the Python build must verify here. That needs the same number spelling (an int as digits, a float as Python's repr), the same key
//! order (sorted by code point) and the same string escaping.

use cg_core::Num;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Python's `repr(float)`.
pub fn py_float_repr(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // shortest round-trip digits and the decimal exponent of the first digit, e.g. 1.5e-7 -> digits "15", exp10 -7
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp10: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len() as i32;
    let decpt = exp10 + 1;
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if decpt <= -4 || decpt > 16 {
        let mut m = digits[..1].to_string();
        if n > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        format!("{sign}{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    } else if decpt <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-decpt) as usize))
    } else if decpt >= n {
        format!("{sign}{digits}{}.0", "0".repeat((decpt - n) as usize))
    } else {
        format!("{sign}{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
    }
}

fn write_string(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_number(out: &mut String, text: &str) {
    match Num::from_text(text) {
        Some(Num::Int(i)) => out.push_str(&i.to_string()),
        Some(Num::Float(f)) => out.push_str(&py_float_repr(f)),
        None => out.push_str(text), // a non-finite float cannot be written by serde_json in the first place
    }
}

fn write(out: &mut String, v: &Value, compact: bool, ascii: bool, sorted: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, &n.to_string()),
        Value::String(s) => write_string(out, s, ascii),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if compact { "," } else { ", " });
                }
                write(out, item, compact, ascii, sorted);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sorted {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(if compact { "," } else { ", " });
                }
                write_string(out, k, ascii);
                out.push_str(if compact { ":" } else { ": " });
                write(out, val, compact, ascii, sorted);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(obj, sort_keys=True, separators=(',', ':'), ensure_ascii=False)`
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write(&mut out, v, true, false, true);
    out
}

/// `json.dumps(obj)` (ASCII only, default separators, keys in insertion order): the spelling of a row on disk.
pub fn line(v: &Value) -> String {
    let mut out = String::new();
    write(&mut out, v, false, true, false);
    out
}

/// `audit.digest`
pub fn digest(v: &Value) -> String {
    hex::encode(Sha256::digest(canonical(v).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_like_python_repr() {
        for (f, want) in [
            (0.0, "0.0"), (-0.0, "-0.0"), (1.0, "1.0"), (2.5, "2.5"), (0.1, "0.1"), (0.30000000000000004, "0.30000000000000004"),
            (1e16, "1e+16"), (1e15, "1000000000000000.0"), (123456789012345680.0, "1.2345678901234568e+17"), (0.0001, "0.0001"),
            (0.00001, "1e-05"), (1.5e-7, "1.5e-07"), (-3.25, "-3.25"), (1e22, "1e+22"), (5e-324, "5e-324"), (1.7976931348623157e308, "1.7976931348623157e+308"),
            (100.0, "100.0"), (12345.678, "12345.678"),
        ] {
            assert_eq!(py_float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn canonical_form_sorts_keys_and_keeps_unicode() {
        let v: Value = serde_json::from_str(r#"{"b": 1, "a": [1.0, 2, "é", " "], "c": {"z": null, "y": true}}"#).unwrap();
        assert_eq!(canonical(&v), "{\"a\":[1.0,2,\"é\",\"\u{2028}\"],\"b\":1,\"c\":{\"y\":true,\"z\":null}}");
    }

    #[test]
    fn numbers_are_respelled_not_copied() {
        let v: Value = serde_json::from_str(r#"{"x": 1E2, "y": 5, "z": 0.10, "w": 1e-5}"#).unwrap();
        assert_eq!(canonical(&v), r#"{"w":1e-05,"x":100.0,"y":5,"z":0.1}"#);
    }

    #[test]
    fn on_disk_lines_are_ascii_with_default_separators() {
        let v = json!({"a": "é ", "b": [1, 2]});
        let want = format!("{{\"a\": \"{b}u00e9{b}u2028\", \"b\": [1, 2]}}", b = char::from(92u8));
        assert_eq!(line(&v), want);
    }

    #[test]
    fn known_digest() {
        // sha256 of '{"a":1}'
        assert_eq!(digest(&json!({"a": 1})), "015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862");
    }
}
