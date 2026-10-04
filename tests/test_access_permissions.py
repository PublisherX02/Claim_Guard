import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import permissions as p

# The spec table, written out literally so a typo in the module cannot hide itself.
L1 = {'claims.view'}
L2 = {'claims.view', 'claims.view_notes', 'claims.decide', 'claims.recheck', 'pii.unmask'}
L3 = L2 | {'claims.decide_high'}
L4 = {'audit.view', 'audit.verify', 'users.manage'}


class LevelTableTests(unittest.TestCase):
    def test_each_level_matches_the_specification_row_by_row(self):
        self.assertEqual(p.LEVEL_DEFAULTS[1], L1)
        self.assertEqual(p.LEVEL_DEFAULTS[2], L2)
        self.assertEqual(p.LEVEL_DEFAULTS[3], L3)
        self.assertEqual(p.LEVEL_DEFAULTS[4], L4)
        self.assertEqual(set(p.LEVEL_DEFAULTS), {1, 2, 3, 4})

    def test_every_listed_permission_is_known_and_none_is_missing(self):
        used = set().union(*p.LEVEL_DEFAULTS.values())
        self.assertEqual(used, set(p.PERMISSIONS))

    def test_separation_of_duties_admin_cannot_decide_claims(self):
        for perm in ('claims.decide', 'claims.decide_high', 'claims.recheck', 'claims.view', 'pii.unmask'):
            self.assertNotIn(perm, p.LEVEL_DEFAULTS[4])

    def test_only_level_three_decides_high_severity(self):
        for level in (1, 2, 4):
            self.assertNotIn('claims.decide_high', p.LEVEL_DEFAULTS[level])
        self.assertIn('claims.decide_high', p.LEVEL_DEFAULTS[3])

    def test_levels_are_immutable(self):
        with self.assertRaises(AttributeError):
            p.LEVEL_DEFAULTS[2].add('users.manage')


class EffectiveTests(unittest.TestCase):
    def test_defaults_when_no_overrides(self):
        self.assertEqual(p.effective_permissions(2, (), ()), frozenset(L2))

    def test_grant_adds_and_revoke_removes(self):
        self.assertIn('claims.decide_high', p.effective_permissions(2, ('claims.decide_high',), ()))
        self.assertNotIn('pii.unmask', p.effective_permissions(2, (), ('pii.unmask',)))

    def test_revoke_wins_over_grant_of_the_same_permission(self):
        self.assertNotIn('pii.unmask', p.effective_permissions(1, ('pii.unmask',), ('pii.unmask',)))

    def test_unknown_permission_or_level_raises(self):
        for args in ((2, ('claims.fly',), ()), (2, (), ('nope',)), (0, (), ()), (5, (), ()), (True, (), ()), ('2', (), ()), (None, (), ())):
            with self.assertRaises(ValueError, msg=repr(args)):
                p.effective_permissions(*args)


class UserChangeTests(unittest.TestCase):
    def change(self, **kw):
        base = dict(actor_level=4, target_level=2, new_level=2, grants=(), revokes=(), self_change=False)
        base.update(kw)
        return p.check_user_change(**base)

    def test_a_plain_change_is_allowed(self):
        self.assertIsNone(self.change())

    def test_cannot_set_a_level_above_your_own(self):
        with self.assertRaises(p.PermissionDenied):
            self.change(actor_level=3, new_level=4)

    def test_only_level_four_may_manage_users_at_all(self):
        with self.assertRaises(p.PermissionDenied):
            self.change(actor_level=3)

    def test_cannot_change_your_own_level(self):
        with self.assertRaises(p.PermissionDenied):
            self.change(self_change=True, target_level=4, new_level=3)

    def test_sensitive_permissions_only_go_to_level_four(self):
        for perm in ('users.manage', 'audit.view', 'audit.verify'):
            with self.assertRaises(p.PermissionDenied, msg=perm):
                self.change(new_level=2, grants=(perm,))
            self.assertIsNone(self.change(target_level=4, new_level=4, grants=(perm,)))

    def test_unknown_grant_or_revoke_is_a_value_error(self):
        with self.assertRaises(ValueError):
            self.change(grants=('claims.fly',))
        with self.assertRaises(ValueError):
            self.change(revokes=('nope',))


if __name__ == '__main__':
    unittest.main()
