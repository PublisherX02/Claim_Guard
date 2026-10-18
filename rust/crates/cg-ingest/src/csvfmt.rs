//! The relational CSV export (claims.csv, lines.csv, coverage.csv, authorizations.csv, attachments.csv) rebuilt into claims: a port of
//! `src/csv_to_jsonl.py` with Python's csv reader rules (quotes, doubled quotes, newlines inside quotes, blank lines skipped, byte-order mark).
//!
//! A text cell in a numeric column is kept as text, so that one claim fails the transport contract and is quarantined alone; a number is read
//! only from plain ASCII here; Arabic-Indic digits stay text and are read, with the change recorded, by `cg_core::localize` (docs/55). Where Python would crash on a short row in a
//! numeric column (an AttributeError nobody catches), this reads the missing cell as null and the claim fails validation instead.

use num_bigint::BigInt;
use num_traits::FromPrimitive;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const TABLES: [&str; 5] = ["claims", "lines", "coverage", "authorizations", "attachments"];

#[derive(Debug, PartialEq)]
enum St {
    StartRecord,
    StartField,
    InField,
    InQuoted,
    QuoteInQuoted,
    EatCrnl,
}

/// Python's `csv.reader` (excel dialect, non-strict) over the whole text: a list of rows; a blank line is an empty row.
pub fn parse_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut field = String::new();
    let mut st = St::StartRecord;
    for c in text.chars() {
        match st {
            St::StartRecord | St::StartField => {
                if c == '\n' || c == '\r' {
                    if st == St::StartField {
                        row.push(std::mem::take(&mut field));
                    }
                    rows.push(std::mem::take(&mut row));
                    st = if c == '\r' { St::EatCrnl } else { St::StartRecord };
                } else if c == '"' {
                    st = St::InQuoted;
                } else if c == ',' {
                    row.push(std::mem::take(&mut field));
                    st = St::StartField;
                } else {
                    field.push(c);
                    st = St::InField;
                }
            }
            St::InField => {
                if c == '\n' || c == '\r' {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    st = if c == '\r' { St::EatCrnl } else { St::StartRecord };
                } else if c == ',' {
                    row.push(std::mem::take(&mut field));
                    st = St::StartField;
                } else {
                    field.push(c);
                }
            }
            St::InQuoted => {
                if c == '"' {
                    st = St::QuoteInQuoted;
                } else {
                    field.push(c);
                }
            }
            St::QuoteInQuoted => {
                if c == '"' {
                    field.push('"');
                    st = St::InQuoted;
                } else if c == ',' {
                    row.push(std::mem::take(&mut field));
                    st = St::StartField;
                } else if c == '\n' || c == '\r' {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    st = if c == '\r' { St::EatCrnl } else { St::StartRecord };
                } else {
                    field.push(c); // a character after a closing quote: kept (non-strict), the field continues unquoted
                    st = St::InField;
                }
            }
            St::EatCrnl => {
                if c == '\r' {
                } else if c == '\n' {
                    st = St::StartRecord;
                } else {
                    return Err("new-line character seen in unquoted field".into());
                }
            }
        }
    }
    match st {
        St::InField | St::InQuoted | St::QuoteInQuoted => {
            row.push(field);
            rows.push(row);
        }
        St::StartField => {
            row.push(field);
            rows.push(row);
        }
        _ => {}
    }
    Ok(rows)
}

/// One table as `DictReader` yields it: each row an ordered map; a short row has null for the missing cells; a long row keeps its extra cells
/// under the key "null" (Python's `None` key), which no valid claim, line or attachment may carry.
pub fn dict_rows(bytes: &[u8]) -> Result<Vec<Map<String, Value>>, String> {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = std::str::from_utf8(body).map_err(|e| format!("Invalid UTF-8: {e}"))?;
    let mut rows = parse_rows(text)?.into_iter();
    let Some(names) = rows.next() else { return Ok(vec![]) };
    let mut out = vec![];
    for r in rows {
        if r.is_empty() {
            continue;
        }
        let mut m = Map::new();
        for (i, name) in names.iter().enumerate() {
            m.insert(name.clone(), r.get(i).map(|v| Value::String(v.clone())).unwrap_or(Value::Null));
        }
        if r.len() > names.len() {
            m.insert("null".into(), Value::Array(r[names.len()..].iter().map(|v| Value::String(v.clone())).collect()));
        }
        out.push(m);
    }
    Ok(out)
}

fn ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\x1f')
}

