//! The grounding guard for AI-written text: a narrow, mechanical check, not a semantic fact-check (`check_grounding` in
//! `src/llm_adapter.py`). It rejects an explanation that asserts something the finding and the rule text cannot support:
//! a currency symbol that is not there, a claim about "today", clinical or fraud language, approval or payment language (also when
//! it is obfuscated: fullwidth letters, `appr0ved`, `a p p r o v e d`, a word broken across lines), invisible characters, garbled
//! text in another script, long repetition, and positive validity claims about things the finding did not evaluate.
//! A phrase that is already present in the finding or rule text is allowed.

use cg_audit::canonical::dumps;
use fancy_regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

struct Patterns {
    ungrounded: Vec<(Regex, &'static str)>,
    spread: Regex,
    skeleton: Regex,
    validity: Regex,
    hedge: Regex,
    foreign: Regex,
    repetition: Regex,
    invisible: Regex,
    leet_run: Regex,
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("static pattern {p:?} is invalid: {e}"))
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        ungrounded: vec![
            (re(r"[$€£¥]"), "currency symbol not present in the supplied finding"),
            (re(r"(?i)\b(?:in the (?:future|past)|today|yesterday|tomorrow|currently|as of now)\b"), "relative-time claim; the model is not given the current date"),
            (
                re(r"(?i)\bmedical(?:ly)?\s+necess\w*\b|\bfraud(?:ulent)?\b|\bdiagnos(?:ed|is)\s+(?:of|with)\b|\b(?:recommend|prescrib)\w*\s+(?:treatment|surgery|medication|therapy)\b|\bpatient(?:'s)?\s+(?:condition|suffers)\b"),
                "clinical, fraud or medical-necessity judgement; out of scope for this system (docs/01)",
            ),
            (
                re(r"(?i)\b(?:(?:claim|request|submission|invoice)\s+(?:is|was|has\s+been|will\s+be|can\s+be|should\s+be)\s+(?:fully\s+)?(?:approved|accepted|paid|processed|cleared|payable)|approved\s+for\s+payment|payment\s+(?:is\s+|has\s+been\s+|will\s+be\s+)?(?:approved|authori[sz]ed|released|made)|(?:will|can|should)\s+be\s+(?:paid|reimbursed)|ready\s+for\s+payment|cleared\s+for\s+payment|no\s+further\s+review\s+(?:is\s+)?(?:needed|required)|(?:we|i)\s+approve|hereby\s+approved)\b"),
                "approval or payment language; the engine never approves or pays a claim",
            ),
        ],
        spread: re(r"(?<![a-z])(?:[a-z][ \t.\-_*]){3,}[a-z](?![a-z])"),
        skeleton: re(r"(?:claim|request|submission|invoice)(?:is|was|hasbeen|willbe|canbe|shouldbe)(?:fully)?(?:approved|accepted|paid|processed|cleared|payable)|approvedforpayment|payment(?:is|hasbeen|willbe)?(?:approved|authori[sz]ed|released|made)|(?:will|can|should)be(?:paid|reimbursed)|readyforpayment|clearedforpayment|nofurtherreview(?:is)?(?:needed|required)|herebyapproved"),
        validity: re(r"(?i)\b(?:is|are|was|were|looks|seems|appears)\s+(?:valid|correct|acceptable|compliant|fine|proper|in order|within\s+(?:the\s+)?(?:fictional\s+|allowed\s+|policy\s+)?(?:limits?|range|window|period))\b|\b(?:has|have|had|with)\s+(?:a\s+|an\s+)?(?:valid|correct)\b|\b(?:match|matches|matched)\s+(?:correctly|properly)\b|\bno other (?:issues|problems)\b|\botherwise\s+(?:valid|correct|fine)\b"),
        hedge: re(r"(?i)\b(?:whether|if|not|cannot|can't|unable|unclear|impossible|determine|verify|confirm|assess)\b"),
        foreign: re(r"[\x{0370}-\x{1dff}\x{1f00}-\x{1fff}\x{2e80}-\x{9fff}\x{a000}-\x{fdff}\x{fe30}-\x{ffff}]"),
        repetition: re(r"(.)\1{19,}|(\S+\s+)\2{7,}"),
        invisible: re(r"[\x{00ad}\x{034f}\x{200b}-\x{200f}\x{2028}-\x{202e}\x{2060}-\x{2064}\x{2066}-\x{2069}\x{fe00}-\x{fe0f}\x{feff}\x{e0000}-\x{e007f}]"),
        leet_run: re(r"(?<=[a-z])[0134@5$7]+(?=[a-z])"),
    })
}

fn leet(c: char) -> char {
    match c {
        '0' => 'o',
        '1' => 'l',
        '3' => 'e',
        '4' => 'a',
        '5' => 's',
        '7' => 't',
        '@' => 'a',
        '$' => 's',
        other => other,
    }
}

/// Obfuscated spellings undone: compatibility forms, accents removed, lower case, digits for letters, spaced-out letters joined.
fn fold(text: &str) -> String {
    let p = patterns();
    let t: String = text.nfkc().collect::<String>().nfd().filter(|c| !is_mark_nonspacing(*c)).collect::<String>().to_lowercase();
    let t = p.leet_run.replace_all(&t, |c: &fancy_regex::Captures| c[0].chars().map(leet).collect::<String>()).into_owned();
    p.spread.replace_all(&t, |c: &fancy_regex::Captures| c[0].chars().filter(|ch| !matches!(ch, ' ' | '\t' | '.' | '-' | '_' | '*')).collect::<String>()).into_owned()
}

