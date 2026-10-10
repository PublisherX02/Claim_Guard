//! Side channel check: can the answer to a refused sign-in be told apart by how long it takes?
//!
//! Every kind of refusal (unknown badge, malformed badge, locked account, inactive account, throttled client, wrong password, wrong or
//! replayed authenticator code) goes through `AccessService::login`; this test times each many times with the real password hash cost and
//! compares the medians. A group that was clearly faster or slower than the others would let a stranger learn which badges exist, which are
//! locked, or whether a guessed password was right. Timing is noisy, so the bound is generous (25 %); the point is to catch a path that
//! skips the password hash, which is a factor of ten to a thousand. Run with:  cargo test --release -p cg-access --test timing -- --ignored --nocapture

use cg_access::securitylog::SecurityLog;
use cg_access::service::{AccessService, PAIR_LIMIT};
use cg_access::settings::test_settings;
use cg_access::store::{MemoryStore, UserStore, UserUpdate};
use cg_access::{totp, Clock};
use std::sync::Arc;
use std::time::Instant;

const PW: &str = "Correct-Horse-9";
const SAMPLES: usize = 40;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore = "takes about a minute: it hashes passwords at the production cost"]
fn every_kind_of_refusal_takes_about_as_long() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = test_settings();
    settings.bcrypt_rounds = 10;
    let log = Arc::new(SecurityLog::open(dir.path().join("sec.jsonl"), &settings.audit_anchor_key).unwrap());
    let store = Arc::new(MemoryStore::new());
    let clock: Clock = Arc::new(|| 1_700_000_000.0);
    let svc = AccessService::new(store.clone(), settings, log, clock);
    let mut seed = String::new();
    for badge in ["CG-2002", "CG-2003", "CG-2004"] {
        let (_, uri) = svc.provision_user(badge, "Test", PW, 2, "test", &[], &[], false).unwrap();
        if badge == "CG-2002" {
            seed = uri.split("secret=").nth(1).unwrap().split('&').next().unwrap().to_string();
        }
    }
    store.update_user("CG-2003", &UserUpdate { locked_until: Some(Some(1_700_000_000.0 + 10_000.0)), ..Default::default() }).unwrap();
    store.update_user("CG-2004", &UserUpdate { active: Some(false), ..Default::default() }).unwrap();
    let step = totp::current_step(1_700_000_000.0, 30);
    let good_code = totp::code_at(&seed, step).unwrap();
    // a code that was valid and is already used, for the replay path
    assert!(svc.login("CG-2002", PW, &good_code, "known-client").is_ok());

    // The groups are sampled in turn (one of each, then the next round) so that a drift in machine speed hits all of them equally.
    type Attempt<'a> = Box<dyn Fn(usize) + 'a>;
    let attempts: Vec<(&str, Attempt)> = vec![
        ("unknown badge", Box::new(|i| drop(svc.login(&format!("CG-7{i:03}"), PW, "000000", &format!("c{i}"))))),
        ("malformed badge", Box::new(|i| drop(svc.login(&format!("not a badge {i}"), PW, "000000", &format!("c{i}"))))),
        ("locked account, new client", Box::new(|i| drop(svc.login("CG-2003", PW, &good_code, &format!("c{i}"))))),
        ("inactive account", Box::new(|i| drop(svc.login("CG-2004", PW, &good_code, &format!("c{i}"))))),
        ("wrong password", Box::new(|i| drop(svc.login("CG-2002", "Wrong-Password-1x", &good_code, &format!("w{i}"))))),
        ("right password, wrong code", Box::new(|i| drop(svc.login("CG-2002", PW, "000000", &format!("x{i}"))))),
        ("right password, replayed code", Box::new(|i| drop(svc.login("CG-2002", PW, &good_code, &format!("r{i}"))))),
        ("throttled client", Box::new(|_| drop(svc.login("CG-2002", PW, &good_code, "hammering-client")))),
    ];
    for _ in 0..PAIR_LIMIT + 1 {
        let _ = svc.login("CG-2002", "Wrong-Password-1x", "000000", "hammering-client"); // so that this client really is throttled
    }
    let mut groups: Vec<(&str, Vec<f64>)> = attempts.iter().map(|(n, _)| (*n, vec![])).collect();
    for i in 0..SAMPLES {
        for (g, (_, f)) in groups.iter_mut().zip(attempts.iter()) {
            let t = Instant::now();
            f(i);
            g.1.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let medians: Vec<(&str, f64)> = groups.iter().map(|(n, v)| (*n, median(v.clone()))).collect();
    for (n, m) in &medians {
        println!("{n:32} median {m:8.2} ms");
    }
    let (lo, hi) = medians.iter().fold((f64::MAX, 0.0_f64), |(lo, hi), (_, m)| (lo.min(*m), hi.max(*m)));
    assert!(hi / lo < 1.25, "the slowest refusal is {:.0} % slower than the fastest: {medians:?}", (hi / lo - 1.0) * 100.0);
}
