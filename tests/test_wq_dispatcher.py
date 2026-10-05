"""The dispatcher: pure planning (worked example, aging, conflicts, fairness, an independent invariant oracle) and the stateful
dealing on the in-memory store and, when configured, MongoDB (races, reproducible deals, expiry, shortages, configuration changes)."""
import dataclasses
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from hypothesis import HealthCheck, given, settings, strategies as st
from workqueue import routing_config as rc
from workqueue.dispatcher import Agent, Dispatcher, plan_deal
from oracle_dispatch import check_plan, spread_of_points
from queue_store_contract import race
from wq_world import World, put_ready, set_cfg, store_makers

NOW = 100000.0
CFG = rc.DEFAULT
L2, L3 = frozenset({'claims.decide'}), frozenset({'claims.decide', 'claims.decide_high'})


def snap(claim_id, score, elig='decide_high', patient='P', state_at=NOW, version=1, prior=(), avoid=()):
    return {'claim_id': claim_id, 'version': version, 'state_at': state_at, 'receipt': {'score': score, 'eligibility': elig},
            'claim': {'patient_id': patient}, 'prior_deciders': tuple(prior), 'avoid': tuple(avoid)}


def cfg_with(**kw):
    return dataclasses.replace(CFG, **kw)


class PlanTests(unittest.TestCase):
    def test_worked_example_three_agents_nine_claims(self):
        seed, scores = 7, [12, 11, 10, 9, 8, 7, 6, 5, 4]
        cfg = cfg_with(on_shift=('A', 'B', 'C'), aging_per_hour=0)
        ready = [snap(f'C{i}', s) for i, s in enumerate(scores)]
        rng = random.Random(seed)
        rng.shuffle(list(ready))                                # the algorithm shuffles the claims first, then the agents
        pool = ['A', 'B', 'C']
        rng.shuffle(pool)
        r1, r2, r3 = pool
        plan = plan_deal(ready, [Agent(b, L3) for b in 'ABC'], {}, cfg, seed, NOW)
        # claims by descending score; each to the least-loaded agent (loads 12/11/10, then 19/19/19, then 25/24/23)
        self.assertEqual(plan, [('C0', r1, 1), ('C1', r2, 1), ('C2', r3, 1), ('C3', r3, 1), ('C4', r2, 1), ('C5', r1, 1),
                                ('C6', r1, 1), ('C7', r2, 1), ('C8', r3, 1)])
        check_plan(ready, [Agent(b, L3) for b in 'ABC'], {}, cfg, plan)

    def test_same_inputs_and_seed_give_the_same_plan_and_another_seed_changes_ties(self):
        ready = [snap(f'C{i}', 4) for i in range(12)]
        agents = [Agent(f'B{i}', L3) for i in range(4)]
        cfg = cfg_with(on_shift=tuple(a.badge_id for a in agents))
        a = plan_deal(ready, agents, {}, cfg, 1, NOW)
        self.assertEqual(a, plan_deal(ready, agents, {}, cfg, 1, NOW))
        self.assertNotEqual(a, plan_deal(ready, agents, {}, cfg, 2, NOW))

    def test_a_high_claim_goes_only_to_an_agent_who_may_decide_it(self):
        cfg = cfg_with(on_shift=('L2', 'L2G', 'L3'))
        ready = [snap('H1', 8)]
        self.assertEqual(plan_deal(ready, [Agent('L2', L2)], {}, cfg, 1, NOW), [])
        self.assertEqual(plan_deal(ready, [Agent('L2', L2), Agent('L2G', L3)], {}, cfg, 1, NOW), [('H1', 'L2G', 1)])
        self.assertEqual(plan_deal([snap('M1', 2, 'decide')], [Agent('L2', L2)], {}, cfg, 1, NOW), [('M1', 'L2', 1)])

    def test_nobody_on_shift_inactive_or_full_means_an_empty_plan(self):
        ready = [snap('C1', 4, 'decide')]
        full = {'B1': {'count': CFG.slice_size, 'points': 40}}
        self.assertEqual(plan_deal(ready, [Agent('B1', L3)], {}, cfg_with(on_shift=()), 1, NOW), [])
        self.assertEqual(plan_deal(ready, [Agent('B1', L3, active=False)], {}, cfg_with(on_shift=('B1',)), 1, NOW), [])
        self.assertEqual(plan_deal(ready, [Agent('B1', L3)], full, cfg_with(on_shift=('B1',)), 1, NOW), [])
        self.assertEqual(plan_deal([], [Agent('B1', L3)], {}, cfg_with(on_shift=('B1',)), 1, NOW), [])

    def test_a_long_wait_outranks_a_fresh_high_score(self):
        cfg = cfg_with(on_shift=('B1',), slice_size=1, low_water=1)
        old = snap('OLD', 2, 'decide', state_at=NOW - 40 * 3600)              # 2 + 0.5 * 40 = 22
        fresh = snap('NEW', 12, 'decide')
        self.assertEqual(plan_deal([fresh, old], [Agent('B1', L2)], {}, cfg, 3, NOW), [('OLD', 'B1', 1)])
        self.assertEqual(plan_deal([fresh, old], [Agent('B1', L2)], {}, cfg_with(on_shift=('B1',), slice_size=1, low_water=1,
                                                                            aging_per_hour=0), 3, NOW), [('NEW', 'B1', 1)])

    def test_an_agent_never_receives_a_later_version_of_a_claim_they_decided(self):
        cfg = cfg_with(on_shift=('B1', 'B2'))
        ready = [snap('C1', 4, version=2, prior=('B1',))]
        for seed in range(30):
            self.assertEqual(plan_deal(ready, [Agent('B1', L3), Agent('B2', L3)], {}, cfg, seed, NOW), [('C1', 'B2', 2)])
        self.assertEqual(plan_deal(ready, [Agent('B1', L3)], {}, cfg, 1, NOW), [])

    def test_an_excluded_badge_and_patient_pair_never_meets(self):
        cfg = cfg_with(on_shift=('B1', 'B2'), exclusions=(('B1', 'PX'),))
        ready = [snap(f'C{i}', 4, patient='PX') for i in range(6)] + [snap('Z', 4, patient='PY')]
        plan = plan_deal(ready, [Agent('B1', L3), Agent('B2', L3)], {}, cfg, 5, NOW)
        self.assertTrue(all(b == 'B2' for c, b, v in plan if c != 'Z'))
        check_plan(ready, [Agent('B1', L3), Agent('B2', L3)], {}, cfg, plan)

    def test_a_handed_back_claim_goes_to_someone_else_when_there_is_someone_else(self):
        cfg = cfg_with(on_shift=('B1', 'B2'))
        ready = [snap('C1', 4, avoid=('B1',))]
        for seed in range(30):
            self.assertEqual(plan_deal(ready, [Agent('B1', L3), Agent('B2', L3)], {}, cfg, seed, NOW), [('C1', 'B2', 1)])
        self.assertEqual(plan_deal(ready, [Agent('B1', L3)], {}, cfg, 1, NOW), [('C1', 'B1', 1)])      # only B1 exists

    def test_existing_work_counts_against_an_agent(self):
        cfg = cfg_with(on_shift=('B1', 'B2'))
        loads = {'B1': {'count': 5, 'points': 40}, 'B2': {'count': 1, 'points': 2}}
        plan = plan_deal([snap('C1', 4)], [Agent('B1', L3), Agent('B2', L3)], loads, cfg, 1, NOW)
        self.assertEqual(plan, [('C1', 'B2', 1)])

    def test_capacity_is_respected(self):
        cfg = cfg_with(on_shift=('B1',), slice_size=3, low_water=1)
        plan = plan_deal([snap(f'C{i}', 4) for i in range(10)], [Agent('B1', L3)], {'B1': {'count': 1, 'points': 4}}, cfg, 1, NOW)
        self.assertEqual(len(plan), 2)
        self.assertEqual(len(plan_deal([snap(f'C{i}', 4) for i in range(10)], [Agent('B1', L3)], {}, cfg, 1, NOW, max_per_agent=1)), 1)

    def test_a_duplicate_agent_is_refused(self):
        with self.assertRaises(ValueError):
            plan_deal([snap('C1', 1)], [Agent('B1', L3), Agent('B1', L2)], {}, cfg_with(on_shift=('B1',)), 1, NOW)


