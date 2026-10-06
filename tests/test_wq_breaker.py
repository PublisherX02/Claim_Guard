"""The circuit breaker, with a fake clock and a function that fails, recovers and slows on command."""
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from hypothesis import given, settings, strategies as st
from workqueue.breaker import CLOSED, HALF_OPEN, OPEN, CircuitBreaker, Fatal, Open, Timeout, Transient
from wq_world import FakeClock


class Probe:
    """A function the test controls: records how often it really ran."""

    def __init__(self):
        self.runs, self.mode = 0, 'ok'

    def __call__(self):
        self.runs += 1
        if self.mode == 'transient':
            raise Transient('503')
        if self.mode == 'fatal':
            raise Fatal('400')
        if self.mode == 'timeout':
            raise Timeout('slow')
        return 'fine'


class BreakerTests(unittest.TestCase):
    def setUp(self):
        self.clock, self.fn = FakeClock(0.0), Probe()
        self.b = CircuitBreaker(self.clock)

    def fail(self, n, gap=0.0):
        self.fn.mode = 'transient'
        for _ in range(n):
            with self.assertRaises(Transient):
                self.b.call(self.fn)
            self.clock.advance(gap)

    def test_it_opens_after_exactly_five_failures_in_a_row_and_not_after_four(self):
        self.fail(4)
        self.assertEqual(self.b.state, CLOSED)
        self.fail(1)
        self.assertEqual(self.b.state, OPEN)

    def test_a_success_in_between_resets_the_streak(self):
        self.fail(4)
        self.fn.mode = 'ok'
        self.assertEqual(self.b.call(self.fn), 'fine')
        self.fail(4)
        self.assertEqual(self.b.state, CLOSED)

    def test_an_open_breaker_refuses_without_calling_the_function(self):
        self.fail(5)
        runs = self.fn.runs
        for _ in range(10):
            with self.assertRaises(Open):
                self.b.call(self.fn)
        self.assertEqual(self.fn.runs, runs)

    def test_the_rate_rule_needs_at_least_twenty_calls(self):
        # alternate failure/success so the streak never reaches five: 19 calls at ~53 % failure must not open
        for i in range(19):
            self.fn.mode = 'transient' if i % 2 == 0 else 'ok'
            try:
                self.b.call(self.fn)
            except Transient:
                pass
        self.assertEqual(self.b.state, CLOSED)
        self.fn.mode = 'transient'                                       # the 20th call: 11 failures of 20 = 55 %
        with self.assertRaises(Transient):
            self.b.call(self.fn)
        self.assertEqual(self.b.state, OPEN)

    def test_five_failures_in_a_row_open_through_the_streak_rule_long_before_twenty_calls(self):
        self.fail(5)
        self.assertEqual(self.b.state, OPEN)
        self.assertEqual(self.fn.runs, 5)

    def test_old_calls_leave_the_window(self):
        for i in range(19):
            self.fn.mode = 'transient' if i % 2 == 0 else 'ok'
            try:
                self.b.call(self.fn)
            except Transient:
                pass
        self.clock.advance(61)                                           # all 19 calls are now outside the 60 s window
        self.fn.mode = 'transient'
        with self.assertRaises(Transient):
            self.b.call(self.fn)
        self.assertEqual(self.b.state, CLOSED)

    def test_after_the_cooldown_one_probe_passes_and_success_closes(self):
        self.fail(5)
        self.clock.advance(19.9)
        with self.assertRaises(Open):
            self.b.call(self.fn)
        self.clock.advance(0.1)
        self.assertEqual(self.b.state, HALF_OPEN)
        self.fn.mode = 'ok'
        self.assertEqual(self.b.call(self.fn), 'fine')
        self.assertEqual(self.b.state, CLOSED)
        self.fail(4)
        self.assertEqual(self.b.state, CLOSED)                           # the streak started again from zero

    def test_a_failed_probe_reopens_for_a_new_cooldown(self):
        self.fail(5)
        self.clock.advance(20)
        self.fn.mode = 'timeout'
        with self.assertRaises(Timeout):
            self.b.call(self.fn)
        self.assertEqual(self.b.state, OPEN)
        self.clock.advance(19.9)
        with self.assertRaises(Open):
            self.b.call(self.fn)
        self.clock.advance(0.2)
        self.fn.mode = 'ok'
        self.assertEqual(self.b.call(self.fn), 'fine')

    def test_a_second_caller_during_the_probe_is_refused(self):
        self.fail(5)
        self.clock.advance(20)
        started, release = threading.Event(), threading.Event()
        results = []

        def slow():
            started.set()
            release.wait(5)
            return 'probe'
        t = threading.Thread(target=lambda: results.append(self.b.call(slow)))
        t.start()
        self.assertTrue(started.wait(5))
        with self.assertRaises(Open):
            self.b.call(self.fn)
        release.set()
        t.join()
        self.assertEqual((results, self.b.state), (['probe'], CLOSED))

    def test_fatal_errors_never_count_and_never_open(self):
        self.fn.mode = 'fatal'
        for _ in range(50):
            with self.assertRaises(Fatal):
                self.b.call(self.fn)
        self.assertEqual(self.b.state, CLOSED)
        self.fn.mode = 'transient'
        self.fail(4)
        self.assertEqual(self.b.state, CLOSED)

    def test_a_fatal_probe_frees_the_probe_slot(self):
        self.fail(5)
        self.clock.advance(20)
        self.fn.mode = 'fatal'
        with self.assertRaises(Fatal):
            self.b.call(self.fn)
        self.fn.mode = 'ok'
        self.assertEqual(self.b.call(self.fn), 'fine')                   # another probe is allowed straight away

    def test_other_exceptions_pass_through_and_do_not_count(self):
        def broken():
            raise KeyError('bug')
        for _ in range(10):
            with self.assertRaises(KeyError):
                self.b.call(broken)
        self.assertEqual(self.b.state, CLOSED)

    def test_the_thresholds_are_settable(self):
        b = CircuitBreaker(self.clock, consecutive=2, cooldown=5)
        self.fn.mode = 'transient'
        for _ in range(2):
            with self.assertRaises(Transient):
                b.call(self.fn)
        self.assertEqual(b.state, OPEN)
        self.clock.advance(5)
        self.assertEqual(b.state, HALF_OPEN)


class BreakerPropertyTests(unittest.TestCase):
    @settings(max_examples=200, deadline=None)
    @given(st.lists(st.tuples(st.sampled_from(['ok', 'transient', 'fatal', 'timeout']), st.floats(0, 30)), max_size=80))
    def test_the_state_is_always_one_of_three_and_an_open_breaker_never_runs_the_function(self, steps):
        clock, fn = FakeClock(0.0), Probe()
        b = CircuitBreaker(clock)
        for mode, gap in steps:
            clock.advance(gap)
            fn.mode = mode
            before, state_before = fn.runs, b.state
            self.assertIn(state_before, (CLOSED, OPEN, HALF_OPEN))
            try:
                b.call(fn)
            except Open:
                self.assertEqual(fn.runs, before)
                self.assertIn(state_before, (OPEN, HALF_OPEN))
            except (Transient, Fatal):
                self.assertEqual(fn.runs, before + 1)
            else:
                self.assertEqual(fn.runs, before + 1)
            self.assertIn(b.state, (CLOSED, OPEN, HALF_OPEN))
            if state_before == OPEN:
                self.assertEqual(fn.runs, before)


if __name__ == '__main__':
    unittest.main()
