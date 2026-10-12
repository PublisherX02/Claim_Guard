//! Python's `str()` and `repr()` of a parsed JSON value, so that the Rust adapters produce the same text as the Python reference where that text
//! ends up in a claim (a line id built from a `sequence`) or in a warning (an unexpected `docStatus`). Only what a JSON value can be.

use serde_json::Value;

/// Python's `float.__repr__`: the shortest digits that round-trip, decimal notation for exponents -4..15, otherwise `1e+16` style.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", f.abs()); // like 1.5e0, 1e-7, 1.2345e20
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if f < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let body = if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                format!("{}{}.0", digits, "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{sign}{body}")
    } else {
        let frac = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{sign}{}{frac}e{}{:02}", &digits[..1], if exp < 0 { "-" } else { "+" }, exp.abs())
    }
}

/// True when Python's json module would read this number as an int (no fraction or exponent in the text).
pub fn is_int_text(text: &str) -> bool {
    !text.contains(['.', 'e', 'E'])
}

fn number_str(n: &serde_json::Number) -> String {
    let t = n.to_string();
    if is_int_text(&t) {
        t
    } else {
        float_repr(t.parse::<f64>().unwrap_or(f64::NAN))
    }
}

fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !(c.is_control() || c.is_whitespace() || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0xFEFF | 0xD800..=0xDFFF | 0xE000..=0xF8FF))
}

pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_str(n),
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}: {}", str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// Python's `str()` of a value (what an f-string prints).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python truthiness of a parsed JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => {
            let t = n.to_string();
            if is_int_text(&t) {
                t.trim_start_matches('-').chars().any(|c| c != '0')
            } else {
                t.parse::<f64>().map(|f| f != 0.0).unwrap_or(true)
            }
        }
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_like_python() {
        for (f, want) in [(1.5, "1.5"), (1.0, "1.0"), (100000.0, "100000.0"), (0.1, "0.1"), (1e-7, "1e-07"), (1e16, "1e+16"), (1.5e300, "1.5e+300"), (0.0001, "0.0001"),
                          (0.00001, "1e-05"), (123456789012345.0, "123456789012345.0"), (-2.5, "-2.5"), (1e15, "1000000000000000.0"), (12345.678, "12345.678")] {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn repr_and_str_of_json_values() {
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!({"a": [1, 2.5, true, null, "b"]})), "{'a': [1, 2.5, True, None, 'b']}");
        assert_eq!(py_str(&json!({"reference": "Patient/nope"})), "{'reference': 'Patient/nope'}");
        assert_eq!(py_str(&json!(10u64.pow(18))), "1000000000000000000");
        assert_eq!(py_repr(&json!("tab\there\u{1}")), "'tab\\there\\x01'");
    }

    #[test]
    fn truthiness_matches_python() {
        for (v, want) in [(json!(null), false), (json!(false), false), (json!(0), false), (json!(0.0), false), (json!(""), false), (json!([]), false), (json!({}), false),
                          (json!(1), true), (json!(-1), true), (json!(0.5), true), (json!("0"), true), (json!([0]), true), (json!({"a": 0}), true), (json!(true), true)] {
            assert_eq!(truthy(&v), want, "{v}");
        }
    }
}
