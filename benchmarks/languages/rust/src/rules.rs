use rust_decimal::{Decimal, RoundingStrategy};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

type Num = Option<Decimal>;
type S = Option<String>;

fn nz<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Num, D::Error> {
    rust_decimal::serde::arbitrary_precision_option::deserialize(d)
}

#[derive(Deserialize, Default)]
pub struct Coverage {
    #[serde(default)]
    pub status: S,
    #[serde(default)]
    pub beneficiary_patient_id: S,
    #[serde(default)]
    pub member_id: S,
    #[serde(default)]
    pub start_date: S,
    #[serde(default)]
    pub end_date: S,
}

#[derive(Deserialize)]
pub struct Line {
    #[serde(default)]
    pub service_code: S,
    #[serde(default)]
    pub service_date: S,
    #[serde(default)]
    pub modifier: S,
    #[serde(default, deserialize_with = "nz")]
    pub quantity: Num,
    #[serde(default, deserialize_with = "nz")]
    pub unit_price: Num,
    #[serde(default, deserialize_with = "nz")]
    pub net_amount: Num,
    #[serde(default)]
    pub authorization_id: S,
}

#[derive(Deserialize)]
pub struct Auth {
    #[serde(default)]
    pub authorization_id: S,
    #[serde(default)]
    pub patient_id: S,
    #[serde(default)]
    pub service_code: S,
    #[serde(default)]
    pub status: S,
    #[serde(default)]
    pub valid_from: S,
    #[serde(default)]
    pub valid_to: S,
    #[serde(default, deserialize_with = "nz")]
    pub max_quantity: Num,
}

#[derive(Deserialize)]
pub struct Attachment {
    #[serde(default, rename = "type")]
    pub kind: S,
    #[serde(default)]
    pub patient_id: S,
    #[serde(default)]
    pub service_code: S,
    #[serde(default)]
    pub service_date: S,
    #[serde(default)]
    pub document_status: S,
}

