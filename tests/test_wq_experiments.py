"""The scale experiment: deterministic, its invariants hold for every seed, the senior bottleneck is reported, and the evidence file is sound."""
import json
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
import queue_experiments as qx

SMALL = dict(n_claims=80, n_agents=8, slice_size=8)


class ExperimentTests(unittest.TestCase):
    def test_the_same_seed_gives_identical_output(self):
        a = json.dumps(qx.run(seed=3, l3=2, **SMALL), sort_keys=True)
        b = json.dumps(qx.run(seed=3, l3=2, **SMALL), sort_keys=True)
        self.assertEqual(a, b)

    def test_another_seed_changes_the_workload(self):
        a, b = qx.run(seed=1, l3=2, **SMALL), qx.run(seed=2, l3=2, **SMALL)
        self.assertNotEqual((a['lane_counts'], a['drain']), (b['lane_counts'], b['drain']))

    def test_no_claim_is_in_two_inboxes_nobody_exceeds_a_slice_and_nobody_decides_what_they_may_not_for_five_seeds(self):
        for seed in range(1, 6):
            r = qx.run(seed=seed, l3=2, **SMALL)
            c = r['checks']
            self.assertEqual(c['claims_in_two_inboxes'], 0, seed)
            self.assertEqual(c['eligibility_respected_pct'], 100.0, seed)
            self.assertEqual(c['decisions_by_agents_not_allowed'], 0, seed)
            self.assertTrue(c['slice_never_exceeded'] and c['max_inbox_seen'] <= SMALL['slice_size'], seed)
            self.assertEqual(c['decisions'], SMALL['n_claims'], seed)
            self.assertEqual(r['drain']['claims_left_undecided'], 0, seed)

    def test_every_claim_is_dealt_to_exactly_one_agent_and_handled_once(self):
        r = qx.run(seed=4, l3=2, **SMALL)
        self.assertEqual(r['checks']['decisions'], r['n_claims'])
        self.assertEqual(sum(r['lane_counts'].values()), r['n_claims'])
        self.assertEqual(sum(r['eligibility_counts'].values()), r['n_claims'])

    def test_with_no_senior_on_shift_the_high_severity_claims_are_left_undrained_and_the_report_says_so(self):
        r = qx.run(seed=1, l3=0, **SMALL)
        high = r['eligibility_counts'].get('decide_high', 0)
        self.assertGreater(high, 0)
        self.assertEqual((r['drain']['claims_left_undecided'], r['drain']['left_requiring_senior']), (high, high))
        self.assertEqual(r['checks']['decisions'], SMALL['n_claims'] - high)
        self.assertIsNone(r['utilization']['senior'])

    def test_granting_the_senior_flag_to_more_agents_shortens_the_drain(self):
        base = qx.run(seed=1, l3=1, **SMALL)
        more = qx.run(seed=1, l3=1, extra_granted=3, **SMALL)
        self.assertEqual(more['senior_agents'], 4)
        self.assertLess(more['drain']['simulated_seconds'], base['drain']['simulated_seconds'])
        self.assertGreater(base['utilization']['senior'], base['utilization']['junior'])          # the bottleneck is the seniors

    def test_with_sign_off_nobody_signs_a_claim_twice_every_claim_is_still_decided_and_it_takes_longer(self):
        plain = qx.run(seed=2, l3=3, **SMALL)
        signed = qx.run(seed=2, l3=3, signoff=True, **SMALL)
        c = signed['checks']
        self.assertEqual((c['claims_signed_twice_by_one_person'], c['decisions'], signed['drain']['claims_left_undecided']), (0, SMALL['n_claims'], 0))
        self.assertGreater(c['signatures'], plain['checks']['signatures'])
        self.assertGreaterEqual(signed['drain']['simulated_seconds'], plain['drain']['simulated_seconds'])
        self.assertEqual(c['eligibility_respected_pct'], 100.0)

    def test_with_sign_off_and_a_single_senior_the_high_claims_cannot_be_countersigned_and_are_left_waiting(self):
        r = qx.run(seed=2, l3=1, signoff=True, **SMALL)
        high_claims = r['eligibility_counts'].get('decide_high', 0)
        self.assertGreater(high_claims, 0)
        self.assertGreater(r['drain']['claims_left_undecided'], 0)
        self.assertEqual(r['checks']['claims_signed_twice_by_one_person'], 0)

    def test_the_template_cache_saves_model_calls(self):
        r = qx.run(seed=1, l3=2, n_claims=120, n_agents=8, slice_size=8)
        c = r['cache']
        self.assertEqual(c['cache_hits'] + c['fresh_explanations'], c['flagged_findings'])
        self.assertEqual(c['model_calls'], c['fresh_explanations'])
        self.assertGreater(c['hit_rate'], 0.5)

    def test_shadow_agreement_covers_every_decided_claim_and_reports_an_interval(self):
        r = qx.run(seed=1, l3=2, **SMALL)
        s = r['shadow_agreement']
        self.assertEqual(s['n'], SMALL['n_claims'])
        self.assertTrue(s['lower'] <= s['rate'] <= s['upper'])

    def test_the_output_says_the_people_and_the_model_are_simulated(self):
        a = qx.run(seed=1, l3=2, **SMALL)['assumptions']
        self.assertTrue(a['people_are_simulated'] and a['model_is_a_stand_in'])

    def test_asking_for_more_claims_than_exist_is_refused(self):
        with self.assertRaises(ValueError):
            qx.run(n_claims=100000, n_agents=2, slice_size=5)

    def test_the_commit_is_read_from_the_working_tree(self):
        self.assertRegex(qx.commit_hash(), r'^[0-9a-f]{40}$')