/// Python's `float(v)` for ASCII text: surrounding whitespace, a sign, digits with single underscores between digits, a fraction, an exponent,
/// and the words inf / infinity / nan. Anything else is not a number.
fn py_float(v: &str) -> Option<f64> {
    let t = v.trim_matches(ascii_space);
    if t.is_empty() {
        return None;
    }
    if t.contains('_') {
        let b = t.as_bytes();
        for (i, c) in b.iter().enumerate() {
            if *c == b'_' && !(i > 0 && b[i - 1].is_ascii_digit() && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
                return None;
            }
        }
    }
    let cleaned: String = t.chars().filter(|c| *c != '_').collect();
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    let signs = lower.len() - body.len();
    if signs > 1 {
        return None;
    }
    // Rust also accepts forms Python does not ("infinit", "+nan" are fine in both; hex floats are not accepted by either)
    if !(body == "inf" || body == "infinity" || body == "nan" || body.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'+' | b'-')) && body.bytes().any(|c| c.is_ascii_digit())) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// `number(v)` of the Python converter.
pub fn number(v: &str) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    if !v.is_ascii() {
        return Value::String(v.to_string());
    }
    match py_float(v) {
        None => Value::String(v.to_string()),
        Some(n) if !n.is_finite() => Value::String(v.to_string()),
        Some(n) if n.fract() == 0.0 => match BigInt::from_f64(n) {
            Some(i) => serde_json::from_str::<Value>(&i.to_string()).unwrap_or(Value::Null),
            None => Value::String(v.to_string()),
        },
        Some(n) => serde_json::Number::from_f64(n).map(Value::Number).unwrap_or_else(|| Value::String(v.to_string())),
    }
}

/// A column named in Arabic (docs/55) gets its canonical name; a header the glossary does not know stays as written and the claim is refused for the unknown field.
fn arabic_headers(table: &str, row: Map<String, Value>) -> Map<String, Value> {
    if row.keys().all(|k| k.is_ascii()) {
        return row;
    }
    row.into_iter().map(|(k, v)| (if k.is_ascii() { k } else { cg_core::localize::column_name(table, &k).unwrap_or(k) }, v)).collect()
}

fn blank_to_null(v: &Value) -> Value {
    match v {
        Value::String(s) if s.is_empty() => Value::Null,
        other => other.clone(),
    }
}

fn numeric_columns(table: &str) -> &'static [&'static str] {
    match table {
        "lines" => &["quantity", "unit_price", "net_amount"],
        "authorizations" => &["max_quantity"],
        _ => &[],
    }
}