#[derive(Deserialize)]
pub struct Claim {
    pub claim_id: String,
    #[serde(default)]
    pub invoice_number: S,
    #[serde(default)]
    pub patient_id: S,
    #[serde(default)]
    pub member_id: S,
    #[serde(default)]
    pub provider_id: S,
    #[serde(default)]
    pub policy_id: S,
    #[serde(default)]
    pub diagnosis_code: S,
    #[serde(default)]
    pub submission_date: S,
    #[serde(default)]
    pub currency: S,
    #[serde(default, deserialize_with = "nz")]
    pub total_amount: Num,
    #[serde(default)]
    pub coverage: Coverage,
    pub lines: Vec<Line>,
    #[serde(default)]
    pub authorizations: Vec<Auth>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
struct RawPolicy {
    currency: String,
    submission_window_days: i64,
    allowed_providers: Vec<String>,
    auth_required_services: Vec<String>,
    required_documents: HashMap<String, String>,
    #[serde(deserialize_with = "map_num")]
    max_unit_price: HashMap<String, Decimal>,
    #[serde(deserialize_with = "map_num")]
    max_quantity_per_line: HashMap<String, Decimal>,
}

fn map_num<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HashMap<String, Decimal>, D::Error> {
    #[derive(Deserialize)]
    struct W(#[serde(with = "rust_decimal::serde::arbitrary_precision")] Decimal);
    let m: HashMap<String, W> = HashMap::deserialize(d)?;
    Ok(m.into_iter().map(|(k, v)| (k, v.0)).collect())
}

pub struct Policy {
    currency: String,
    window: i64,
    allowed: HashSet<String>,
    auth_required: HashSet<String>,
    required_documents: HashMap<String, String>,
    max_unit_price: HashMap<String, Decimal>,
    max_quantity: HashMap<String, Decimal>,
}

pub struct Pack {
    policies: HashMap<String, Policy>,
    services: HashSet<String>,
}

impl Pack {
    pub fn new(policies_json: &str, services_json: &str) -> Result<Pack, serde_json::Error> {
        let raw: HashMap<String, RawPolicy> = serde_json::from_str(policies_json)?;
        let svc: HashMap<String, serde_json::Value> = serde_json::from_str(services_json)?;
        let policies = raw
            .into_iter()
            .map(|(id, p)| {
                (
                    id,
                    Policy {
                        currency: p.currency,
                        window: p.submission_window_days,
                        allowed: p.allowed_providers.into_iter().collect(),
                        auth_required: p.auth_required_services.into_iter().collect(),
                        required_documents: p.required_documents,
                        max_unit_price: p.max_unit_price,
                        max_quantity: p.max_quantity_per_line,
                    },
                )
            })
            .collect();
        Ok(Pack { policies, services: svc.into_keys().collect() })
    }
}

// ---- helpers --------------------------------------------------------------------------------------------------------

fn empty(s: &S) -> bool {
    match s {
        None => true,
        Some(v) => v.trim().is_empty(),
    }
}

fn day(s: &S) -> Option<i64> {
    let v = s.as_ref()?.as_bytes();
    if v.len() != 10 {
        return None;
    }
    for (i, b) in v.iter().enumerate() {
        if i == 4 || i == 7 {
            if *b != b'-' {
                return None;
            }
        } else if !b.is_ascii_digit() {
            return None;
        }
    }
    let n = |i: usize| (v[i] - b'0') as i64;
    let y = n(0) * 1000 + n(1) * 100 + n(2) * 10 + n(3);
    let m = (n(5) * 10 + n(6)) as usize;
    let d = n(8) * 10 + n(9);
    if y < 1 || !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let mut dim = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1];
    if m == 2 && leap {
        dim = 29;
    }
    if d > dim {
        return None;
    }
    let yy = y - 1;
    let cum = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = yy * 365 + yy / 4 - yy / 100 + yy / 400 + cum[m - 1] + d;
    if m > 2 && leap {
        days += 1;
    }
    Some(days)
}

fn money(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

const PASS: u8 = b'P';
const FAIL: u8 = b'F';
const UNABLE: u8 = b'U';
const NA: u8 = b'N';

fn verdict(f: bool, u: bool, ok: u8) -> u8 {
    if f {
        FAIL
    } else if u {
        UNABLE
    } else {
        ok
    }
}

fn cent() -> Decimal {
    Decimal::new(1, 2)
}

impl Pack {
    fn policy(&self, c: &Claim) -> Option<&Policy> {
        c.policy_id.as_ref().and_then(|p| self.policies.get(p))
    }
    fn known(&self, code: &S) -> bool {
        !empty(code) && self.services.contains(code.as_ref().unwrap())
    }
    fn find_auth<'a>(c: &'a Claim, id: &str) -> Option<&'a Auth> {
        c.authorizations.iter().find(|a| a.authorization_id.as_deref() == Some(id))
    }

    fn r001(c: &Claim) -> u8 {
        let mut bad = empty(&c.invoice_number) || empty(&c.member_id) || empty(&c.diagnosis_code);
        for l in &c.lines {
            if empty(&l.service_date) || empty(&l.service_code) || l.quantity.is_none() || l.unit_price.is_none() || l.net_amount.is_none() {
                bad = true;
            }
        }
        if bad { FAIL } else { PASS }
    }

    fn r002(c: &Claim) -> u8 {
        let sub = day(&c.submission_date);
        let (mut f, mut u) = (false, false);
        for l in &c.lines {
            match (day(&l.service_date), sub) {
                (Some(d), Some(s)) => {
                    if d > s {
                        f = true
                    }
                }
                _ => u = true,
            }
        }
        verdict(f, u, PASS)
    }

    fn r003(c: &Claim) -> u8 {
        let cv = &c.coverage;
        let (start, end) = (day(&cv.start_date), day(&cv.end_date));
        let (mut f, mut u) = (false, false);
        if empty(&cv.status) {
            u = true;
        } else if cv.status.as_deref() != Some("active") {
            f = true;
        }
        if start.is_none() || end.is_none() {
            u = true;
        }
        for l in &c.lines {
            match day(&l.service_date) {
                None => u = true,
                Some(d) => {
                    if start.map_or(false, |s| d < s) || end.map_or(false, |e| d > e) {
                        f = true;
                    }
                }
            }
        }
        verdict(f, u, PASS)
    }

