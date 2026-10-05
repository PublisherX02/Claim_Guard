"""The claim state machine: the whole 9 x 9 table is checked against a literal copy, so a typo in the module shows."""
import itertools
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from hypothesis import given, strategies as st
from workqueue import states

ALL = ('received', 'triaged', 'explained', 'explanation_skipped', 'ready', 'leased', 'decided', 'rechecked', 'dead_lettered')
ALLOWED = {
    'received': {'triaged', 'dead_lettered'},
    'triaged': {'explained', 'explanation_skipped', 'ready', 'dead_lettered'},
    'explained': {'ready', 'dead_lettered'},
    'explanation_skipped': {'ready', 'dead_lettered'},
    'ready': {'leased', 'dead_lettered'},
    'leased': {'ready', 'decided', 'dead_lettered'},
    'decided': {'rechecked'},
    'rechecked': {'triaged'},
    'dead_lettered': {'triaged'},
}


class TableTests(unittest.TestCase):
    def test_the_state_names(self):
        self.assertEqual(states.STATES, ALL)

    def test_every_pair_matches_the_literal_table(self):
        for frm, to in itertools.product(ALL, ALL):
            if to in ALLOWED[frm]:
                self.assertIsNone(states.check_transition(frm, to), (frm, to))
            else:
                with self.assertRaises(states.IllegalTransition, msg=(frm, to)):
                    states.check_transition(frm, to)

    def test_decided_can_only_be_rechecked(self):
        self.assertEqual({t for t in ALL if t in states.TRANSITIONS['decided']}, {'rechecked'})

    def test_a_claim_is_never_decided_without_having_been_leased(self):
        for frm in ALL:
            if frm != 'leased':
                with self.assertRaises(states.IllegalTransition):
                    states.check_transition(frm, 'decided')

    def test_unknown_names_and_wrong_types_raise(self):
        for frm, to in (('nope', 'ready'), ('ready', 'nope'), (None, 'ready'), ('ready', None), (1, 2), (['ready'], 'leased'),
                        ({'$ne': None}, 'leased')):
            with self.assertRaises(states.IllegalTransition, msg=(frm, to)):
                states.check_transition(frm, to)

    def test_the_table_is_immutable(self):
        with self.assertRaises((AttributeError, TypeError)):
            states.TRANSITIONS['decided'].add('ready')
        with self.assertRaises(TypeError):
            states.TRANSITIONS['decided'] = frozenset()

    @given(st.lists(st.integers(0, 8), max_size=40))
    def test_any_walk_through_allowed_transitions_stays_inside_the_states(self, picks):
        state = 'received'
        for p in picks:
            options = sorted(states.TRANSITIONS[state])
            if not options:
                break
            nxt = options[p % len(options)]
            states.check_transition(state, nxt)
            state = nxt
            self.assertIn(state, states.STATES)


class EventTests(unittest.TestCase):
    def event(self, **kw):
        args = dict(frm='ready', to='leased', actor='B1', now=5.0, detail=None)
        args.update(kw)
        return states.make_event(args['frm'], args['to'], args['actor'], args['now'], args['detail'])

    def test_an_event_records_both_states_the_actor_and_the_time(self):
        self.assertEqual(self.event(detail={'seed': 7, 'ok': True, 'why': 'x', 'ids': ['a']}),
                         {'from': 'ready', 'to': 'leased', 'actor': 'B1', 'at': 5.0, 'detail': {'seed': 7, 'ok': True, 'why': 'x', 'ids': ['a']}})
        self.assertEqual(self.event()['detail'], {})

    def test_an_event_for_an_illegal_move_is_refused(self):
        with self.assertRaises(states.IllegalTransition):
            self.event(frm='decided', to='ready')

    def test_the_actor_must_be_non_empty_text(self):
        for bad in ('', None, 5, ['B1']):
            with self.assertRaises(ValueError, msg=repr(bad)):
                self.event(actor=bad)
        self.assertEqual(self.event(actor='system:worker-1')['actor'], 'system:worker-1')

    def test_detail_must_be_small_plain_data(self):
        deep = {'a': {'b': {'c': {'d': 1}}}}
        for bad in (deep, {'x': object()}, {'x': {1, 2}}, {'x': float('nan')}, 'text', {1: 'x'}, {'x': b'bytes'}):
            with self.assertRaises(ValueError, msg=repr(bad)):
                self.event(detail=bad)
        self.event(detail={'a': {'b': {'c': 1}}})

    def test_the_time_must_be_a_number(self):
        for bad in (None, '5', True):
            with self.assertRaises(ValueError, msg=repr(bad)):
                self.event(now=bad)


if __name__ == '__main__':
    unittest.main()