/// Rebuild the claims from the five tables (file name without `.csv` -> bytes). Errors are whole-folder problems (a missing file or column,
/// invalid UTF-8): the caller reports one quarantined item for the folder.
pub fn convert(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<Value>, String> {
    let mut tables: BTreeMap<&str, Vec<Map<String, Value>>> = BTreeMap::new();
    for t in TABLES {
        let bytes = files.get(t).ok_or_else(|| format!("{t}.csv is missing"))?;
        let rows = dict_rows(bytes).map_err(|e| format!("{t}.csv: {e}"))?;
        tables.insert(t, rows.into_iter().map(|r| arabic_headers(t, r)).collect());
    }
    let mut claims: Vec<Map<String, Value>> = tables.remove("claims").unwrap_or_default();
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for (i, c) in claims.iter().enumerate() {
        let id = c.get("claim_id").ok_or_else(|| "'claim_id'".to_string())?;
        by.insert(id.as_str().map(str::to_string).unwrap_or_default(), i); // the last row of a repeated id is the one that receives the lines
    }
    for c in claims.iter_mut() {
        for v in c.values_mut() {
            *v = blank_to_null(v);
        }
        let total = c.get("total_amount").ok_or_else(|| "'total_amount'".to_string())?.clone();
        let converted = match &total {
            Value::String(s) => number(s),
            _ => Value::Null,
        };
        c.insert("total_amount".into(), converted);
        c.insert("coverage".into(), Value::Null);
        c.insert("lines".into(), Value::Array(vec![]));
        c.insert("authorizations".into(), Value::Array(vec![]));
        c.insert("attachments".into(), Value::Array(vec![]));
    }
    for name in ["lines", "authorizations", "coverage", "attachments"] {
        let numeric = numeric_columns(name);
        for mut row in tables.remove(name).unwrap_or_default() {
            let cid = row.remove("claim_id").ok_or_else(|| "'claim_id'".to_string())?;
            let Some(&idx) = by.get(cid.as_str().unwrap_or("")) else { continue }; // an orphan row has nothing to attach to
            let keys: Vec<String> = row.keys().cloned().collect();
            for k in keys {
                let v = row[&k].clone();
                let nv = if numeric.contains(&k.as_str()) {
                    match &v {
                        Value::String(s) => number(s),
                        _ => Value::Null,
                    }
                } else {
                    blank_to_null(&v)
                };
                row.insert(k, nv);
            }
            let target = &mut claims[idx];
            if name == "coverage" {
                target.insert("coverage".into(), Value::Object(row));
            } else if let Some(Value::Array(a)) = target.get_mut(name) {
                a.push(Value::Object(row));
            }
        }
    }
    Ok(claims.into_iter().map(Value::Object).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_csv_rules() {
        let rows = parse_rows("a,b\r\n\"x,1\",\"he said \"\"hi\"\"\"\r\n\r\nl1\nl2,,\n\"multi\nline\",z").unwrap();
        assert_eq!(rows[0], vec!["a", "b"]);
        assert_eq!(rows[1], vec!["x,1", "he said \"hi\""]);
        assert_eq!(rows[2], Vec::<String>::new());
        assert_eq!(rows[3], vec!["l1"]);
        assert_eq!(rows[4], vec!["l2", "", ""]);
        assert_eq!(rows[5], vec!["multi\nline", "z"]);
        assert!(parse_rows("a,b\rc,d\n").is_err());
        assert_eq!(parse_rows("a,\"unterminated").unwrap()[0], vec!["a", "unterminated"]);
        assert_eq!(parse_rows("x\"y,z").unwrap()[0], vec!["x\"y", "z"]);
        assert_eq!(parse_rows("\"a\"b,c").unwrap()[0], vec!["ab", "c"]);
    }

    #[test]
    fn numbers_like_python() {
        assert_eq!(number(""), Value::Null);
        assert_eq!(number("12"), json!(12));
        assert_eq!(number("12.0"), json!(12));
        assert_eq!(number("12.5"), json!(12.5));
        assert_eq!(number(" 7 "), json!(7));
        assert_eq!(number("1_000"), json!(1000));
        assert_eq!(number("1e3"), json!(1000));
        assert_eq!(number("abc"), json!("abc"));
        assert_eq!(number("inf"), json!("inf"));
        assert_eq!(number("nan"), json!("nan"));
        assert_eq!(number("\u{0661}\u{0662}"), json!("\u{0661}\u{0662}"));
        assert_eq!(number("1__0"), json!("1__0"));
        assert_eq!(number("1_"), json!("1_"));
        assert_eq!(number("0x10"), json!("0x10"));
        assert_eq!(number("--1"), json!("--1"));
        assert_eq!(number("1e30").to_string(), "1000000000000000019884624838656");
    }

    fn files(claims: &str, lines: &str) -> BTreeMap<String, Vec<u8>> {
        let mut f = BTreeMap::new();
        f.insert("claims".into(), claims.as_bytes().to_vec());
        f.insert("lines".into(), lines.as_bytes().to_vec());
        f.insert("coverage".into(), b"claim_id,coverage_id,status\nC1,COV,active\n".to_vec());
        f.insert("authorizations".into(), b"claim_id,authorization_id,max_quantity\nC1,A1,10\n".to_vec());
        f.insert("attachments".into(), b"claim_id,attachment_id\nC1,D1\nORPHAN,D2\n".to_vec());
        f
    }

    #[test]
    fn a_folder_is_rebuilt_and_orphans_are_dropped() {
        let f = files("claim_id,total_amount,notes\nC1,100.0,\n", "claim_id,line_id,quantity,unit_price,net_amount\nC1,L1,2,50,100\nC1,L2,x,,\n");
        let c = convert(&f).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["total_amount"], json!(100));
        assert_eq!(c[0]["notes"], Value::Null);
        assert_eq!(c[0]["lines"][0]["quantity"], json!(2));
        assert_eq!(c[0]["lines"][1]["quantity"], json!("x"));
        assert_eq!(c[0]["lines"][1]["unit_price"], Value::Null);
        assert_eq!(c[0]["authorizations"][0]["max_quantity"], json!(10));
        assert_eq!(c[0]["coverage"]["status"], "active");
        assert_eq!(c[0]["attachments"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_missing_column_or_file_is_a_folder_error_and_extra_cells_poison_only_their_claim() {
        let f = files("claim_id,notes\nC1,\n", "claim_id,line_id\nC1,L1\n");
        assert!(convert(&f).is_err(), "no total_amount column");
        let mut f = files("claim_id,total_amount\nC1,1\n", "claim_id,line_id\nC1,L1,extra\n");
        let c = convert(&f).unwrap();
        assert!(c[0]["lines"][0].as_object().unwrap().contains_key("null"));
        f.remove("lines");
        assert!(convert(&f).is_err());
        let mut f = files("claim_id,total_amount\nC1,1\n", "claim_id,line_id\nC1,L1\n");
        f.insert("claims".into(), vec![0xff, 0xfe, 0x41]);
        assert!(convert(&f).is_err());
    }

    #[test]
    fn a_repeated_claim_id_gives_the_lines_to_the_last_row_only() {
        let f = files("claim_id,total_amount\nC1,1\nC1,2\n", "claim_id,line_id\nC1,L1\n");
        let c = convert(&f).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["lines"], json!([]));
        assert_eq!(c[1]["lines"].as_array().unwrap().len(), 1);
    }
}
