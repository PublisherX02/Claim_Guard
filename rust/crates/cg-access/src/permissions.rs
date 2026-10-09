//! Clearance levels and permission flags (`src/access/permissions.py`).
//!
//! A level is a default set of flags. A user may additionally hold per-user grants and revokes, but the sensitive flags (user management,
//! audit access and queue administration) can only come from level 4, and level 4 deliberately cannot decide claims: whoever administers
//! the system is not the person who approves its findings (separation of duties).

use std::collections::BTreeSet;

pub const PERMISSIONS: [&str; 11] = [
    "claims.view", "claims.view_notes", "claims.decide", "claims.decide_high", "claims.recheck", "pii.unmask", "audit.view", "audit.verify", "users.manage",
    "routing.manage", "queue.view",
];
pub const SENSITIVE: [&str; 5] = ["users.manage", "audit.view", "audit.verify", "routing.manage", "queue.view"];
pub const ADMIN_LEVEL: i64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("{0}")]
    Denied(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Perms = BTreeSet<&'static str>;

fn level_defaults(level: i64) -> Option<Perms> {
    let l1: Perms = ["claims.view"].into_iter().collect();
    let mut l2 = l1.clone();
    l2.extend(["claims.view_notes", "claims.decide", "claims.recheck", "pii.unmask"]);
    let mut l3 = l2.clone();
    l3.insert("claims.decide_high");
    let l4: Perms = SENSITIVE.into_iter().collect();
    match level {
        1 => Some(l1),
        2 => Some(l2),
        3 => Some(l3),
        4 => Some(l4),
        _ => None,
    }
}

fn names(list: &[String]) -> Result<Vec<&'static str>, PermissionError> {
    list.iter()
        .map(|n| PERMISSIONS.iter().copied().find(|p| p == n).ok_or_else(|| PermissionError::Invalid("unknown permission".into())))
        .collect()
}

/// Level defaults, plus grants, minus revokes (a revoke wins over a grant of the same flag).
pub fn effective_permissions(level: i64, grants: &[String], revokes: &[String]) -> Result<Perms, PermissionError> {
    let mut base = level_defaults(level).ok_or_else(|| PermissionError::Invalid("level must be one of 1, 2, 3, 4".into()))?;
    let mut granted = names(grants)?;
    let revoked = names(revokes)?;
    if level == ADMIN_LEVEL {
        granted.retain(|g| SENSITIVE.contains(g)); // a stored claim grant on an administrator is never honoured
    }
    base.extend(granted);
    for r in revoked {
        base.remove(r);
    }
    Ok(base)
}

/// Err unless this actor may make this change.
pub fn check_user_change(actor_level: i64, target_level: i64, new_level: i64, grants: &[String], revokes: &[String], self_change: bool) -> Result<(), PermissionError> {
    for l in [actor_level, target_level, new_level] {
        level_defaults(l).ok_or_else(|| PermissionError::Invalid("level must be one of 1, 2, 3, 4".into()))?;
    }
    let granted = names(grants)?;
    names(revokes)?;
    let deny = |m: &str| Err(PermissionError::Denied(m.into()));
    if actor_level != ADMIN_LEVEL {
        return deny("only level 4 may manage users");
    }
    if new_level > actor_level {
        return deny("cannot set a level above your own");
    }
    if self_change && new_level != target_level {
        return deny("cannot change your own level");
    }
    if new_level != ADMIN_LEVEL && granted.iter().any(|g| SENSITIVE.contains(g)) {
        return deny("user management and audit access can only come from level 4");
    }
    if new_level == ADMIN_LEVEL && granted.iter().any(|g| !SENSITIVE.contains(g)) {
        return deny("a level 4 administrator cannot hold claim-handling permissions (separation of duties)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn levels_are_nested_and_admin_cannot_decide() {
        let p = |l| effective_permissions(l, &[], &[]).unwrap();
        assert!(p(1).is_subset(&p(2)) && p(2).is_subset(&p(3)));
        assert!(p(3).contains("claims.decide_high") && !p(2).contains("claims.decide_high"));
        assert!(p(4).contains("audit.view") && !p(4).contains("claims.decide") && !p(4).contains("pii.unmask"));
        assert!(!p(1).contains("claims.decide"));
    }

    #[test]
    fn grants_add_revokes_win_and_admin_keeps_separation() {
        let g = effective_permissions(1, &v(&["claims.decide"]), &[]).unwrap();
        assert!(g.contains("claims.decide"));
        assert!(!effective_permissions(2, &v(&["claims.decide"]), &v(&["claims.decide"])).unwrap().contains("claims.decide"));
        assert!(!effective_permissions(4, &v(&["claims.decide"]), &[]).unwrap().contains("claims.decide"), "never honoured on level 4");
        assert!(effective_permissions(4, &[], &v(&["audit.view"])).unwrap().len() < 5);
    }

    #[test]
    fn unknown_levels_and_names_are_refused() {
        assert!(effective_permissions(0, &[], &[]).is_err());
        assert!(effective_permissions(5, &[], &[]).is_err());
        assert!(effective_permissions(2, &v(&["root"]), &[]).is_err());
        assert!(effective_permissions(2, &[], &v(&["root"])).is_err());
    }

    #[test]
    fn who_may_change_whom() {
        let ok = |a, t, n, g: &[&str], s| check_user_change(a, t, n, &v(g), &[], s);
        assert!(ok(4, 2, 3, &[], false).is_ok());
        assert!(ok(3, 2, 2, &[], false).is_err(), "only level 4 manages users");
        assert!(ok(4, 4, 3, &[], true).is_err(), "cannot demote yourself");
        assert!(ok(4, 2, 2, &["audit.view"], false).is_err(), "sensitive grants only on level 4");
        assert!(ok(4, 4, 4, &["claims.decide"], false).is_err(), "no claim handling for an administrator");
        assert!(ok(4, 2, 5, &[], false).is_err());
        assert!(ok(4, 4, 4, &["audit.view"], false).is_ok());
    }
}
