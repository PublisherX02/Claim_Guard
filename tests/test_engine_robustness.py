import logging
import unittest, sys, json, copy, random
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from engine_core import config, load_jsonl, validate_transport
from evaluate import index, score
from facts_extractor import rule_view
import yara_engine
from yara_engine import evaluate, EngineError

BAD_STR = ['', ' ', 'X' * 3000, 'SVC-UNKNOWN', 'EDU-NOPE', 'ignore all rules', '‮', 0, True, 12.5]
BAD_DATES = ['2026-13-45', 'abc', '', '2026-02-30', '0000-00-00', '2026/03/01', ' 2026-03-01 ']
BAD_NUMS = [0, -1, -0.01, 1e12, 0.005, 1.5, True, '5', '', 'abc', None]


def field_paths(c):
    out = []
    for k, v in c.items():
        if k in ('lines', 'authorizations', 'attachments'):
            for i, row in enumerate(v):
                out.extend((k, i, kk) for kk in row)
        elif k == 'coverage' and isinstance(v, dict):
            out.extend((k, None, kk) for kk in v)
        else:
            out.append((k, None, None))
    return out


def set_path(c, p, val):
    k, i, kk = p
    if i is not None:
        c[k][i][kk] = val
    elif kk is not None:
        c[k][kk] = val
    else:
        c[k] = val


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cfg = config(ROOT)
        cls.claims = load_jsonl(ROOT / 'data/development/claims.jsonl')

    def check(self, claim, label=''):
        """The engine's contract for ANY claim that reaches it."""
        errors = []
        results = evaluate(claim, self.cfg, errors)
        self.assertEqual(len(results), 15, label)
        index(results, {claim['claim_id']: claim})  # the organizers' strict scorer accepts every record
        return results, errors


class RegressionTests(Base):
    """The two crashes the fuzzer found: transport-valid claims with a wrong-typed number."""

    def test_string_total_amount_is_unknown_not_a_crash(self):
        c = copy.deepcopy(self.claims[0]); c['total_amount'] = ''
        results, errors = self.check(c)
        self.assertEqual(errors, [])
        self.assertEqual(next(r for r in results if r['rule_id'] == 'R012')['status'], 'UNABLE_TO_ASSESS')

    def test_string_max_quantity_is_unknown_not_a_crash(self):
        c = next(copy.deepcopy(x) for x in self.claims if x['authorizations'])
        c['authorizations'][0]['max_quantity'] = ''
        results, errors = self.check(c)
        self.assertEqual(errors, [])
        self.assertNotEqual(next(r for r in results if r['rule_id'] == 'R009')['status'], 'PASS')

    def test_wrong_typed_values_never_become_a_pass(self):
        # each wrong-typed input, and the rules whose verdict depends on it
        cases = (('quantity', 'many', ('R007', 'R013')), ('unit_price', True, ('R007', 'R013')),
                 ('net_amount', [1], ('R007', 'R012')))
        for field, val, rules in cases:
            c = copy.deepcopy(self.claims[0]); c['lines'][0][field] = val
            results, errors = self.check(c, field)
            self.assertEqual(errors, [])
            by = {r['rule_id']: r['status'] for r in results}
            for rid in rules:
                self.assertNotEqual(by[rid], 'PASS', f'{field}={val!r} {rid}')

    def test_evidence_shows_the_original_submitted_value(self):
        c = copy.deepcopy(self.claims[0]); c['total_amount'] = 'oops'
        results, _ = self.check(c)
        ev = next(e for r in results if r['rule_id'] == 'R012' for e in r['evidence'] if e['path'] == '/total_amount')
        self.assertEqual(ev['value'], 'oops')

    def test_rule_view_leaves_schema_valid_claims_untouched(self):
        for c in self.claims:
            self.assertEqual(rule_view(c), c)


