//! One contract every `UserStore` must satisfy: the in-memory twin and the MongoDB store run the same checks, so the twin used by fast
//! tests is verified against the real database's behaviour (the Python build does the same with tests/access_store_contract.py).

use crate::store::{StoreError, User, UserStore, UserUpdate};
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn sample(badge: &str, level: i64) -> User {
    User {
        badge_id: badge.into(), name: "Sam".into(), password_hash: "h".into(), totp_secret_enc: "t".into(), level, grants: vec![], revokes: vec![], active: true,
        failed_attempts: 0, locked_until: None, must_change_password: false, created_by: "test".into(), created_at: 1.0, last_login: None,
    }
}

/// Panics with a message on the first violation. `fresh` must give an empty store each time.
pub fn run(fresh: &dyn Fn() -> Box<dyn UserStore>) {
    // create, read, duplicate, list in badge order
    let s = fresh();
    assert!(s.ping());
    s.create_user(&sample("CG-2002", 2)).unwrap();
    s.create_user(&sample("CG-1001", 1)).unwrap();
    assert_eq!(s.create_user(&sample("CG-2002", 3)), Err(StoreError::Duplicate), "one user per badge");
    assert_eq!(s.get_user("CG-2002").unwrap().unwrap().level, 2);
    assert!(s.get_user("CG-9999").unwrap().is_none());
    assert_eq!(s.list_users().unwrap().iter().map(|u| u.badge_id.as_str()).collect::<Vec<_>>(), ["CG-1001", "CG-2002"]);
    assert!(s.create_user(&sample("CG-1", 9)).is_err(), "invalid values are refused before storage");

    // operator-shaped text is just text: it never matches anything
    assert!(s.get_user("{\"$ne\": null}").unwrap().is_none());
    assert!(s.get_user("").unwrap().is_none());

    // updates change only what they name; clearing a lock works; unknown users are NotFound
    let u = s.update_user("CG-2002", &UserUpdate { level: Some(3), grants: Some(vec!["audit.view".into()]), must_change_password: Some(true), ..Default::default() }).unwrap();
    assert_eq!((u.level, u.grants.clone(), u.must_change_password, u.name.as_str()), (3, vec!["audit.view".to_string()], true, "Sam"));
    s.update_user("CG-2002", &UserUpdate { locked_until: Some(Some(99.0)), failed_attempts: Some(4), ..Default::default() }).unwrap();
    let u = s.update_user("CG-2002", &UserUpdate { locked_until: Some(None), failed_attempts: Some(0), ..Default::default() }).unwrap();
    assert_eq!((u.locked_until, u.failed_attempts), (None, 0));
    assert_eq!(s.update_user("CG-0000", &UserUpdate { active: Some(false), ..Default::default() }), Err(StoreError::NotFound));
    assert!(s.update_user("CG-2002", &UserUpdate { level: Some(7), ..Default::default() }).is_err());
    assert_eq!(s.update_user("CG-2002", &UserUpdate::default()).unwrap().badge_id, "CG-2002", "an empty update reads the user back");

    // lockout counting
    let s = fresh();
    s.create_user(&sample("CG-1001", 1)).unwrap();
    for i in 1..=2 {
        let u = s.record_failed_login("CG-1001", 100.0, 3, 60.0).unwrap();
        assert_eq!((u.failed_attempts, u.locked_until), (i, None));
    }
    assert_eq!(s.record_failed_login("CG-1001", 100.0, 3, 60.0).unwrap().locked_until, Some(160.0));
    assert_eq!(s.record_failed_login("CG-1001", 120.0, 3, 60.0).unwrap().locked_until, Some(160.0), "a lock is never extended");
    let u = s.record_failed_login("CG-1001", 170.0, 3, 60.0).unwrap();
    assert_eq!((u.failed_attempts, u.locked_until), (1, None), "the lock ran out: a new count starts");
    s.record_successful_login("CG-1001", 200.0).unwrap();
    let u = s.get_user("CG-1001").unwrap().unwrap();
    assert_eq!((u.failed_attempts, u.locked_until, u.last_login), (0, None, Some(200.0)));
    assert_eq!(s.record_failed_login("CG-0000", 1.0, 3, 60.0), Err(StoreError::NotFound));
    assert_eq!(s.record_successful_login("CG-0000", 1.0), Err(StoreError::NotFound));

    // parallel failures lose no count
    let s = fresh();
    s.create_user(&sample("CG-1001", 1)).unwrap();
    std::thread::scope(|t| {
        for _ in 0..8 {
            t.spawn(|| {
                for _ in 0..5 {
                    s.record_failed_login("CG-1001", 100.0, 1000, 60.0).unwrap();
                }
            });
        }
    });
    assert_eq!(s.get_user("CG-1001").unwrap().unwrap().failed_attempts, 40, "no failed attempt is lost");

    // a one-time code step is accepted once, even under 50 parallel submissions
    let s = fresh();
    assert!(s.mark_totp_used("CG-1", 5, 4_000_000_000.0).unwrap());
    assert!(!s.mark_totp_used("CG-1", 5, 4_000_000_000.0).unwrap());
    assert!(s.mark_totp_used("CG-1", 6, 4_000_000_000.0).unwrap() && s.mark_totp_used("CG-2", 5, 4_000_000_000.0).unwrap());
    let wins = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for _ in 0..50 {
            t.spawn(|| {
                if s.mark_totp_used("CG-9", 77, 4_000_000_000.0).unwrap() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(wins.load(Ordering::SeqCst), 1, "exactly one of 50 parallel submissions wins");

    // revocation lasts until the token would have expired, and only grows
    let s = fresh();
    s.revoke_token("j", 4_000_000_000.0).unwrap();
    s.revoke_token("j", 100.0).unwrap();
    assert!(s.is_revoked("j", 3_999_999_999.0).unwrap() && !s.is_revoked("j", 4_000_000_000.0).unwrap() && !s.is_revoked("other", 1.0).unwrap());
    std::thread::scope(|t| {
        for _ in 0..10 {
            t.spawn(|| s.revoke_token("same", 4_000_000_000.0).unwrap());
        }
    });
    assert!(s.is_revoked("same", 1.0).unwrap());

    // administrators are counted only while active
    let s = fresh();
    for b in ["CG-4001", "CG-4002"] {
        s.create_user(&sample(b, 4)).unwrap();
    }
    s.create_user(&sample("CG-1001", 1)).unwrap();
    s.update_user("CG-4002", &UserUpdate { active: Some(false), ..Default::default() }).unwrap();
    assert_eq!(s.count_active_admins().unwrap(), 1);
}
