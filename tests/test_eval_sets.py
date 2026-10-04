import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
import eval_sets as es
from engine_core import config
from yara_engine import evaluate


def drain(s):
    return list(s.items())


class GitHeadTests(unittest.TestCase):
    def test_reads_a_branch_ref(self):
        with tempfile.TemporaryDirectory() as tmp:
            g = Path(tmp) / '.git'
            (g / 'refs' / 'heads').mkdir(parents=True)
            (g / 'HEAD').write_text('ref: refs/heads/x\n')
            (g / 'refs' / 'heads' / 'x').write_text('abc123\n')
            self.assertEqual(es.read_git_head(tmp), 'abc123')

    def test_reads_a_detached_head(self):
        with tempfile.TemporaryDirectory() as tmp:
            g = Path(tmp) / '.git'
            g.mkdir()
            (g / 'HEAD').write_text('def456\n')
            self.assertEqual(es.read_git_head(tmp), 'def456')

    def test_reads_packed_refs(self):
        with tempfile.TemporaryDirectory() as tmp:
            g = Path(tmp) / '.git'
            g.mkdir()
            (g / 'HEAD').write_text('ref: refs/heads/y\n')
            (g / 'packed-refs').write_text('# pack-refs\n789abc refs/heads/y\n')
            self.assertEqual(es.read_git_head(tmp), '789abc')

    def test_unknown_when_there_is_no_git(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(es.read_git_head(tmp), 'unknown')

    def test_reads_this_checkout(self):
        if not (ROOT / '.git').exists():
            self.skipTest('not a git checkout (for example an unpacked zip)')
        head = es.read_git_head(ROOT)
        self.assertRegex(head, r'^[0-9a-f]{40}$')


class OrganizerSetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.sets = es.organizer_sets(ROOT)
        cls.by_id = {s.set_id: s for s in cls.sets}

    def test_ids_tiers_and_labels(self):
        self.assertEqual([s.set_id for s in self.sets], ['S1', 'S2', 'S3', 'S4'])
        for s in self.sets:
            self.assertEqual(s.tier, 'A')
            self.assertEqual(s.label_kind, 'organizer_key')

    def test_sizes_match_the_manifest(self):
        self.assertEqual(len(drain(self.by_id['S1'])), 400)
        self.assertEqual(len(drain(self.by_id['S2'])), 150)
        self.assertEqual(len(drain(self.by_id['S3'])), 50)

    def test_every_item_has_all_fifteen_rules(self):
        for claim, gold in drain(self.by_id['S3']):
            self.assertEqual(sorted(gold), list(es.RULES))

    def test_handbook_cases_are_deduplicated_against_the_splits(self):
        items = drain(self.by_id['S4'])
        dup = self.by_id['S4'].notes['duplicates_of_s1_s3']
        self.assertEqual(len(items) + dup, 10)

    def test_no_claim_is_counted_twice_across_tier_a(self):
        seen = set()
        for s in self.sets:
            for claim, _ in drain(s):
                h = es.digest(claim)
                self.assertNotIn(h, seen)
                seen.add(h)


class FormatVariantTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fhir, cls.csv = es.format_variant_sets(ROOT)

    def test_ids_and_paths(self):
        self.assertEqual((self.fhir.set_id, self.csv.set_id), ('S5', 'S6'))
        self.assertEqual((self.fhir.path, self.csv.path), ('ingest_fhir', 'ingest_csv'))

    def test_every_record_is_accepted_or_counted_as_quarantined(self):
        for s in (self.fhir, self.csv):
            items = drain(s)
            self.assertEqual(len(items) + s.notes['quarantined'], 600)
            self.assertEqual(s.notes['ingested'], len(items))

    def test_gold_comes_from_the_organizer_key_for_the_same_claim_id(self):
        organizer = {}
        for s in es.organizer_sets(ROOT)[:3]:
            for c, g in drain(s):
                organizer[c['claim_id']] = g
        for claim, gold in drain(self.csv)[:50]:
            self.assertEqual(gold, organizer[claim['claim_id']])


class BoundarySetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.s = es.boundary_set(ROOT)
        cls.items = drain(cls.s)

    def test_counts(self):
        self.assertEqual(len(self.items), 123)
        self.assertEqual(sum(len(g) for _, g in self.items), 140)
        self.assertEqual((self.s.tier, self.s.label_kind), ('C', 'hand_derived'))

    def test_claim_ids_are_unique(self):
        ids = [c['claim_id'] for c, _ in self.items]
        self.assertEqual(len(ids), len(set(ids)))

    def test_renaming_the_claim_changes_no_verdict(self):
        cfg = config(ROOT)
        claim, gold = self.items[0]
        a = {r['rule_id']: r['status'] for r in evaluate(claim, cfg)}
        renamed = dict(claim, claim_id='CG-OTHER')
        b = {r['rule_id']: r['status'] for r in evaluate(renamed, cfg)}
        self.assertEqual(a, b)


class ProvenanceTests(unittest.TestCase):
    def test_entry_has_the_citation_fields(self):
        s = es.organizer_sets(ROOT)[2]
        e = es.provenance_entry(s, ROOT, 'abc123', claims=50, results=750)
        for key in ('set_id', 'tier', 'name', 'label_kind', 'label_source', 'generator', 'limitation', 'files',
                    'claims', 'results', 'commit', 'notes'):
            self.assertIn(key, e)
        self.assertEqual(len(e['files'][0]['sha256']), 64)
        self.assertTrue(e['files'][0]['path'].startswith('data/'))


if __name__ == '__main__':
    unittest.main()