BADGES = ['B1', 'B2', 'B3', 'B4', 'B5']


@st.composite
def worlds(draw):
    slice_size = draw(st.integers(1, 6))
    n = draw(st.integers(0, 5))
    agents = [Agent(BADGES[i], draw(st.sampled_from([L2, L3, frozenset()])), draw(st.booleans())) for i in range(n)]
    on_shift = tuple(b for b in BADGES[:n] if draw(st.booleans()))
    pairs = tuple((draw(st.sampled_from(BADGES)), draw(st.sampled_from(['P1', 'P2', 'P3']))) for _ in range(draw(st.integers(0, 4))))
    cfg = cfg_with(slice_size=slice_size, low_water=draw(st.integers(1, slice_size)), on_shift=on_shift,
                   exclusions=tuple(dict.fromkeys(pairs)), aging_per_hour=draw(st.sampled_from([0, 0.5, 5])))
    ready = [snap(f'C{i}', draw(st.integers(0, 30)), draw(st.sampled_from(['decide', 'decide_high'])),
                  draw(st.sampled_from(['P1', 'P2', 'P3'])), NOW - draw(st.integers(0, 100 * 3600)),
                  prior=tuple(draw(st.lists(st.sampled_from(BADGES), max_size=2, unique=True))))
             for i in range(draw(st.integers(0, 15)))]
    loads = {a.badge_id: {'count': draw(st.integers(0, slice_size + 1)), 'points': draw(st.integers(0, 60))} for a in agents}
    return ready, agents, loads, cfg, draw(st.integers(0, 2 ** 31))


