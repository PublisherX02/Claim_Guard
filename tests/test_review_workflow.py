import unittest, unittest.mock, sys, json, copy, tempfile
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from engine_core import config
from audit import verify
from audit_log import AuditLog, audited_review, verify_ai_ordering, verify_with_anchor
from llm_adapter import MockExplanationProvider
from review_workflow import (DecisionError, apply_decisions, load_decisions, recheck,
                             review_state, unresolved_counts, validate_decision, index_findings)


class ReviewWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cfg = config(ROOT)
        cls.clean = json.loads((ROOT / 'examples/worked_cases.json').read_text())[0]['claim']

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / 'audit.jsonl'
        self.log = AuditLog(self.path)
        self.claim = copy.deepcopy(self.clean)
        self.claim['invoice_number'] = None  # -> R001 FAIL
        self.results, _, self.trace = audited_review(self.log, copy.deepcopy(self.claim), self.cfg,
                                                     provider=MockExplanationProvider())
        self.findings = index_findings(self.results)

    def tearDown(self):
        self.tmp.cleanup()

    def decision(self, **over):
        d = {'claim_id': self.claim['claim_id'], 'rule_id': 'R001', 'action': 'confirm_issue',
             'actor': 'reviewer-1', 'reason': 'Invoice number really is blank on the source',
             'created_at': '2026-09-23T16:00:00.000Z', 'original_status': 'FAIL'}
        d.update(over)
        return d

    # --- validation -------------------------------------------------------
    def test_shape_the_review_page_downloads_is_accepted(self):
        validate_decision(self.decision(), self.findings)

    def test_status_mismatch_is_rejected(self):
        with self.assertRaisesRegex(DecisionError, 'does not match'):
            validate_decision(self.decision(original_status='UNABLE_TO_ASSESS'), self.findings)

    def test_decision_on_a_passing_finding_is_rejected(self):
        self.assertEqual(self.findings[(self.claim['claim_id'], 'R002')]['status'], 'PASS')
        with self.assertRaisesRegex(DecisionError, 'only'):
            validate_decision(self.decision(rule_id='R002', original_status='PASS'), self.findings)

    def test_unknown_finding_blank_reason_and_extra_fields_are_rejected(self):
        for bad in (self.decision(claim_id='CG-NOPE'), self.decision(reason='   '),
                    self.decision(actor=' '), self.decision(extra='x'),
                    self.decision(action='approve_claim'), self.decision(created_at='yesterday')):
            with self.assertRaises(DecisionError, msg=str(bad)):
                validate_decision(bad, self.findings)

    def test_one_bad_decision_rejects_the_whole_batch(self):
        before = self.log.count
        with self.assertRaises(DecisionError):
            apply_decisions(self.log, [self.decision(), self.decision(original_status='PASS')], self.results)
        self.assertEqual(self.log.count, before)

    # --- recording and queue state ---------------------------------------
    def test_decisions_are_recorded_and_never_mutate_results(self):
        snapshot = copy.deepcopy(self.results)
        apply_decisions(self.log, [self.decision()], self.results)
        self.assertEqual(self.results, snapshot)
        self.assertEqual(verify_with_anchor(self.path)[1], self.log.count)

    def test_unresolved_counts_track_each_action(self):
        before = unresolved_counts(self.results, self.path)
        self.assertEqual(before['resolved'], 0)
        self.assertEqual(before['unresolved_total'], before['unreviewed'])
        apply_decisions(self.log, [self.decision(action='request_information')], self.results)
        mid = unresolved_counts(self.results, self.path)
        self.assertEqual(mid['awaiting_follow_up'], 1)
        self.assertEqual(mid['unresolved_total'], before['unresolved_total'])  # still unresolved
        apply_decisions(self.log, [self.decision(action='dismiss_with_reason')], self.results)
        after = unresolved_counts(self.results, self.path)
        self.assertEqual(after['resolved'], 1)
        self.assertEqual(after['unresolved_total'], before['unresolved_total'] - 1)

    def test_load_decisions_reads_downloaded_jsonl(self):
        p = Path(self.tmp.name) / 'review_decisions.jsonl'
        p.write_text(json.dumps(self.decision()) + '\n' + json.dumps(self.decision(action='request_information')) + '\n')
        self.assertEqual(len(load_decisions(p)), 2)

    # --- recheck ----------------------------------------------------------
    def corrected(self):
        c = copy.deepcopy(self.claim)
        c['invoice_number'] = 'INV-FIXED-1'
        return c

    def test_recheck_creates_a_new_version_and_run_and_leaves_the_original_alone(self):
        original_claim = copy.deepcopy(self.claim)
        prior_results = copy.deepcopy(self.results)
        new_results, _, new_trace, changes = recheck(
            self.log, self.claim, self.corrected(), self.results, self.trace, self.cfg,
            ['R001'], 'reviewer-1', 'Supplier sent the invoice number', provider=MockExplanationProvider())
        self.assertEqual(changes, {'R001': ('FAIL', 'PASS')})
        self.assertNotEqual(new_trace['run_id'], self.trace['run_id'])
        self.assertNotEqual(new_trace['input_hash'], self.trace['input_hash'])
        self.assertEqual(self.claim, original_claim)
        self.assertEqual(self.results, prior_results)
        link = [json.loads(l)['event'] for l in self.path.read_text().splitlines()
                if json.loads(l)['event'].get('event_type') == 'recheck_run']
        self.assertEqual(len(link), 1)
        self.assertEqual((link[0]['prior_run_id'], link[0]['new_run_id']),
                         (self.trace['run_id'], new_trace['run_id']))
        self.assertEqual(verify_with_anchor(self.path)[1], self.log.count)
        verify_ai_ordering(self.path)
        actions = [json.loads(l)['event'].get('action') for l in self.path.read_text().splitlines()]
        self.assertIn('mark_corrected_for_recheck', actions)

    def test_recheck_that_does_not_fix_the_problem_is_reported_honestly(self):
        still_broken = copy.deepcopy(self.claim)
        still_broken['invoice_number'] = ''  # different bytes, still empty -> R001 still FAILs
        new_results, _, _, changes = recheck(
            self.log, self.claim, still_broken, self.results, self.trace, self.cfg,
            ['R001'], 'reviewer-1', 'Tried a fix', provider=MockExplanationProvider())
        self.assertEqual(changes['R001'], ('FAIL', 'FAIL'))
        # the old decision must not carry over to the new run
        self.assertEqual(review_state(new_results, self.path)[(self.claim['claim_id'], 'R001')], 'unreviewed')

    def test_review_state_parses_an_unchanged_log_once_and_notices_every_append(self):
        key = (self.claim['claim_id'], 'R001')
        reads = []
        real = Path.read_text
        def counting(path, *args, **kw):
            if Path(path) == self.path:
                reads.append(1)
            return real(path, *args, **kw)
        def reads_during(call):
            before = len(reads)
            out = call()
            return out, len(reads) - before
        with unittest.mock.patch.object(Path, 'read_text', counting):
            first, n1 = reads_during(lambda: review_state(self.results, self.path))
            second, n2 = reads_during(lambda: review_state(self.results, self.path))
            self.assertEqual((first[key], second[key], n1, n2), ('unreviewed', 'unreviewed', 1, 0))
            apply_decisions(self.log, [self.decision()], self.results)
            third, n3 = reads_during(lambda: review_state(self.results, self.path))
            fourth, n4 = reads_during(lambda: review_state(self.results, self.path))
            self.assertEqual((third[key], fourth[key], n3, n4), ('resolved', 'resolved', 1, 0))

    def test_review_state_on_a_missing_log_is_empty_of_decisions(self):
        self.assertEqual(review_state(self.results, Path(self.tmp.name) / 'nope.jsonl')[(self.claim['claim_id'], 'R001')], 'unreviewed')

    def test_recheck_rejects_identical_or_reidentified_claims_and_unknown_rules(self):
        with self.assertRaises(DecisionError):
            recheck(self.log, self.claim, copy.deepcopy(self.claim), self.results, self.trace, self.cfg,
                    ['R001'], 'r', 'x', provider=MockExplanationProvider())
        other = self.corrected(); other['claim_id'] = 'CG-SOMEONE-ELSE'
        with self.assertRaises(DecisionError):
            recheck(self.log, self.claim, other, self.results, self.trace, self.cfg,
                    ['R001'], 'r', 'x', provider=MockExplanationProvider())
        with self.assertRaises(DecisionError):
            recheck(self.log, self.claim, self.corrected(), self.results, self.trace, self.cfg,
                    ['R999'], 'r', 'x', provider=MockExplanationProvider())
        self.assertEqual(verify(self.path)[1], self.log.count)

    def test_recheck_of_a_passing_rule_is_rejected(self):
        with self.assertRaises(DecisionError):
            recheck(self.log, self.claim, self.corrected(), self.results, self.trace, self.cfg,
                    ['R002'], 'r', 'x', provider=MockExplanationProvider())


if __name__ == '__main__':
    unittest.main()
