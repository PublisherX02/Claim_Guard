import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import run_extensions as cli
from ext_world import IDS, claim, line


def write(path, claims):
    path.write_text(''.join(json.dumps(c) + '\n' for c in claims), encoding='utf-8')


class CliTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_eight_results_per_claim_are_written_in_claim_then_rule_order(self):
        write(self.dir / 'c.jsonl', [claim([line(1, 'SVC-LAB')], claim_id='C-2'), claim([line(1, 'SVC-LAB')], claim_id='C-1')])
        self.assertEqual(cli.main(['--claims', str(self.dir / 'c.jsonl'), '--out', str(self.dir / 'o.jsonl')]), 0)
        rows = [json.loads(l) for l in (self.dir / 'o.jsonl').read_text(encoding='utf-8').splitlines()]
        self.assertEqual([(r['claim_id'], r['rule_id']) for r in rows], [(c, r) for c in ('C-2', 'C-1') for r in IDS])

    def test_the_default_history_is_the_claim_file_itself(self):
        a = claim([line(1, 'SVC-LAB', quantity=2)], claim_id='A', date='2026-03-01')
        b = claim([line(1, 'SVC-LAB', quantity=2)], claim_id='B', date='2026-03-02')
        write(self.dir / 'c.jsonl', [a, b])
        cli.main(['--claims', str(self.dir / 'c.jsonl'), '--out', str(self.dir / 'o.jsonl')])
        rows = [json.loads(l) for l in (self.dir / 'o.jsonl').read_text(encoding='utf-8').splitlines()]
        status = {(r['claim_id'], r['rule_id']): r['status'] for r in rows}
        self.assertEqual((status[('A', 'E101')], status[('B', 'E101')]), ('PASS', 'FAIL'))

    def test_no_history_makes_the_cross_claim_rules_unable(self):
        write(self.dir / 'c.jsonl', [claim([line(1, 'SVC-LAB')])])
        cli.main(['--claims', str(self.dir / 'c.jsonl'), '--no-history', '--out', str(self.dir / 'o.jsonl')])
        rows = [json.loads(l) for l in (self.dir / 'o.jsonl').read_text(encoding='utf-8').splitlines()]
        self.assertEqual({r['status'] for r in rows if r['rule_id'] == 'E101'}, {'UNABLE_TO_ASSESS'})

    def test_the_output_is_deterministic(self):
        write(self.dir / 'c.jsonl', [claim([line(1, 'SVC-EXT-PRIMARY'), line(2, 'SVC-EXT-COMPONENT')])])
        for name in ('1', '2'):
            cli.main(['--claims', str(self.dir / 'c.jsonl'), '--out', str(self.dir / f'{name}.jsonl')])
        self.assertEqual((self.dir / '1.jsonl').read_bytes(), (self.dir / '2.jsonl').read_bytes())

    def test_a_missing_or_malformed_input_is_rejected_with_exit_code_two(self):
        for path in (self.dir / 'nope.jsonl',):
            with self.assertRaises(SystemExit) as cm:
                cli.main(['--claims', str(path), '--out', str(self.dir / 'o.jsonl')])
            self.assertEqual(cm.exception.code, 2)
        (self.dir / 'bad.jsonl').write_text('{not json\n', encoding='utf-8')
        with self.assertRaises(SystemExit) as cm:
            cli.main(['--claims', str(self.dir / 'bad.jsonl'), '--out', str(self.dir / 'o.jsonl')])
        self.assertEqual(cm.exception.code, 2)

    def test_the_official_outputs_are_never_written(self):
        before = {p: p.stat().st_mtime_ns for p in (ROOT / 'outputs').glob('dev_*.json*')}
        write(self.dir / 'c.jsonl', [claim([line(1, 'SVC-LAB')])])
        cli.main(['--claims', str(self.dir / 'c.jsonl'), '--out', str(self.dir / 'o.jsonl')])
        self.assertEqual(before, {p: p.stat().st_mtime_ns for p in (ROOT / 'outputs').glob('dev_*.json*')})


if __name__ == '__main__':
    unittest.main()
