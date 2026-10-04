"""Fuzz the ingestion boundary: hostile bytes, hostile JSON, hostile FHIR, hostile CSV."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
from hypothesis import given, strategies as st
from engine_core import load_jsonl, validate_transport
from ingest import CSV_FOLDER, FHIR, NORMALIZED, ingest

GOOD = load_jsonl(ROOT / 'data/development/claims.jsonl')[:3]
GOOD_LINES = [json.dumps(c).encode('utf-8') for c in GOOD]
BUNDLE = (ROOT / 'data/development/fhir_bundles.jsonl').read_bytes().split(b'\n')[0]


def run(data, fmt, name='in.jsonl'):
    with tempfile.TemporaryDirectory() as tmp:
        p = Path(tmp) / name
        p.write_bytes(data)
        return list(ingest(p, fmt))


def check_contract(test, items):
    for it in items:
        test.assertNotEqual(it.claim is None, it.error is None, 'exactly one of claim/error must be set')
        if it.accepted:
            validate_transport(it.claim)


class IngestFuzz(unittest.TestCase):
    @given(fs.byte_edits(b'\n'.join(GOOD_LINES) + b'\n'))
    def test_mutated_jsonl_never_raises_and_keeps_the_contract(self, data):
        check_contract(self, run(data, NORMALIZED))

    @given(st.lists(st.binary(max_size=200).filter(lambda b: b'\n' not in b and b'\r' not in b), max_size=6))
    def test_garbage_lines_never_cost_the_valid_neighbours(self, garbage):
        lines = [GOOD_LINES[0]] + [x for pair in zip(garbage, GOOD_LINES[1:] * 3) for x in pair]
        items = run(b'\n'.join(lines) + b'\n', NORMALIZED)
        check_contract(self, items)
        want = {GOOD[0]['claim_id']} | {json.loads(l)['claim_id'] for l in lines[1:] if l in GOOD_LINES}
        self.assertTrue(want <= {i.claim['claim_id'] for i in items if i.accepted})

    @given(fs.json_values)
    def test_any_json_value_as_a_record_is_accepted_or_quarantined(self, value):
        check_contract(self, run(json.dumps(value).encode('utf-8') + b'\n', NORMALIZED))

    @given(fs.byte_edits(BUNDLE + b'\n'))
    def test_mutated_fhir_bundles_never_raise(self, data):
        check_contract(self, run(data, FHIR))

    @given(fs.json_values)
    def test_arbitrary_json_as_a_fhir_bundle_never_raises(self, value):
        check_contract(self, run(json.dumps({'resourceType': 'Bundle', 'entry': value}).encode() + b'\n', FHIR))

    @given(st.lists(st.lists(fs.hostile_text.filter(lambda s: '\x00' not in s), min_size=1, max_size=4), max_size=5))
    def test_hostile_csv_folder_never_raises(self, rows):
        with tempfile.TemporaryDirectory() as tmp:
            src = ROOT / 'data/development/csv'
            for f in src.glob('*.csv'):
                (Path(tmp) / f.name).write_bytes(f.read_bytes())
            header = (src / 'claims.csv').read_text(encoding='utf-8-sig').split('\n')[0]
            body = '\n'.join(','.join(json.dumps(c) for c in r) for r in rows)
            (Path(tmp) / 'claims.csv').write_text(header + '\n' + body + '\n', encoding='utf-8')
            check_contract(self, list(ingest(tmp, CSV_FOLDER)))


if __name__ == '__main__':
    unittest.main()
