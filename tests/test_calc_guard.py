"""Architecture B's calculator tool evaluates model-chosen text. The character whitelist stops code execution but
admitted exponent chains such as 9**9**9, which hang the process (resource exhaustion). calc_guard closes that.

These tests import comparison/architecture_b/calc_guard.py directly; it has no heavy dependencies, unlike agent.py.
"""
import re
import sys
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'comparison' / 'architecture_b'))
import calc_guard as g


class GuardTests(unittest.TestCase):
    def test_ordinary_arithmetic_is_allowed(self):
        for e in ('1+1', '(2+3)*4', '10/4', '7//2', '1.5*2', ' 3 - 1 ', '2*(3+(4-1))'):
            self.assertIsNone(g.check_expression(e), e)

    def test_exponentiation_is_rejected_however_it_is_spaced(self):
        for e in ('9**9**9', '2**1000000', '2 ** 3', '2**\t3'):
            self.assertIn('exponentiation', g.check_expression(e), e)

    def test_disallowed_characters_keep_the_original_message(self):
        for e in ("__import__('os')", '().__class__', 'abs(1)', '', '1;2'):
            self.assertEqual(g.check_expression(e), 'Error: expression contains disallowed characters.', repr(e))

    def test_overlong_expressions_are_rejected(self):
        self.assertIn('too long', g.check_expression('1+' * 200 + '1'))
        self.assertIsNone(g.check_expression('1' * g.MAX_EXPRESSION_CHARS))

    def test_non_text_input_is_an_error_not_a_crash(self):
        for e in (None, 5, ['1+1']):
            self.assertIn('text', g.check_expression(e))


class WorstCaseTests(unittest.TestCase):
    """Whatever the guard accepts must finish quickly when evaluated the way agent.py evaluates it."""

    def evaluate(self, expression):
        return eval(expression, {"__builtins__": {}}, {})

    def test_the_longest_accepted_expressions_finish_in_well_under_a_second(self):
        n = g.MAX_EXPRESSION_CHARS
        worst = [
            '9' * n,
            '9' * (n // 2) + '*' + '9' * (n // 2 - 1),
            '*'.join(['99999'] * (n // 6)),
            '(' * 90 + '9' + ')' * 90,
            '9//' * (n // 3 - 1) + '9',
        ]
        for e in worst:
            self.assertIsNone(g.check_expression(e), e[:30])
            t0 = time.perf_counter()
            try:
                self.evaluate(e)
            except Exception:
                pass                              # a syntax or arithmetic error is fine; hanging is not
            self.assertLess(time.perf_counter() - t0, 1.0, e[:30])

    def test_the_original_hang_payload_is_now_blocked_before_evaluation(self):
        self.assertIsNotNone(g.check_expression('9**9**9'))


class WiringTests(unittest.TestCase):
    def test_agent_py_calls_the_guard_before_evaluating(self):
        text = (ROOT / 'comparison' / 'architecture_b' / 'agent.py').read_text(encoding='utf-8')
        self.assertIn('from calc_guard import check_expression', text)
        body = text[text.index('def calculator'):]
        guard_at = body.index('check_expression(expression)')
        eval_at = re.search(r'\beval\(', body).start()
        self.assertLess(guard_at, eval_at)


if __name__ == '__main__':
    unittest.main()