class PlanPropertyTests(unittest.TestCase):
    @settings(max_examples=150, deadline=None, suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large])
    @given(worlds())
    def test_every_plan_satisfies_the_independent_invariants(self, world):
        ready, agents, loads, cfg, seed = world
        plan = plan_deal(ready, agents, loads, cfg, seed, NOW)
        considered = [a for a in agents if a.active and a.badge_id in cfg.on_shift]
        check_plan(ready, considered, loads, cfg, plan)
        self.assertEqual(plan, plan_deal(ready, agents, loads, cfg, seed, NOW))

    @settings(max_examples=100, deadline=None)
    @given(st.lists(st.integers(0, 30), min_size=1, max_size=40), st.integers(2, 6), st.integers(0, 2 ** 31))
    def test_equal_agents_end_with_loads_no_further_apart_than_the_largest_claim(self, scores, n_agents, seed):
        agents = [Agent(f'B{i}', L3) for i in range(n_agents)]
        cfg = cfg_with(on_shift=tuple(a.badge_id for a in agents), slice_size=40, aging_per_hour=0)
        ready = [snap(f'C{i}', s) for i, s in enumerate(scores)]
        plan = plan_deal(ready, agents, {}, cfg, seed, NOW)
        self.assertEqual(len(plan), len(ready))
        spread, biggest = spread_of_points(ready, agents, {}, cfg, plan)
        self.assertLessEqual(spread, biggest)


