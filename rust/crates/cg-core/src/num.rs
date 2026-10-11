//! JSON numbers with Python's semantics.
//!
//! Python's json module reads `3` as an int of any size and `3.0` or `1e2` as a float (binary, 64 bit). The rules add, compare and
//! convert those values, and their results depend on which kind each one is (0.1 + 0.2 is not 0.3; an int is exact). `Num` keeps the
//! two kinds apart so the Rust engine reaches the same verdicts as the Python one, including on the awkward inputs.

use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use serde_json::Value;
use std::cmp::Ordering;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq)]
pub enum Num {
    Int(BigInt),
    Float(f64),
}

impl Num {
    /// `facts_extractor._clean_num`: a JSON number (never a bool or a string) that is finite; anything else is unknown.
    pub fn from_json(v: &Value) -> Option<Num> {
        match v {
            Value::Number(n) => Num::from_text(&n.to_string()),
            _ => None,
        }
    }

    /// Reads the text of a JSON number the way `json.loads` does: no dot or exponent means int, otherwise float.
    pub fn from_text(text: &str) -> Option<Num> {
        if text.contains(['.', 'e', 'E']) {
            let f: f64 = text.parse().ok()?;
            f.is_finite().then_some(Num::Float(f))
        } else {
            BigInt::from_str(text).ok().map(Num::Int)
        }
    }

    pub fn int(i: i64) -> Num {
        Num::Int(BigInt::from(i))
    }

    /// `Decimal(str(x))`: the exact value of the shortest text Python would print for this number.
    pub fn to_decimal(&self) -> BigDecimal {
        match self {
            Num::Int(i) => BigDecimal::from(i.clone()),
            // Rust prints the shortest text that round-trips, never with an exponent: the same digits as Python's repr.
            Num::Float(f) => BigDecimal::from_str(&format!("{f}")).unwrap_or_else(|_| BigDecimal::zero()),
        }
    }

    pub fn is_zero_or_negative(&self) -> bool {
        match self {
            Num::Int(i) => !i.is_positive(),
            Num::Float(f) => *f <= 0.0,
        }
    }

    /// `facts_extractor._whole_number`: 3 and 3.0 yes; 1.5 no.
    pub fn is_whole(&self) -> bool {
        match self {
            Num::Int(_) => true,
            Num::Float(f) => f.fract() == 0.0,
        }
    }

    /// Python's `a + b`: int + int is exact, anything with a float is a float.
    pub fn add(&self, other: &Num) -> Num {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => Num::Int(a + b),
            _ => Num::Float(self.to_f64() + other.to_f64()),
        }
    }

    pub fn to_f64(&self) -> f64 {
        match self {
            Num::Int(i) => i.to_f64().unwrap_or(f64::INFINITY * if i.is_negative() { -1.0 } else { 1.0 }),
            Num::Float(f) => *f,
        }
    }

    /// Python's comparison of numbers: exact, also between an int and a float.
    pub fn compare(&self, other: &Num) -> Ordering {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a.cmp(b),
            (Num::Float(a), Num::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Num::Int(i), Num::Float(f)) => cmp_int_float(i, *f),
            (Num::Float(f), Num::Int(i)) => cmp_int_float(i, *f).reverse(),
        }
    }

    pub fn gt(&self, other: &Num) -> bool {
        self.compare(other) == Ordering::Greater
    }
}

fn cmp_int_float(i: &BigInt, f: f64) -> Ordering {
    if f.fract() == 0.0 {
        match BigInt::from_f64(f) {
            Some(fi) => i.cmp(&fi),
            None => Ordering::Equal,
        }
    } else {
        // i against a fraction: compare with the floor; equal floors mean the float is larger.
        let floor = BigInt::from_f64(f.floor()).unwrap_or_else(BigInt::zero);
        if *i <= floor {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}

/// `Decimal.quantize(Decimal('0.01'), ROUND_HALF_UP)`: round to cents, ties away from zero, exactly.
pub fn cents(d: &BigDecimal) -> BigDecimal {
    d.with_scale_round(2, bigdecimal::RoundingMode::HalfUp)
}

/// The text of a quantized decimal as Python prints it (two decimals, no exponent).
pub fn plain(d: &BigDecimal) -> String {
    d.to_plain_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_and_floats_are_told_apart() {
        assert_eq!(Num::from_text("3"), Some(Num::int(3)));
        assert_eq!(Num::from_text("3.0"), Some(Num::Float(3.0)));
        assert_eq!(Num::from_text("1e2"), Some(Num::Float(100.0)));
        assert_eq!(Num::from_text("1e999"), None, "an infinite float is unknown, as in _clean_num");
        assert_eq!(Num::from_text("123456789012345678901234567890"), Some(Num::Int("123456789012345678901234567890".parse().unwrap())));
    }

    #[test]
    fn float_addition_is_binary_like_python() {
        let sum = Num::Float(0.1).add(&Num::Float(0.2));
        assert!(sum.gt(&Num::Float(0.3)), "0.1 + 0.2 > 0.3 in Python, and so here");
        assert_eq!(Num::int(2).add(&Num::int(3)), Num::int(5));
        assert_eq!(Num::int(2).add(&Num::Float(0.5)), Num::Float(2.5));
    }

    #[test]
    fn mixed_comparisons_are_exact() {
        assert_eq!(Num::int(3).compare(&Num::Float(3.0)), Ordering::Equal);
        assert_eq!(Num::int(3).compare(&Num::Float(3.5)), Ordering::Less);
        assert_eq!(Num::Float(2.5).compare(&Num::int(3)), Ordering::Less);
        assert_eq!(Num::Float(-0.5).compare(&Num::int(0)), Ordering::Less);
        let big = Num::Int("9007199254740993".parse().unwrap()); // 2**53 + 1: not representable as a float
        assert_eq!(big.compare(&Num::Float(9007199254740992.0)), Ordering::Greater);
    }

    #[test]
    fn decimal_of_a_float_uses_its_shortest_text() {
        assert_eq!(plain(&Num::Float(0.1).to_decimal()), "0.1");
        assert_eq!(plain(&Num::Float(0.335).to_decimal()), "0.335");
        assert_eq!(plain(&Num::int(180).to_decimal()), "180");
        assert_eq!(plain(&Num::Float(1e16).to_decimal()), "10000000000000000");
    }

    #[test]
    fn rounding_to_cents_is_half_away_from_zero() {
        let d = |s: &str| BigDecimal::from_str(s).unwrap();
        assert_eq!(plain(&cents(&d("0.675"))), "0.68");
        assert_eq!(plain(&cents(&d("0.665"))), "0.67");
        assert_eq!(plain(&cents(&d("-0.005"))), "-0.01");
        assert_eq!(plain(&cents(&d("2"))), "2.00");
    }

    #[test]
    fn whole_numbers() {
        assert!(Num::int(3).is_whole());
        assert!(Num::Float(3.0).is_whole());
        assert!(!Num::Float(1.5).is_whole());
    }
}
