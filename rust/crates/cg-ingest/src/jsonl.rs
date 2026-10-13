//! Tolerant JSONL reading (`src/jsonl_reader.py`): a file from Windows (UTF-8 byte-order mark, CRLF), with one invalid byte, one claim whose text
//! contains U+2028, or one record that is not JSON must never abort the run or lose a neighbouring claim. Each bad line is reported; the next is read.

use serde_json::Value;

/// (line number, text or the reason it could not be read). Splits on b"\n" only, tolerates a leading byte-order mark and CRLF, decodes each line
/// on its own so one invalid byte costs one line, and skips blank lines (they still count in the numbering).
pub fn read_lines(bytes: &[u8]) -> Vec<(usize, Result<String, String>)> {
    let mut out = vec![];
    let mut n = 0usize;
    let mut rest = bytes;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|b| *b == b'\n') {
            Some(i) => (&rest[..i + 1], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        rest = tail;
        n += 1;
        let mut raw = line;
        if n == 1 && raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
            raw = &raw[3..];
        }
        while raw.last().is_some_and(|b| *b == b'\r' || *b == b'\n') {
            raw = &raw[..raw.len() - 1];
        }
        if raw.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)) {
            continue;
        }
        match std::str::from_utf8(raw) {
            Ok(s) => out.push((n, Ok(s.to_string()))),
            Err(e) => out.push((n, Err(format!("Invalid UTF-8: {e}")))),
        }
    }
    out
}

/// (value, None) or (None, error).
pub fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(text).map_err(|e| format!("Invalid JSON: {}", e.to_string().chars().take(200).collect::<String>()))
}

/// Python's name for the type of a parsed JSON value, for the "Expected a JSON object, got ..." message.
pub fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if crate::pyfmt::is_int_text(&n.to_string()) { "int" } else { "float" }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// (source reference, record or None, error or None) for a whole file: JSONL, or one JSON array (which can only be read, and lost, as a whole).
pub fn records(bytes: &[u8], name: &str) -> Vec<(String, Option<Value>, Option<String>)> {
    let head: &[u8] = &bytes[..bytes.len().min(4096)];
    let head = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    let first = head.iter().copied().find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c));
    if first == Some(b'[') {
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        let text = match std::str::from_utf8(body) {
            Ok(t) => t,
            Err(e) => return vec![(name.to_string(), None, Some(format!("Invalid UTF-8: {e}")))],
        };
        return match parse_json(text) {
            Ok(Value::Array(items)) => items.into_iter().enumerate().map(|(i, r)| (format!("{name}[{}]", i + 1), Some(r), None)).collect(),
            Ok(_) => vec![(name.to_string(), None, Some("Invalid JSON: expected an array".into()))],
            Err(e) => vec![(name.to_string(), None, Some(e))],
        };
    }
    read_lines(bytes)
        .into_iter()
        .map(|(i, line)| {
            let r = format!("{name}:{i}");
            match line {
                Err(e) => (r, None, Some(e)),
                Ok(t) => match parse_json(&t) {
                    Ok(v) => (r, Some(v), None),
                    Err(e) => (r, None, Some(e)),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_crlf_blank_lines_and_bad_bytes() {
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"{\"a\":1}\r\n\r\n   \n{\"b\":2}\n");
        data.extend_from_slice(&[b'{', b'"', b'c', b'"', b':', 0xFF, b'}', b'\n']);
        data.extend_from_slice("{\"d\":\"x\u{2028}y\"}\n".as_bytes());
        let rec = records(&data, "f.jsonl");
        assert_eq!(rec.len(), 4);
        assert_eq!(rec[0].0, "f.jsonl:1");
        assert_eq!(rec[1].0, "f.jsonl:4");
        assert!(rec[2].2.as_ref().unwrap().starts_with("Invalid UTF-8"));
        assert_eq!(rec[2].0, "f.jsonl:5");
        assert_eq!(rec[3].1.as_ref().unwrap()["d"], "x\u{2028}y");
    }

    #[test]
    fn an_array_is_read_whole_and_a_bad_array_is_one_error() {
        let rec = records(b"  [ {\"a\":1}, 5, null ]", "x.json");
        assert_eq!(rec.len(), 3);
        assert_eq!(rec[1].0, "x.json[2]");
        let bad = records(b"[{\"a\":1}, ", "x.json");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].2.is_some());
    }

    #[test]
    fn a_line_that_is_not_json_is_reported_not_fatal() {
        let rec = records(b"{\"a\":1}\nnot json\n{\"a\":2}\n", "f.jsonl");
        assert_eq!(rec.len(), 3);
        assert!(rec[1].2.as_ref().unwrap().starts_with("Invalid JSON"));
        assert!(rec[2].1.is_some());
    }
}
