//! Text helpers that mirror the Python helpers of the same job.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::Value;

/// Python's `str.isspace` for one character: Unicode white space plus the four information separators U+001C to U+001F.
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` emptiness: no characters, or only white space.
pub fn blank(s: &str) -> bool {
    s.chars().all(py_isspace)
}

/// `engine_core.empty` on a cleaned optional text: missing, or only white space.
pub fn empty(s: &Option<String>) -> bool {
    s.as_deref().map_or(true, blank)
}

/// `urllib.parse.quote(text, safe='')`: everything except letters, digits and `_ . - ~` becomes %XX of its UTF-8 bytes.
const KEEP: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'.').remove(b'-').remove(b'~');

pub fn quote(text: &str) -> String {
    utf8_percent_encode(text, KEEP).to_string()
}

/// `engine_core.pointer`: an RFC 6901 style lookup (`/lines/0/quantity`) in the original claim. None when it cannot be resolved.
pub fn pointer<'a>(claim: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = claim;
    for part in path.trim_matches('/').split('/') {
        if path.is_empty() {
            return Some(claim);
        }
        let part = part.replace("~1", "/").replace("~0", "~");
        cur = match cur {
            Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            Value::Object(map) => map.get(&part)?,
            _ => return None,
        };
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blank_follows_python_strip() {
        assert!(blank(""));
        assert!(blank("  \t\n"));
        assert!(blank("\u{a0}\u{2003}"));
        assert!(blank("\u{1f}"), "python treats the information separators as space");
        assert!(!blank(" a "));
        assert!(empty(&None));
        assert!(!empty(&Some("x".into())));
    }

    #[test]
    fn quote_matches_python() {
        assert_eq!(quote("USD R009:MISMATCH:"), "USD%20R009%3AMISMATCH%3A");
        assert_eq!(quote("a_b.c-d~e"), "a_b.c-d~e");
        assert_eq!(quote("é\n"), "%C3%A9%0A");
        assert_eq!(quote(""), "");
    }

    #[test]
    fn pointers_resolve_or_return_none() {
        let c = json!({"a": [{"b": 1}], "x~y": {"p/q": 2}});
        assert_eq!(pointer(&c, "/a/0/b"), Some(&json!(1)));
        assert_eq!(pointer(&c, "/x~0y/p~1q"), Some(&json!(2)));
        assert_eq!(pointer(&c, "/a/1"), None);
        assert_eq!(pointer(&c, "/a/x"), None);
        assert_eq!(pointer(&c, "/nope"), None);
    }
}
