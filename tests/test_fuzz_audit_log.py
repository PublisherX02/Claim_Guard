"""Any change to a finished audit log must be caught by strict verification."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import fuzz_strategies as fs
from hypothesis import assume, given, strategies as st
from audit import digest
from audit_log import AuditLog, verify_with_anchor


def make_log(tmp, n=8):
    path = Path(tmp) / 'audit.jsonl'
    AuditLog(path)._write([{'event_type': 'rule_check', 'claim_id': f'CG-{i}', 'rule_id': 'R001', 'status': 'PASS'} for i in range(n)])
    return path


def detected(path):
    try:
        verify_with_anchor(path, strict=True)
    except ValueError:
        return True
    return False


class AuditTamperFuzz(unittest.TestCase):
    @given(st.integers(min_value=0), st.integers(0, 255))
    def test_flipping_any_byte_is_detected(self, where, value):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            raw = bytearray(path.read_bytes())
            i = where % len(raw)
            assume(raw[i] != value)
            raw[i] = value
            path.write_bytes(bytes(raw))
            self.assertTrue(detected(path))

    @given(st.integers(0, 7))
    def test_deleting_any_row_is_detected(self, k):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            lines = path.read_bytes().split(b'\n')
            del lines[k]
            path.write_bytes(b'\n'.join(lines))
            self.assertTrue(detected(path))

    @given(st.integers(0, 7), st.integers(0, 7))
    def test_swapping_two_rows_is_detected(self, a, b):
        assume(a != b)
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            lines = path.read_bytes().split(b'\n')
            lines[a], lines[b] = lines[b], lines[a]
            path.write_bytes(b'\n'.join(lines))
            self.assertTrue(detected(path))

    @given(st.integers(1, 5), fs.hostile_text)
    def test_appending_validly_chained_forged_rows_is_detected_by_strict(self, n, text):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            rows = [json.loads(l) for l in path.read_text(encoding='utf-8').splitlines() if l.strip()]
            for i in range(n):
                row = {'sequence': len(rows) + 1, 'recorded_at': 'x', 'previous_hash': rows[-1]['hash'],
                       'event': {'event_type': 'rule_check', 'claim_id': text, 'rule_id': 'R001', 'status': 'PASS'}}
                rows.append({**row, 'hash': digest(row)})
            path.write_text(''.join(json.dumps(r) + '\n' for r in rows), encoding='utf-8')
            self.assertTrue(detected(path))

    @given(fs.byte_edits(b'x'))
    def test_a_corrupted_anchor_never_passes_and_never_raises_anything_but_valueerror(self, junk):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            path.with_name(path.name + '.head.json').write_bytes(junk)
            self.assertTrue(detected(path))

    @given(st.integers(0, 7))
    def test_truncating_the_file_is_detected(self, keep):
        with tempfile.TemporaryDirectory() as tmp:
            path = make_log(tmp)
            lines = path.read_bytes().split(b'\n')
            path.write_bytes(b'\n'.join(lines[:keep]) + (b'\n' if keep else b''))
            self.assertTrue(detected(path))


class MalformedRecordsAreChainErrors(unittest.TestCase):
    """Found by fuzzing: a record that is valid JSON but not a valid audit row raised KeyError/AttributeError/TypeError
    from audit.verify instead of the documented ValueError, so verify_audit.py printed a traceback, not 'chain invalid'."""
    CASES = {
        'previous_hash key renamed': lambda row: {**{k: v for k, v in row.items() if k != 'previous_hash'}, 'previous_hasH': row['previous_hash']},
        'hash key removed': lambda row: {k: v for k, v in row.items() if k != 'hash'},
        'row is a list': lambda row: [row],
        'row is a number': lambda row: 7,
        'row is null': lambda row: None,
        'row is a string': lambda row: 'x',
    }

    def test_every_malformed_record_shape_raises_valueerror(self):
        for label, break_row in self.CASES.items():
            with self.subTest(label), tempfile.TemporaryDirectory() as tmp:
                path = make_log(tmp)
                lines = path.read_text(encoding='utf-8').split('\n')
                lines[3] = json.dumps(break_row(json.loads(lines[3])))
                path.write_text('\n'.join(lines), encoding='utf-8')
                with self.assertRaisesRegex(ValueError, 'Audit chain invalid at event 4'):
                    verify_with_anchor(path, strict=True)


if __name__ == '__main__':
    unittest.main()