    fn r004(c: &Claim) -> u8 {
        let (mut f, mut u) = (false, false);
        for (a, b) in [(&c.patient_id, &c.coverage.beneficiary_patient_id), (&c.member_id, &c.coverage.member_id)] {
            if empty(a) || empty(b) {
                u = true;
            } else if a != b {
                f = true;
            }
        }
        verdict(f, u, PASS)
    }

    fn r005(&self, c: &Claim) -> u8 {
        match self.policy(c) {
            Some(p) if !empty(&c.provider_id) => {
                if p.allowed.contains(c.provider_id.as_ref().unwrap()) { PASS } else { FAIL }
            }
            _ => UNABLE,
        }
    }

    fn r006(c: &Claim) -> u8 {
        let mut seen: HashSet<(&str, &str, &str)> = HashSet::with_capacity(c.lines.len());
        let (mut dup, mut missing) = (false, false);
        for l in &c.lines {
            if empty(&l.service_code) || day(&l.service_date).is_none() {
                missing = true;
                continue;
            }
            let k = (l.service_code.as_deref().unwrap(), l.service_date.as_deref().unwrap(), l.modifier.as_deref().unwrap_or(""));
            if !seen.insert(k) {
                dup = true;
            }
        }
        verdict(dup, missing, PASS)
    }

    fn r007(c: &Claim) -> u8 {
        let (mut f, mut u) = (false, false);
        for l in &c.lines {
            match (l.quantity, l.unit_price, l.net_amount) {
                (Some(q), Some(p), Some(n)) => {
                    if (n - money(q * p)).abs() > cent() {
                        f = true;
                    }
                }
                _ => u = true,
            }
        }
        verdict(f, u, PASS)
    }

    fn r008(&self, c: &Claim) -> u8 {
        let Some(pol) = self.policy(c) else { return UNABLE };
        let (mut f, mut u, mut required) = (false, false, false);
        for l in &c.lines {
            if !self.known(&l.service_code) {
                u = true;
            } else if pol.auth_required.contains(l.service_code.as_ref().unwrap()) {
                required = true;
                if empty(&l.authorization_id) {
                    f = true;
                }
            }
        }
        verdict(f, u, if required { PASS } else { NA })
    }

    fn r009(&self, c: &Claim) -> u8 {
        let Some(pol) = self.policy(c) else { return UNABLE };
        let (mut f, mut u, mut required) = (false, false, false);
        for l in &c.lines {
            if !self.known(&l.service_code) {
                u = true;
                continue;
            }
            let code = l.service_code.as_ref().unwrap();
            if !pol.auth_required.contains(code) {
                continue;
            }
            required = true;
            if empty(&l.authorization_id) {
                u = true;
                continue;
            }
            let Some(rec) = Self::find_auth(c, l.authorization_id.as_ref().unwrap()) else {
                f = true;
                continue;
            };
            if empty(&rec.patient_id) {
                u = true;
            } else if rec.patient_id != c.patient_id {
                f = true;
            }
            if empty(&rec.service_code) {
                u = true;
            } else if rec.service_code.as_ref() != Some(code) {
                f = true;
            }
            if empty(&rec.status) {
                u = true;
            } else if rec.status.as_deref() != Some("approved") {
                f = true;
            }
            match (day(&l.service_date), day(&rec.valid_from), day(&rec.valid_to)) {
                (Some(d), Some(lo), Some(hi)) => {
                    if !(lo <= d && d <= hi) {
                        f = true
                    }
                }
                _ => u = true,
            }
        }
        let mut done: Vec<&str> = Vec::new();
        for l in &c.lines {
            if empty(&l.authorization_id) || !self.known(&l.service_code) || !pol.auth_required.contains(l.service_code.as_ref().unwrap()) {
                continue;
            }
            let aid = l.authorization_id.as_deref().unwrap();
            if done.contains(&aid) {
                continue;
            }
            done.push(aid);
            let Some(rec) = Self::find_auth(c, aid) else { continue };
            let mut sum = Decimal::ZERO;
            let mut missing = rec.max_quantity.is_none();
            for m in &c.lines {
                if m.authorization_id.as_deref() == Some(aid) {
                    match m.quantity {
                        Some(q) => sum += q,
                        None => missing = true,
                    }
                }
            }
            if missing {
                u = true;
            } else if sum > rec.max_quantity.unwrap() {
                f = true;
            }
        }
        verdict(f, u, if required { PASS } else { NA })
    }