class EvidenceFileTests(unittest.TestCase):
    PATH = ROOT / 'outputs' / 'defense' / 'queue.json'

    def setUp(self):
        if not self.PATH.exists():
            self.skipTest('outputs/defense/queue.json has not been generated: run scripts/queue_experiments.py')
        self.data = json.loads(self.PATH.read_text(encoding='utf-8'))

    def test_it_names_the_commit_and_has_every_scenario(self):
        self.assertRegex(self.data['commit'], r'^[0-9a-f]{40}$')
        self.assertEqual(set(self.data), {'commit', 'workload', 'base_4_senior_of_20', 'with_4_more_l2_granted_the_senior_flag',
                                          'no_senior_on_shift', 'with_two_person_signoff', 'with_two_person_signoff_and_4_more_l2_granted',
                                          'invariants_over_five_seeds'})
        self.assertEqual(self.data['workload'], {'claims': 500, 'agents': 20, 'slice_size': 25, 'seed': 1})

    def test_every_scenario_has_the_same_report_keys(self):
        keys = None
        for name in ('base_4_senior_of_20', 'with_4_more_l2_granted_the_senior_flag', 'no_senior_on_shift', 'with_two_person_signoff',
                     'with_two_person_signoff_and_4_more_l2_granted'):
            got = set(self.data[name])
            keys = keys or got
            self.assertEqual(got, keys, name)
        self.assertTrue({'checks', 'drain', 'fairness_points', 'cache', 'shadow_agreement', 'assumptions', 'utilization'} <= keys)

    def test_the_recorded_invariants_hold(self):
        for name in ('base_4_senior_of_20', 'with_4_more_l2_granted_the_senior_flag', 'no_senior_on_shift', 'with_two_person_signoff',
                     'with_two_person_signoff_and_4_more_l2_granted'):
            c = self.data[name]['checks']
            self.assertEqual((c['claims_in_two_inboxes'], c['eligibility_respected_pct'], c['slice_never_exceeded'],
                              c['claims_signed_twice_by_one_person']), (0, 100.0, True, 0), name)
        inv = self.data['invariants_over_five_seeds']
        self.assertEqual((set(inv['overlaps']), set(inv['eligibility_respected_pct']), set(inv['slice_never_exceeded'])), ({0}, {100.0}, {True}))

    def test_the_cost_of_two_person_sign_off_is_recorded(self):
        base, signed = self.data['base_4_senior_of_20'], self.data['with_two_person_signoff']
        self.assertGreater(signed['drain']['simulated_seconds'], base['drain']['simulated_seconds'])
        self.assertGreater(signed['checks']['signatures'], base['checks']['signatures'])
        self.assertEqual(signed['checks']['decisions'], 500)
        self.assertTrue(signed['assumptions']['two_person_signoff'])

    def test_the_bottleneck_is_recorded_honestly(self):
        base, granted, none = (self.data[k] for k in ('base_4_senior_of_20', 'with_4_more_l2_granted_the_senior_flag', 'no_senior_on_shift'))
        self.assertLess(granted['drain']['simulated_seconds'], base['drain']['simulated_seconds'])
        self.assertGreater(base['utilization']['senior'], base['utilization']['junior'])
        self.assertEqual(none['drain']['claims_left_undecided'], none['eligibility_counts']['decide_high'])
        self.assertEqual(base['checks']['decisions'], 500)

    def test_it_contains_no_claim_or_patient_identifier(self):
        text = self.PATH.read_text(encoding='utf-8')
        self.assertIsNone(re.search(r'(?:PAT|MEM|CG)-[0-9A-F]{6,}', text))


if __name__ == '__main__':
    unittest.main()
