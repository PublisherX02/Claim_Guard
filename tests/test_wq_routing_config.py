"""The routing configuration: defaults are the specification's table, and any invalid configuration is refused whole."""
import copy
import dataclasses
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from workqueue import routing_config as rc


def make(**changes):
    return dataclasses.replace(rc.RoutingConfig(), **changes)


class DefaultTests(unittest.TestCase):
    def test_defaults_are_the_specification_table_written_out(self):
        d = rc.DEFAULT
        self.assertEqual(d.version, 1)
        self.assertEqual(d.points, {'FAIL': {'high': 4, 'medium': 2}, 'UNABLE_TO_ASSESS': {'high': 2, 'medium': 1}})
        self.assertEqual((d.lane_b_flagged, d.lane_b_score), (4, 10))
        self.assertEqual((d.slice_size, d.low_water, d.lease_seconds), (25, 10, 1800))
        self.assertEqual((d.aging_per_hour, d.ai_per_minute, d.ai_daily_budget), (0.5, 30, 2000))
        self.assertEqual((d.on_shift, d.exclusions), ((), ()))

    def test_the_default_object_is_itself_valid_and_frozen(self):
        self.assertIs(rc.validate(rc.DEFAULT), rc.DEFAULT)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            rc.DEFAULT.slice_size = 3


class BoundTests(unittest.TestCase):
    def refused(self, **changes):
        with self.assertRaises(ValueError, msg=repr(changes)):
            rc.validate(make(**changes))

    def accepted(self, **changes):
        rc.validate(make(**changes))

    def test_numeric_bounds_on_both_sides(self):
        self.refused(lane_b_flagged=0); self.refused(lane_b_flagged=16)
        self.accepted(lane_b_flagged=1); self.accepted(lane_b_flagged=15)
        self.refused(lane_b_score=0); self.refused(lane_b_score=1001)
        self.refused(low_water=0); self.refused(slice_size=201, low_water=10)
        self.accepted(slice_size=200, low_water=200); self.accepted(slice_size=1, low_water=1)
        self.refused(slice_size=5, low_water=6)
        self.refused(lease_seconds=59); self.refused(lease_seconds=86401)
        self.accepted(lease_seconds=60); self.accepted(lease_seconds=86400)
        self.refused(aging_per_hour=-0.1); self.refused(aging_per_hour=100.1); self.accepted(aging_per_hour=0)
        self.refused(ai_per_minute=-1); self.refused(ai_per_minute=601)
        self.refused(ai_daily_budget=-1); self.refused(ai_daily_budget=100001)

    def test_points_must_be_whole_numbers_in_range_and_fail_not_below_unable(self):
        good = copy.deepcopy(rc.DEFAULT.points)
        for status, sev, value in (('FAIL', 'high', -1), ('FAIL', 'high', 101), ('FAIL', 'medium', 1.5), ('UNABLE_TO_ASSESS', 'high', '2')):
            bad = copy.deepcopy(good); bad[status][sev] = value
            self.refused(points=bad)
        inverted = copy.deepcopy(good); inverted['UNABLE_TO_ASSESS']['high'] = 5
        self.refused(points=inverted)
        missing = copy.deepcopy(good); del missing['FAIL']['medium']
        self.refused(points=missing)
        extra = copy.deepcopy(good); extra['PASS'] = {'high': 0, 'medium': 0}
        self.refused(points=extra)

    def test_true_is_not_a_number(self):
        self.refused(slice_size=True, low_water=1)
        self.refused(lane_b_flagged=True)

    def test_on_shift_is_a_tuple_of_distinct_non_empty_text(self):
        self.accepted(on_shift=('B1', 'B2'))
        for bad in (('B1', 'B1'), ('',), (1,), ['B1'], {'B1': 1}, None):
            self.refused(on_shift=bad)

    def test_exclusions_are_pairs_of_non_empty_text(self):
        self.accepted(exclusions=(('B1', 'P1'),))
        for bad in ((('B1',),), (('B1', ''),), (('B1', 'P1', 'x'),), (('B1', 7),), ('B1P1',)):
            self.refused(exclusions=bad)


class DocumentTests(unittest.TestCase):
    def test_round_trip_is_exact(self):
        cfg = make(version=7, on_shift=('B1', 'B2'), exclusions=(('B1', 'P9'),), slice_size=30, low_water=12)
        self.assertEqual(rc.from_doc(rc.to_doc(cfg)), cfg)

    def test_unknown_key_is_refused(self):
        doc = rc.to_doc(rc.DEFAULT); doc['admin_override'] = True
        with self.assertRaises(ValueError):
            rc.from_doc(doc)

    def test_from_doc_validates(self):
        doc = rc.to_doc(rc.DEFAULT); doc['lease_seconds'] = 0
        with self.assertRaises(ValueError):
            rc.from_doc(doc)

    def test_a_document_is_plain_data(self):
        doc = rc.to_doc(make(on_shift=('B1',), exclusions=(('B1', 'P1'),)))
        self.assertIsInstance(doc['on_shift'], list)
        self.assertIsInstance(doc['exclusions'], list)
        self.assertNotIn(dataclasses.FrozenInstanceError, [type(v) for v in doc.values()])


class SecurityEventTests(unittest.TestCase):
    def test_queue_event_types_are_registered(self):
        from audit_log import SECURITY_EVENTS
        self.assertEqual(SECURITY_EVENTS['routing_config_changed'], {'actor', 'version', 'before', 'after'})
        self.assertEqual(SECURITY_EVENTS['lease_expired'], {'claim_id', 'badge_id'})
        self.assertEqual(SECURITY_EVENTS['claim_dealt'], {'deal_id', 'claim_id', 'badge_id'})
        self.assertEqual(SECURITY_EVENTS['claim_decided_green'], {'badge_id', 'claim_id', 'action'})


if __name__ == '__main__':
    unittest.main()
