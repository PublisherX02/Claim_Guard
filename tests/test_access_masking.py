import copy
import json
import logging
import re
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from access import claimstore, masking, permissions
from engine_core import config, load_jsonl

KEY = 'm' * 40
L1, L2, L3 = (permissions.LEVEL_DEFAULTS[i] for i in (1, 2, 3))


def public_claims():
    return [c for sp in ('development', 'validation', 'stress') for c in load_jsonl(ROOT / 'data' / sp / 'claims.jsonl')]


def raw_ids_in(claim):
    """An independent way to find every identifier in a claim: by pattern in its raw text, not through identifier_map."""
    return set(re.findall(r'(?:PAT|MEM)-[0-9A-F]{6,}', json.dumps(claim)))


def crafted_claim():
    return {
        'claim_id': 'CG-TEST', 'patient_id': 'PAT-AAA111', 'member_id': 'MEM-BBB222', 'notes': 'Call PAT-AAA111 on 555-0100',
        'coverage': {'beneficiary_patient_id': 'PAT-AAA111', 'member_id': 'MEM-BBB222', 'status': 'active'},
        'authorizations': [{'authorization_id': 'AUTH-1', 'patient_id': 'PAT-AAA111'}],
        'attachments': [{'attachment_id': 'D1', 'patient_id': 'PAT-CCC333', 'text': 'Patient PAT-CCC333 / MEM-BBB222 seen', 'type': 'service-note'}],
        'lines': [{'line_id': 'L1', 'quantity': 1}],
    }


def crafted_results():
    return [{'claim_id': 'CG-TEST', 'rule_id': 'R004', 'status': 'FAIL', 'severity': 'high',
             'explanation': 'Member MEM-BBB222 does not match beneficiary PAT-AAA111.',
             'evidence': [{'path': '/patient_id', 'value': 'PAT-AAA111'}, {'path': '/member_id', 'value': 'MEM-BBB222'},
                          {'path': '/notes', 'value': 'Call PAT-AAA111 on 555-0100'},
                          {'path': '/attachments/0/text', 'value': 'Patient PAT-CCC333 seen'},
                          {'path': '/lines', 'value': [{'line_id': 'L1', 'ref': 'for PAT-AAA111'}]}]}]


class PseudonymTests(unittest.TestCase):
    def test_format_stability_and_key_dependence(self):
        a = masking.pseudonym(KEY, 'PAT', 'PAT-AAA111')
        self.assertRegex(a, r'^PAT-[0-9A-F]{8}$')
        self.assertEqual(a, masking.pseudonym(KEY, 'PAT', 'PAT-AAA111'))
        self.assertNotEqual(a, masking.pseudonym('n' * 40, 'PAT', 'PAT-AAA111'))
        self.assertNotEqual(a, masking.pseudonym(KEY, 'PAT', 'PAT-AAA112'))
        self.assertNotEqual(masking.pseudonym(KEY, 'PAT', 'X'), masking.pseudonym(KEY, 'MEM', 'X'))

    def test_a_pseudonym_does_not_contain_the_original_or_its_suffix(self):
        raw = 'PAT-03B3FEC24A'
        p = masking.pseudonym(KEY, 'PAT', raw)
        self.assertNotIn('03B3FEC24A', p)

    def test_public_patients_and_members_get_distinct_pseudonyms(self):
        raws = {(c['patient_id'], 'PAT') for c in public_claims()} | {(c['member_id'], 'MEM') for c in public_claims() if c['member_id']}
        pseudo = {masking.pseudonym(KEY, prefix, raw) for raw, prefix in raws}
        self.assertEqual(len(pseudo), len(raws))

    def test_bad_input_is_an_error(self):
        for args in ((KEY, 'PAT', None), (KEY, 'PAT', 5), (None, 'PAT', 'x'), ('short', 'PAT', 'x'), (KEY, 'BAD', 'x')):
            with self.assertRaises(ValueError, msg=repr(args)):
                masking.pseudonym(*args)