    fn r010(&self, c: &Claim) -> u8 {
        let Some(pol) = self.policy(c) else { return UNABLE };
        let (mut f, mut u, mut required) = (false, false, false);
        for l in &c.lines {
            if !self.known(&l.service_code) {
                u = true;
                continue;
            }
            let code = l.service_code.as_ref().unwrap();
            let Some(need) = pol.required_documents.get(code) else { continue };
            required = true;
            let Some(d) = day(&l.service_date) else {
                u = true;
                continue;
            };
            let (mut hits, mut fin) = (0, false);
            for a in &c.attachments {
                if a.kind.as_ref() != Some(need) || a.patient_id.is_none() || a.patient_id != c.patient_id || a.service_code.as_ref() != Some(code) {
                    continue;
                }
                if day(&a.service_date) != Some(d) {
                    continue;
                }
                hits += 1;
                if a.document_status.as_deref() == Some("final") {
                    fin = true;
                }
            }
            if hits == 0 {
                f = true;
            } else if !fin {
                u = true;
            }
        }
        verdict(f, u, if required { PASS } else { NA })
    }

    fn r011(&self, c: &Claim) -> u8 {
        let (mut f, mut u) = (false, false);
        for l in &c.lines {
            if empty(&l.service_code) {
                u = true;
            } else if !self.services.contains(l.service_code.as_ref().unwrap()) {
                f = true;
            }
        }
        verdict(f, u, PASS)
    }

    fn r012(c: &Claim) -> u8 {
        let Some(total) = c.total_amount else { return UNABLE };
        let mut sum = Decimal::ZERO;
        for l in &c.lines {
            match l.net_amount {
                Some(n) => sum += n,
                None => return UNABLE,
            }
        }
        if (total - money(sum)).abs() > cent() { FAIL } else { PASS }
    }

    fn r013(&self, c: &Claim) -> u8 {
        let pol = self.policy(c);
        let (mut f, mut u) = (false, false);
        for l in &c.lines {
            let limits = match (pol, l.service_code.as_ref()) {
                (Some(p), Some(code)) if !empty(&l.service_code) => match (p.max_unit_price.get(code), p.max_quantity.get(code)) {
                    (Some(mp), Some(mq)) => Some((*mp, *mq)),
                    _ => None,
                },
                _ => None,
            };
            if l.quantity.is_none() || l.unit_price.is_none() || limits.is_none() {
                u = true;
            }
            if let Some(q) = l.quantity {
                if q <= Decimal::ZERO || q != q.trunc() {
                    f = true;
                }
                if let Some((_, mq)) = limits {
                    if q > mq {
                        f = true;
                    }
                }
            }
            if let Some(p) = l.unit_price {
                if p <= Decimal::ZERO {
                    f = true;
                }
                if let Some((mp, _)) = limits {
                    if p > mp {
                        f = true;
                    }
                }
            }
        }
        verdict(f, u, PASS)
    }

    fn r014(&self, c: &Claim) -> u8 {
        let (Some(pol), Some(sub)) = (self.policy(c), day(&c.submission_date)) else { return UNABLE };
        if c.lines.is_empty() {
            return UNABLE;
        }
        let mut latest = 0;
        for l in &c.lines {
            match day(&l.service_date) {
                Some(d) => latest = latest.max(d),
                None => return UNABLE,
            }
        }
        let lag = sub - latest;
        if lag < 0 {
            NA
        } else if lag > pol.window {
            FAIL
        } else {
            PASS
        }
    }

    fn r015(&self, c: &Claim) -> u8 {
        match self.policy(c) {
            Some(p) if !empty(&c.currency) => {
                if c.currency.as_deref() == Some(p.currency.as_str()) { PASS } else { FAIL }
            }
            _ => UNABLE,
        }
    }

    pub fn evaluate(&self, c: &Claim) -> [u8; 15] {
        [
            Self::r001(c),
            Self::r002(c),
            Self::r003(c),
            Self::r004(c),
            self.r005(c),
            Self::r006(c),
            Self::r007(c),
            self.r008(c),
            self.r009(c),
            self.r010(c),
            self.r011(c),
            Self::r012(c),
            self.r013(c),
            self.r014(c),
            self.r015(c),
        ]
    }
}