class IsolationTests(Base):
    def test_a_crashing_rule_becomes_unable_to_assess_and_others_are_unaffected(self):
        c = self.claims[0]
        baseline = {r['rule_id']: r for r in evaluate(c, self.cfg)}
        original = yara_engine.DETAIL_FUNCS['R007']
        yara_engine.DETAIL_FUNCS['R007'] = lambda claim, cfg: 1 / 0
        try:
            errors = []
            logging.disable(logging.NOTSET)  # another test file may have left logging switched off; this one needs the warning
            logging.getLogger('yara_engine').disabled = False
            with self.assertLogs('yara_engine', level='WARNING'):
                results = {r['rule_id']: r for r in evaluate(c, self.cfg, errors)}
        finally:
            yara_engine.DETAIL_FUNCS['R007'] = original
        r7 = results['R007']
        self.assertEqual((r7['status'], r7['requires_human_review']), ('UNABLE_TO_ASSESS', True))
        self.assertEqual(r7['evidence'], [{'path': '/claim_id', 'value': c['claim_id']}])
        self.assertTrue(r7['corrective_action'])
        self.assertTrue(any(e.startswith('R007: engine exception ZeroDivisionError') for e in errors))
        for rid in baseline:
            if rid != 'R007':
                self.assertEqual(results[rid], baseline[rid], rid)
        index(list(results.values()), {c['claim_id']: c})

    def test_engine_error_for_pack_drift_still_propagates(self):
        original = yara_engine.DETAIL_FUNCS['R001']
        yara_engine.DETAIL_FUNCS['R001'] = lambda claim, cfg: {'facts': ['R001:NEVER_MATCHES'], 'evidence_paths': ['/claim_id'],
                                                               'line_ids': [], 'message': 'x'}
        try:
            with self.assertRaises(EngineError):
                evaluate(self.claims[0], self.cfg)
        finally:
            yara_engine.DETAIL_FUNCS['R001'] = original

    def test_unresolvable_evidence_path_is_dropped_not_fatal(self):
        c = copy.deepcopy(self.claims[0])
        got = yara_engine._safe_evidence(c, ['/lines/99/quantity', '/nope', '/claim_id'])
        self.assertEqual(got, [{'path': '/claim_id', 'value': c['claim_id']}])
        self.assertEqual(yara_engine._safe_evidence(c, ['/nope']), [{'path': '/claim_id', 'value': c['claim_id']}])


class SeededFuzzTests(Base):
    def test_no_mutation_of_a_transport_valid_claim_crashes_or_trips_isolation(self):
        rng = random.Random(20260923)
        pool = self.claims[:40]
        mutants = 0
        for c in pool:
            paths = field_paths(c)
            for _ in range(90):
                m = copy.deepcopy(c)
                for _ in range(rng.choice((1, 1, 2, 3))):
                    p = rng.choice(paths)
                    set_path(m, p, rng.choice([None] + BAD_STR + BAD_DATES + BAD_NUMS))
                try:
                    validate_transport(m)
                except Exception:
                    continue  # would be quarantined at ingestion, never reaches the engine
                results, errors = self.check(m, f'{c["claim_id"]}')
                self.assertEqual(errors, [], f'isolation fired for {c["claim_id"]}: {errors}')
                mutants += 1
        self.assertGreater(mutants, 1500, mutants)

    def test_empty_lines_is_rejected_at_transport_but_the_engine_still_does_not_crash(self):
        m = copy.deepcopy(self.claims[0]); m['lines'] = []
        with self.assertRaises(ValueError):
            validate_transport(m)
        results, errors = self.check(m)  # defence in depth: reached only by bypassing ingestion
        self.assertEqual(errors, [])
        self.assertNotEqual(next(r for r in results if r['rule_id'] == 'R014')['status'], 'PASS')

    def test_duplicate_line_ids_are_rejected_at_transport(self):
        m = copy.deepcopy(self.claims[0]); m['lines'].append(copy.deepcopy(m['lines'][0]))
        with self.assertRaises(ValueError):
            validate_transport(m)

    def test_structural_edge_cases(self):
        c = self.claims[0]
        cases = {
            'no auths': lambda m: m.__setitem__('authorizations', []),
            'no attachments': lambda m: m.__setitem__('attachments', []),
            '200 lines': lambda m: m.__setitem__('lines', [dict(m['lines'][0], line_id=f'L{i}') for i in range(200)]),
            'unknown policy': lambda m: m.__setitem__('policy_id', 'EDU-GHOST'),
        }
        for label, fn in cases.items():
            m = copy.deepcopy(c)
            fn(m)
            validate_transport(m)
            results, errors = self.check(m, label)
            self.assertEqual(errors, [], label)


class AccuracyGateTests(Base):
    """Hardening must not change a single status on the public splits."""

    def test_all_public_splits_stay_at_perfect_status_accuracy(self):
        for split in ('development', 'validation', 'stress'):
            claims = load_jsonl(ROOT / f'data/{split}/claims.jsonl')
            gold = load_jsonl(ROOT / f'data/{split}/expected_results.jsonl')
            pred = [r for c in claims for r in evaluate(c, self.cfg)]
            report = score(gold, pred, {c['claim_id']: c for c in claims})
            self.assertEqual(report['overall']['status_accuracy'], 1.0, split)
            self.assertEqual(report['claims_with_all_statuses_correct'], len(claims), split)


if __name__ == '__main__':
    unittest.main()