class MapAndScrubTests(unittest.TestCase):
    def test_every_identifier_field_is_mapped(self):
        m = masking.identifier_map(crafted_claim(), KEY)
        self.assertEqual(set(m), {'PAT-AAA111', 'MEM-BBB222', 'PAT-CCC333'})
        self.assertTrue(m['PAT-AAA111'].startswith('PAT-') and m['MEM-BBB222'].startswith('MEM-'))

    def test_scrub_replaces_inside_nested_values_and_free_text_without_touching_the_input(self):
        original = crafted_results()
        before = copy.deepcopy(original)
        m = masking.identifier_map(crafted_claim(), KEY)
        out = masking.scrub(original, m)
        self.assertEqual(original, before)
        text = json.dumps(out)
        for raw in m:
            self.assertNotIn(raw, text)
        self.assertIn(m['MEM-BBB222'], out[0]['explanation'])

    def test_scrub_leaves_other_values_alone(self):
        self.assertEqual(masking.scrub({'a': [1, 2.5, None, True, 'plain']}, {'PAT-X': 'PAT-Y'}), {'a': [1, 2.5, None, True, 'plain']})

    def test_longer_identifiers_are_replaced_before_their_prefixes(self):
        out = masking.scrub('PAT-12345 and PAT-123', {'PAT-123': 'PAT-AAAAAAAA', 'PAT-12345': 'PAT-BBBBBBBB'})
        self.assertEqual(out, 'PAT-BBBBBBBB and PAT-AAAAAAAA')

    def test_a_differently_cased_copy_of_an_identifier_is_scrubbed_and_detected(self):
        m = {'pat-aaa111': 'PAT-DEADBEEF'}
        for variant in ('pat-aaa111', 'PAT-AAA111', 'Pat-Aaa111'):
            out = masking.scrub({'notes': f'Called patient {variant} today'}, m)
            self.assertNotIn(variant, json.dumps(out), variant)
            self.assertIn('PAT-DEADBEEF', json.dumps(out))
            self.assertEqual(masking.leaks({'x': f'see {variant}'}, {'pat-aaa111'}), {'pat-aaa111'}, variant)

    def test_regex_metacharacters_in_an_identifier_are_matched_literally(self):
        m = {'A.B+C(1)': 'PAT-DEADBEEF'}
        self.assertEqual(masking.scrub('x A.B+C(1) y AxBBC1 z', m), 'x PAT-DEADBEEF y AxBBC1 z')
        self.assertEqual(masking.leaks('AxBBC1', {'A.B+C(1)'}), set())

    def test_leaks_finds_a_planted_identifier_anywhere(self):
        self.assertEqual(masking.leaks({'a': [{'b': 'x PAT-1 y'}]}, {'PAT-1', 'PAT-2'}), {'PAT-1'})
        self.assertEqual(masking.leaks({'a': 'clean'}, {'PAT-1'}), set())


class ShapeTests(unittest.TestCase):
    def shape(self, perms, unmasked=False):
        return masking.shape_claim(crafted_claim(), crafted_results(), perms, KEY, unmasked=unmasked)

    def test_the_viewer_gets_no_notes_no_attachment_text_and_no_raw_identifiers(self):
        shaped = self.shape(L1)
        text = json.dumps(shaped)
        for raw in ('PAT-AAA111', 'MEM-BBB222', 'PAT-CCC333', '555-0100'):
            self.assertNotIn(raw, text)
        self.assertNotIn('notes', shaped['claim'])
        self.assertTrue(all('text' not in a for a in shaped['claim']['attachments']))
        paths = [e['path'] for e in shaped['results'][0]['evidence']]
        self.assertNotIn('/notes', paths)
        self.assertNotIn('/attachments/0/text', paths)
        self.assertIn('/patient_id', paths)

    def test_a_reviewer_sees_notes_and_attachment_text_but_still_masked_identifiers(self):
        shaped = self.shape(L2)
        self.assertIn('notes', shaped['claim'])
        self.assertTrue(all('text' in a for a in shaped['claim']['attachments']))
        text = json.dumps(shaped)
        for raw in ('PAT-AAA111', 'MEM-BBB222', 'PAT-CCC333'):
            self.assertNotIn(raw, text)
        self.assertIn('/notes', [e['path'] for e in shaped['results'][0]['evidence']])

    def test_unmasked_returns_the_real_identifiers(self):
        text = json.dumps(self.shape(L2, unmasked=True))
        for raw in ('PAT-AAA111', 'MEM-BBB222', 'PAT-CCC333'):
            self.assertIn(raw, text)

    def test_shaping_never_changes_its_inputs(self):
        claim, results = crafted_claim(), crafted_results()
        b1, b2 = copy.deepcopy(claim), copy.deepcopy(results)
        masking.shape_claim(claim, results, L1, KEY)
        self.assertEqual((claim, results), (b1, b2))

    def test_a_redaction_failure_is_an_error_not_a_leak(self):
        with mock.patch.object(masking, 'scrub', lambda obj, mapping: obj):
            with self.assertRaises(masking.RedactionError):
                self.shape(L1)

    def test_every_public_claim_at_every_level_leaks_no_identifier_when_masked(self):
        logging.disable(logging.WARNING)
        try:
            cfg = config(ROOT)
            from yara_engine import evaluate
            checked = 0
            for claim in public_claims():
                results = evaluate(claim, cfg)
                raws = raw_ids_in(claim)
                self.assertGreaterEqual(len(raws), 1)
                for perms in (L1, L2, L3):
                    text = json.dumps(masking.shape_claim(claim, results, perms, KEY))
                    for raw in raws:
                        self.assertNotIn(raw, text, claim['claim_id'])
                    checked += 1
            self.assertEqual(checked, 3 * len(public_claims()))
        finally:
            logging.disable(logging.NOTSET)

    def test_engine_text_never_contains_a_raw_identifier_outside_evidence(self):
        logging.disable(logging.WARNING)
        try:
            from yara_engine import evaluate
            cfg = config(ROOT)
            for claim in public_claims():
                raws = raw_ids_in(claim)
                for r in evaluate(claim, cfg):
                    rest = {k: v for k, v in r.items() if k != 'evidence'}
                    self.assertEqual(masking.leaks(rest, raws), set(), (claim['claim_id'], r['rule_id']))
        finally:
            logging.disable(logging.NOTSET)


class ClaimStoreTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        logging.disable(logging.WARNING)
        cls.store = claimstore.FileClaimStore.from_jsonl(ROOT / 'data' / 'stress' / 'claims.jsonl', config(ROOT))

    @classmethod
    def tearDownClass(cls):
        logging.disable(logging.NOTSET)

    def test_summaries_list_every_claim_with_counts_and_no_identifiers(self):
        rows = self.store.summaries(limit=200)
        self.assertEqual(len(rows), 50)
        self.assertEqual(set(rows[0]), {'claim_id', 'counts', 'max_severity', 'flagged'})
        raws = {i for c in public_claims() for i in raw_ids_in(c)}
        self.assertEqual(masking.leaks(rows, raws), set())

    def test_pagination_and_validation(self):
        a = self.store.summaries(limit=10, offset=0)
        b = self.store.summaries(limit=10, offset=10)
        self.assertEqual(len(a), 10)
        self.assertFalse({r['claim_id'] for r in a} & {r['claim_id'] for r in b})
        self.assertEqual(self.store.summaries(limit=10, offset=500), [])
        for kw in (dict(limit=0), dict(limit=201), dict(limit='x'), dict(offset=-1), dict(status='MAYBE'), dict(rule_id='R1'),
                   dict(rule_id='R001; drop'), dict(status=5)):
            with self.assertRaises(ValueError, msg=str(kw)):
                self.store.summaries(**kw)

    def test_filters(self):
        fails = self.store.summaries(limit=200, status='FAIL')
        self.assertTrue(0 < len(fails) < 50)
        self.assertTrue(all(r['counts'].get('FAIL', 0) > 0 for r in fails))
        r9 = self.store.summaries(limit=200, rule_id='R009')
        for row in r9:
            _, results = self.store.get(row['claim_id'])
            self.assertIn(next(x for x in results if x['rule_id'] == 'R009')['status'], ('FAIL', 'UNABLE_TO_ASSESS'))
        both = self.store.summaries(limit=200, rule_id='R009', status='FAIL')
        self.assertTrue(all(x['claim_id'] in {y['claim_id'] for y in fails} for x in both))

    def test_get_returns_copies_and_refuses_odd_ids(self):
        cid = self.store.summaries(limit=1)[0]['claim_id']
        claim, results = self.store.get(cid)
        claim['patient_id'] = 'CHANGED'
        results.clear()
        again, again_results = self.store.get(cid)
        self.assertNotEqual(again['patient_id'], 'CHANGED')
        self.assertTrue(again_results)
        for bad in ('CG-NOPE', '', '../etc/passwd', 'a' * 500, None, 5, {'$ne': 1}, ['x'], cid + '\n', cid.lower()):
            self.assertIsNone(self.store.get(bad), repr(bad)[:30])


if __name__ == '__main__':
    unittest.main()