class DispatcherBase:
    def setUp(self):
        self.world = World()
        self.store, self.cleanup = self.factory()
        self.team = [Agent('B1', L3), Agent('B2', L3), Agent('B3', L2)]
        self.dispatcher = Dispatcher(self.store, lambda: list(self.team), self.world.clock, rng_seed=11, securitylog=self.world.log)
        set_cfg(self.store, on_shift=('B1', 'B2', 'B3'), slice_size=5, low_water=2)

    def tearDown(self):
        self.cleanup()
        self.world.close()

    def ready(self, n, prefix='C', **kw):
        for i in range(n):
            put_ready(self.store, f'{prefix}{i:03d}', now=self.world.clock(), **kw)

    def test_a_deal_leases_claims_into_personal_inboxes_up_to_the_slice_size(self):
        self.ready(30, eligibility='decide_high')
        deal = self.dispatcher.deal()
        self.assertEqual(len(deal.assigned), 10)                   # B3 may not take high claims: two agents x 5
        self.assertEqual((len(self.store.inbox('B1')), len(self.store.inbox('B2')), len(self.store.inbox('B3'))), (5, 5, 0))
        self.assertEqual(self.store.counts()['ready|A|decide_high'], 20)
        sent = [e for e in self.world.events('claim_dealt')]
        self.assertEqual(len(sent), 10)
        self.assertEqual(sent[0]['deal_id'], deal.deal_id)

    def test_an_agent_sees_only_the_slice_not_the_pool(self):
        self.ready(60, eligibility='decide')
        self.dispatcher.deal()
        for badge in ('B1', 'B2', 'B3'):
            self.assertEqual(len(self.store.inbox(badge)), 5)
        ids = [d['claim_id'] for b in ('B1', 'B2', 'B3') for d in self.store.inbox(b)]
        self.assertEqual(len(ids), len(set(ids)))                   # no overlap between inboxes

    def test_shortages_leave_claims_ready_and_nothing_crashes(self):
        self.ready(4, eligibility='decide_high')
        set_cfg(self.store, on_shift=())
        self.assertEqual(self.dispatcher.deal().assigned, [])
        set_cfg(self.store, on_shift=('B3',))                       # only an L2 on shift, only high claims
        self.assertEqual(self.dispatcher.deal().assigned, [])
        set_cfg(self.store, on_shift=('B1',), slice_size=1, low_water=1)
        self.assertEqual(len(self.dispatcher.deal().assigned), 1)
        self.assertEqual(self.dispatcher.deal().assigned, [])       # B1 is now full
        self.assertEqual(self.store.counts(), {'ready|A|decide_high': 3, 'leased|A|decide_high': 1})
        self.assertEqual(self.store.deals(10)[0]['assigned'][0][1], 'B1')

    def test_the_agent_list_is_read_fresh_so_a_demotion_takes_effect_at_once(self):
        self.ready(6, eligibility='decide_high')
        self.team[1] = Agent('B2', L2)                               # B2 is no longer allowed to take high claims
        self.dispatcher.deal()
        self.assertEqual(self.store.inbox('B2'), [])
        self.team[0] = Agent('B1', L3, active=False)
        self.ready(2, prefix='D', eligibility='decide_high')
        self.assertEqual(self.dispatcher.deal().assigned, [])

    def test_conflict_of_interest_across_versions_on_the_store(self):
        self.ready(1, eligibility='decide_high')
        self.store.lease('C000', 1, 'B1', self.world.clock(), self.world.clock() + 5)
        self.store.transition('C000', 1, 'leased', 'decided', 'B1', self.world.clock(), set_fields={'decided_by': 'B1'})
        put_ready(self.store, 'C000', now=self.world.clock(), version=2, input_hash='second')
        set_cfg(self.store, on_shift=('B1',))
        self.assertEqual(self.dispatcher.deal().assigned, [])
        set_cfg(self.store, on_shift=('B1', 'B2'))
        self.assertEqual([(c, b) for c, b, v in self.dispatcher.deal().assigned], [('C000', 'B2')])

    def test_excluded_pairs_from_the_configuration_are_respected(self):
        self.ready(8, eligibility='decide', patient='PX')
        set_cfg(self.store, exclusions=(('B1', 'PX'), ('B2', 'PX')))
        self.dispatcher.deal()
        self.assertEqual((self.store.inbox('B1'), self.store.inbox('B2')), ([], []))
        self.assertEqual(len(self.store.inbox('B3')), 5)

    def test_a_deal_can_be_replayed_exactly_and_a_tampered_seed_is_detected(self):
        self.ready(25, eligibility='decide')
        deal = self.dispatcher.deal()
        again = self.dispatcher.replay(deal.deal_id)
        self.assertEqual(again, [tuple(t) for t in deal.planned])
        self.assertTrue(self.dispatcher.verify(deal.deal_id))
        self.tamper(deal.deal_id, seed=deal.seed + 1)
        self.assertNotEqual(self.dispatcher.replay(deal.deal_id), again)
        self.assertFalse(self.dispatcher.verify(deal.deal_id))

    def test_a_deal_records_one_configuration_version_even_if_the_configuration_changes_mid_deal(self):
        self.ready(30, eligibility='decide')
        before = self.store.latest_config()
        original = self.store.by_state

        def changing(*a, **k):
            rows = original(*a, **k)
            set_cfg(self.store, lease_seconds=60, slice_size=2, on_shift=())
            return rows
        self.store.by_state = changing
        try:
            deal = self.dispatcher.deal(now=NOW)
        finally:
            self.store.by_state = original
        self.assertEqual(deal.config_version, before['version'])
        self.assertEqual(len(deal.assigned), 15)                    # three agents x the slice size the deal began with
        first = self.store.inbox('B1')[0]
        self.assertEqual(first['lease']['expires_at'], NOW + before['lease_seconds'])
        self.assertEqual(self.dispatcher.replay(deal.deal_id), [tuple(t) for t in deal.planned])      # replay uses that version
        self.assertTrue(self.dispatcher.verify(deal.deal_id))

    def test_twenty_dispatchers_dealing_at_once_never_put_a_claim_in_two_inboxes(self):
        set_cfg(self.store, slice_size=100, low_water=10)
        self.ready(200, eligibility='decide')
        dispatchers = [Dispatcher(self.store, lambda: list(self.team), self.world.clock, securitylog=None) for _ in range(20)]
        race(20, lambda i: dispatchers[i].deal())
        leased = self.store.by_state('leased', 1000)
        self.assertEqual(len(leased), 200)
        for d in leased:
            self.assertEqual(sum(1 for e in d['events'] if (e['from'], e['to']) == ('ready', 'leased')), 1, d['claim_id'])
        total = sum(len(self.store.inbox(b)) for b in ('B1', 'B2', 'B3'))
        self.assertEqual(total, 200)

    def test_top_up_does_nothing_at_or_above_low_water_and_fills_to_the_slice_below_it(self):
        self.ready(40, eligibility='decide')
        self.assertEqual(self.dispatcher.top_up_if_low('B1'), 5)            # empty inbox: below low water (2), filled to 5
        self.assertEqual(self.dispatcher.top_up_if_low('B1'), 0)            # at the slice size
        for d in self.store.inbox('B1')[:3]:
            self.store.transition(d['claim_id'], 1, 'leased', 'decided', 'B1', self.world.clock(), set_fields={'decided_by': 'B1'})
        self.assertEqual(len(self.store.inbox('B1')), 2)
        self.assertEqual(self.dispatcher.top_up_if_low('B1'), 0)            # exactly low_water: no top-up
        self.store.transition(self.store.inbox('B1')[0]['claim_id'], 1, 'leased', 'decided', 'B1', self.world.clock(), set_fields={'decided_by': 'B1'})
        self.assertEqual(self.dispatcher.top_up_if_low('B1'), 4)            # one left: filled back up to 5

    def test_the_scheduled_top_up_touches_only_agents_below_low_water(self):
        self.ready(40, eligibility='decide')
        self.dispatcher.deal()
        for d in self.store.inbox('B1')[:4]:
            self.store.transition(d['claim_id'], 1, 'leased', 'decided', 'B1', self.world.clock(), set_fields={'decided_by': 'B1'})
        for d in self.store.inbox('B2')[:1]:
            self.store.transition(d['claim_id'], 1, 'leased', 'decided', 'B1', self.world.clock(), set_fields={'decided_by': 'B2'})
        deal = self.dispatcher.deal(full=False)
        self.assertEqual({b for _, b, _ in deal.assigned}, {'B1'})
        self.assertEqual(len(self.store.inbox('B1')), 5)
        self.assertEqual(len(self.store.inbox('B2')), 4)

    def test_next_for_adds_exactly_one_claim_or_none(self):
        self.ready(3, eligibility='decide')
        got = self.dispatcher.next_for('B1')
        self.assertEqual((got['state'], got['lease']['badge_id']), ('leased', 'B1'))
        self.assertEqual(len(self.store.inbox('B1')), 1)
        self.assertIsNone(self.dispatcher.next_for('NOBODY'))
        set_cfg(self.store, slice_size=1, low_water=1)
        self.assertIsNone(self.dispatcher.next_for('B1'))                   # full

    def test_expired_leases_return_to_the_pool_and_go_to_a_different_agent_when_there_is_one(self):
        for k in range(8):
            t = NOW + 10000 * k
            claim_id = f'X{k}'
            put_ready(self.store, claim_id, now=t, eligibility='decide')
            self.dispatcher.rng_seed = k
            holder = self.dispatcher.deal(now=t).assigned[0][1]
            self.assertEqual(self.dispatcher.expire(now=t + 1799), 0)
            self.assertEqual(self.dispatcher.expire(now=t + 1800), 1)
            self.assertEqual(self.store.get(claim_id)['state'], 'ready')
            self.dispatcher.rng_seed = k + 100
            again = self.dispatcher.deal(now=t + 2000)
            self.assertEqual([c for c, _, _ in again.assigned], [claim_id])
            self.assertNotEqual(again.assigned[0][1], holder, claim_id)
            self.store.transition(claim_id, 1, 'leased', 'decided', again.assigned[0][1], t + 2100, set_fields={'decided_by': again.assigned[0][1]})
        self.assertEqual(len(self.world.events('lease_expired')), 8)

    def test_with_one_agent_a_handed_back_claim_goes_back_to_that_agent(self):
        set_cfg(self.store, on_shift=('B1',))
        put_ready(self.store, 'C1', now=NOW, eligibility='decide')
        self.dispatcher.deal(now=NOW)
        self.dispatcher.expire(now=NOW + 1800)
        self.assertEqual([b for _, b, _ in self.dispatcher.deal(now=NOW + 1900).assigned], ['B1'])

    def test_a_decision_in_time_beats_the_expiry(self):
        self.ready(1, eligibility='decide')
        deal = self.dispatcher.deal(now=NOW)
        holder = deal.assigned[0][1]
        self.store.transition('C000', 1, 'leased', 'decided', holder, NOW + 10, set_fields={'decided_by': holder})
        self.assertEqual(self.dispatcher.expire(now=NOW + 5000), 0)
        self.assertEqual(self.store.get('C000')['state'], 'decided')

    def test_an_empty_pool_records_no_deal(self):
        self.dispatcher.deal()
        self.assertEqual(self.store.deals(10), [])

    def test_replaying_an_unknown_deal_is_an_error(self):
        with self.assertRaises(ValueError):
            self.dispatcher.replay('nope')


def tamper_memory(store, deal_id, seed):
    for d in store._deals:
        if d['deal_id'] == deal_id:
            d['seed'] = seed


def tamper_mongo(store, deal_id, seed):
    store.raw_db['deals'].update_one({'deal_id': deal_id}, {'$set': {'seed': seed}})


def _make(name, factory, tamper):
    def tamper_method(self, deal_id, seed):
        tamper(self.store, deal_id, seed)
    return type('Dispatcher_' + name, (DispatcherBase, unittest.TestCase),
                {'factory': staticmethod(factory), 'tamper': tamper_method})


for _name, _factory in store_makers():
    globals()['Dispatcher_' + _name] = _make(_name, _factory, tamper_memory if _name == 'memory' else tamper_mongo)
del _name, _factory

if __name__ == '__main__':
    unittest.main()