/// Unicode category Mn (nonspacing mark), exactly as Python's `unicodedata.category(c) == 'Mn'`.
fn is_mark_nonspacing(c: char) -> bool {
    unicode_general_category::get_general_category(c) == unicode_general_category::GeneralCategory::NonspacingMark
}

fn letters(text: &str) -> String {
    fold(text).chars().filter(|c| c.is_ascii_lowercase()).collect()
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Ok(()) when the text is grounded in what was supplied; Err(reason) otherwise.
pub fn check_grounding(text: &str, finding: &Value, rule: Option<&Value>) -> Result<(), String> {
    let p = patterns();
    let source = lower(&(dumps(finding) + &rule.map(dumps).unwrap_or_default()));
    let all = |re: &Regex, hay: &str| -> Vec<(usize, String)> { re.find_iter(hay).filter_map(|m| m.ok()).map(|m| (m.start(), m.as_str().to_string())).collect() };

    for (_, m) in all(&p.foreign, text) {
        if !source.contains(&lower(&m)) {
            return Err(format!("Ungrounded statement (garbled text: character {m:?} in an unexpected script)"));
        }
    }
    if p.repetition.is_match(text).unwrap_or(false) {
        return Err("Ungrounded statement (garbled text: long repetition)".into());
    }
    for (_, m) in all(&p.invisible, text) {
        if !source.contains(&m) {
            return Err(format!("Ungrounded statement (invisible character {m:?})"));
        }
    }
    for (pattern, why) in &p.ungrounded {
        for (_, m) in all(pattern, text) {
            if !source.contains(&lower(&m)) {
                return Err(format!("Ungrounded statement ({why}): {m:?}"));
            }
        }
    }
    let (folded, folded_source) = (fold(text), fold(&source));
    for (pattern, why) in &p.ungrounded[2..] {
        for (_, m) in all(pattern, &folded) {
            if !folded_source.contains(&m) {
                return Err(format!("Ungrounded statement ({why}, obfuscated spelling): {m:?}"));
            }
        }
    }
    let (spelled, source_letters) = (letters(text), letters(&source));
    for (_, m) in all(&p.skeleton, &spelled) {
        if !source_letters.contains(&m) {
            return Err(format!("Ungrounded statement (approval or payment language, spelling broken up): {m:?}"));
        }
    }
    for (start, m) in all(&p.validity, text) {
        let before: String = text[..start].chars().rev().take(45).collect::<Vec<_>>().into_iter().rev().collect();
        if source.contains(&lower(&m)) || p.hedge.is_match(&before).unwrap_or(false) {
            continue;
        }
        return Err(format!("Ungrounded statement (asserts validity of something the finding does not cover): {m:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn finding() -> Value {
        json!({"rule_id": "R013", "status": "FAIL", "explanation": "Quantity or unit price violates the fictional limits.", "evidence": [{"path": "/lines/0/quantity", "value": 3}]})
    }

    fn ok(t: &str) -> bool {
        check_grounding(t, &finding(), None).is_ok()
    }

    #[test]
    fn plain_text_about_the_finding_passes() {
        assert!(ok("Line 1 has a quantity of 3, which is above the limit for this service. Ask the provider to confirm the quantity."));
        assert!(ok("The unit price on line 2 cannot be checked because the service code is missing."));
    }

    #[test]
    fn invented_currency_time_clinical_and_approval_language_is_refused() {
        for bad in ["The amount of $180 is too high.", "This was submitted in the future.", "This looks like fraud.", "A medically necessary visit.", "The claim is approved.",
                    "Payment will be released.", "No further review is needed.", "We approve this one.", "diagnosed with a condition"] {
            assert!(!ok(bad), "{bad}");
        }
    }

    #[test]
    fn obfuscated_approval_is_caught() {
        for bad in ["The claim is appr0ved.", "The claim is a p p r o v e d.", "the claim is approved", "The claim is ap-\nproved.", "The claim is \u{ff41}\u{ff50}\u{ff50}\u{ff52}\u{ff4f}\u{ff56}\u{ff45}\u{ff44}."] {
            assert!(!ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn invisible_foreign_and_repeated_text_is_refused() {
        assert!(!ok("The claim is app\u{200b}roved."));
        assert!(!ok("The quantity is 3 \u{4e2d}\u{6587}."));
        assert!(!ok(&format!("The quantity is {}.", "x".repeat(25))));
        assert!(!ok(&"word ".repeat(10)));
    }

    #[test]
    fn validity_claims_need_to_be_hedged_or_already_in_the_source() {
        assert!(!ok("The price on the second line is valid."));
        assert!(ok("It cannot be determined whether the price is valid."));
        assert!(check_grounding("The fictional limits are applied.", &finding(), Some(&json!({"logic": "the price is valid when ..."}))).is_ok());
    }

    #[test]
    fn a_phrase_already_in_the_finding_is_allowed() {
        let f = json!({"explanation": "The currency symbol $ appears in the note.", "rule_id": "R015"});
        assert!(check_grounding("The $ sign was found.", &f, None).is_ok());
    }
}
