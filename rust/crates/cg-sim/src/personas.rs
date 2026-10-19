//! The simulated reviewers. Each rule has a hidden true upheld rate; a reviewer's answer is the truth plus that person's bias and noise.
//! Everything here is invented for a demonstration: no real reviewer behaves like this.

use std::collections::BTreeMap;

pub const RULES: [&str; 15] = ["R001", "R002", "R003", "R004", "R005", "R006", "R007", "R008", "R009", "R010", "R011", "R012", "R013", "R014", "R015"];

#[derive(Debug, Clone)]
pub struct Persona {
    pub badge: String,
    pub senior: bool,
    /// Added to the probability of confirming (negative: dismisses more).
    pub bias: f64,
    /// Chance of answering at random instead of by the truth.
    pub noise: f64,
    /// Dismisses everything.
    pub adversarial: bool,
}

/// The hidden truth: most rules are upheld about 97 % of the time; R006 only 35 % and R013 50 %.
pub fn truth() -> BTreeMap<&'static str, f64> {
    let mut t: BTreeMap<&'static str, f64> = RULES.iter().map(|r| (*r, 0.97)).collect();
    t.insert("R006", 0.35);
    t.insert("R013", 0.50);
    t
}

/// Twelve people: eight juniors (careful, lenient, noisy, slow) and four seniors, one of whom can be switched to the adversarial persona.
pub fn default_reviewers(with_adversary: bool) -> Vec<Persona> {
    let mut v = vec![];
    for i in 0..8 {
        let (bias, noise) = match i % 4 { 0 => (0.0, 0.02), 1 => (0.05, 0.03), 2 => (-0.05, 0.03), _ => (0.0, 0.15) };
        v.push(Persona { badge: format!("SIM-2{i:03}"), senior: false, bias, noise, adversarial: false });
    }
    for i in 0..4 {
        v.push(Persona { badge: format!("SIM-3{i:03}"), senior: true, bias: 0.0, noise: 0.02, adversarial: with_adversary && i == 0 });
    }
    v
}
