# Phase 2 Evaluation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compute detection metrics (per-rule and macro F1 by category, claim-level false-positive rate, latency) over nine labelled example sets with a provenance manifest, and publish the test evaluation report `docs/29_Test_Evaluation_Report.md`.

**Architecture:** `src/eval_metrics.py` holds pure, streaming metric code (a `Tally` that consumes one claim at a time, so 107,635 generated claims never sit in memory). `scripts/eval_sets.py` loads the nine sets, each tagged with its evidence tier and label origin. `scripts/evaluate_phase2.py` runs the engine, scores every set, measures latency, and writes `outputs/evaluation/{metrics,provenance}.json`. `scripts/render_eval_report.py` fills marked tables in the report from those files so every number is traceable.

**Tech Stack:** Python 3.10 to 3.14, standard library only for new code, `unittest`, existing `yara_engine`, `tests/oracle.py`, `tests/claim_gen.py`, `src/ingest.py`.

**Spec:** `docs/superpowers/specs/2026-10-04-phase2-evaluation-design.md`

## Global Constraints

- New code uses only the standard library (no numpy or scipy). The existing runtime requirements are untouched.
- `scripts/` and `src/` must not contain the sink words in `tests/security_sinks.py` (`subprocess`, `pickle`, `marshal`, `shelve`, `os.system`, `os.popen`, bare `eval(`, `exec(`, `compile(`, `input(`, `__import__(`), not even in docstrings; `bandit -r src scripts -ll` must stay clean.
- Run single modules with `python -m unittest discover -s tests -p <file>`; the dotted `tests.<module>` form does not work here. Use `.venv/Scripts/python.exe` from the repo root.
- Results are deterministic given the fixed seeds (S7 seed 20260927, S8 seed 20261004); latency numbers are the only non-deterministic content and are labelled so.
- Positive class is `FAIL`, as in `src/evaluate.py`. `src/evaluate.py` is not modified.
- Never pool evidence tiers into one headline number. Tier A is the only external truth; tier B is agreement with our own oracle; tier C is hand-derived.
- `outputs/` is gitignored: `outputs/evaluation/*.json` must be added with `git add -f`.
- Existing 523 tests must still pass. Commits end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Do not push.

## Review Focus

- A rule with no `FAIL` in a set must give F1 "undefined, listed as excluded", never a silent 0 or a crash.
- Generated, mutated and boundary claims must each get a unique `claim_id`; a collision would overwrite a gold label and hide an error. Renaming must not change any verdict.
- A FHIR or CSV record that ingestion quarantines must reduce reported coverage, not be counted as correct and not crash the run.
- A claim present in more than one tier-A set must be counted once in the tier-A headline.
- A rule that crashes inside the engine (isolated, reported in `tool_errors`) must be counted and reported, not skipped.

---

### Task 1: Streaming metrics module

**Files:**
- Create: `src/eval_metrics.py`
- Test: `tests/test_eval_metrics.py`

**Interfaces:**
- Produces:
  - constants `POSITIVE='FAIL'`, `RULES` (tuple R001..R015), `CATEGORIES` (dict name -> tuple of rule ids), `CLEAN_STATUSES=('PASS','NOT_APPLICABLE')`
  - `severity_groups(rules: list[dict]) -> dict[str, tuple[str, ...]]` (from `rules/rules.json` entries with `rule_id`, `severity`)
  - `prf(counts: dict) -> dict` with keys `precision`, `recall`, `f1` (each `None` when undefined; f1 is `None` only when tp+fp+fn == 0)
  - `macro(values: dict[str, float | None]) -> dict` with `macro_f1`, `defined`, `excluded`
  - `clopper_pearson_upper(k: int, n: int, confidence: float = 0.95) -> float | None` (one-sided exact upper bound; Wilson fallback when k > 2000)
  - `percentile(values: list[float], q: float) -> float | None` (nearest rank) and `latency_summary(values) -> dict` (`n`, `mean`, `p50`, `p95`, `p99`, `max`, all in the input unit)
  - `class Tally` with `add_claim(claim_id: str, gold: dict[str, str], pred: dict[str, str]) -> None` and `summary(severity: dict | None = None) -> dict`

- [ ] **Step 1: Write the failing tests** `tests/test_eval_metrics.py`
```python
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
import eval_metrics as m


def claim(tally, cid, gold, pred):
    tally.add_claim(cid, gold, pred)


class PrfTests(unittest.TestCase):
    def test_hand_checked_values(self):
        r = m.prf({'tp': 1, 'fp': 1, 'fn': 1, 'tn': 1})
        self.assertEqual((r['precision'], r['recall'], r['f1']), (0.5, 0.5, 0.5))

    def test_no_positive_anywhere_is_undefined_not_zero(self):
        r = m.prf({'tp': 0, 'fp': 0, 'fn': 0, 'tn': 9})
        self.assertIsNone(r['f1'])
        self.assertIsNone(r['precision'])
        self.assertIsNone(r['recall'])

    def test_only_false_alarms_is_a_real_zero(self):
        r = m.prf({'tp': 0, 'fp': 3, 'fn': 0, 'tn': 9})
        self.assertEqual(r['f1'], 0.0)
        self.assertEqual(r['precision'], 0.0)
        self.assertIsNone(r['recall'])


class MacroTests(unittest.TestCase):
    def test_undefined_values_are_excluded_and_listed(self):
        r = m.macro({'a': 1.0, 'b': 0.0, 'c': None})
        self.assertEqual(r['macro_f1'], 0.5)
        self.assertEqual(r['defined'], 2)
        self.assertEqual(r['excluded'], ['c'])

    def test_all_undefined(self):
        r = m.macro({'a': None})
        self.assertIsNone(r['macro_f1'])
        self.assertEqual(r['defined'], 0)


class StructureTests(unittest.TestCase):
    def test_categories_partition_the_fifteen_rules(self):
        flat = [r for rules in m.CATEGORIES.values() for r in rules]
        self.assertEqual(sorted(flat), list(m.RULES))
        self.assertEqual(len(flat), len(set(flat)))

    def test_severity_groups(self):
        g = m.severity_groups([{'rule_id': 'R001', 'severity': 'high'}, {'rule_id': 'R002', 'severity': 'medium'}])
        self.assertEqual(g, {'high': ('R001',), 'medium': ('R002',)})


class BoundTests(unittest.TestCase):
    def test_zero_errors_closed_form(self):
        self.assertAlmostEqual(m.clopper_pearson_upper(0, 100), 1 - 0.05 ** (1 / 100), places=9)

    def test_one_error_in_100(self):
        self.assertAlmostEqual(m.clopper_pearson_upper(1, 100), 0.0466, places=3)

    def test_monotone_and_above_the_point_estimate(self):
        a, b = m.clopper_pearson_upper(2, 200), m.clopper_pearson_upper(3, 200)
        self.assertLess(a, b)
        self.assertGreater(a, 2 / 200)

    def test_all_errors_and_empty(self):
        self.assertEqual(m.clopper_pearson_upper(5, 5), 1.0)
        self.assertIsNone(m.clopper_pearson_upper(0, 0))

    def test_large_k_uses_wilson_and_stays_a_probability(self):
        u = m.clopper_pearson_upper(5000, 100000)
        self.assertTrue(0.05 < u < 0.06)


class LatencyTests(unittest.TestCase):
    def test_nearest_rank_percentiles(self):
        v = list(range(1, 101))
        self.assertEqual((m.percentile(v, 50), m.percentile(v, 95), m.percentile(v, 99)), (50, 95, 99))
        self.assertIsNone(m.percentile([], 50))

    def test_summary(self):
        s = m.latency_summary([1.0, 2.0, 3.0])
        self.assertEqual((s['n'], s['mean'], s['max']), (3, 2.0, 3.0))


G = {'R001': 'FAIL', 'R002': 'PASS'}


class TallyTests(unittest.TestCase):
    def build(self):
        t = m.Tally()
        # c1: gold FAIL/PASS, engine FAIL/FAIL  -> R001 tp, R002 fp
        claim(t, 'c1', {'R001': 'FAIL', 'R002': 'PASS'}, {'R001': 'FAIL', 'R002': 'FAIL'})
        # c2: gold FAIL/PASS, engine PASS/PASS  -> R001 fn, R002 tn
        claim(t, 'c2', {'R001': 'FAIL', 'R002': 'PASS'}, {'R001': 'PASS', 'R002': 'PASS'})
        return t

    def test_per_rule_counts_and_f1(self):
        s = self.build().summary()
        r1, r2 = s['per_rule']['R001'], s['per_rule']['R002']
        self.assertEqual((r1['tp'], r1['fn'], r1['f1']), (1, 1, 2 / 3))
        self.assertEqual((r2['fp'], r2['tn'], r2['f1']), (1, 1, 0.0))

    def test_category_pools_the_counts_of_its_rules(self):
        t = self.build()
        s = t.summary()
        # categories need all 15 rules to exist; only R001 and R002 were scored, so the groups hold what was scored
        self.assertEqual(s['by_category']['completeness_and_arithmetic']['tp'], 1)
        self.assertEqual(s['by_category']['timing']['fp'], 1)

    def test_disagreements_are_counted_and_sampled(self):
        s = self.build().summary()
        self.assertEqual(s['disagreements']['count'], 2)
        self.assertEqual(s['disagreements']['examples'][0]['claim_id'], 'c1')

    def test_status_accuracy_and_multiclass(self):
        s = self.build().summary()
        self.assertEqual(s['status_accuracy'], 0.5)
        self.assertIn('macro_f1', s['multiclass'])

    def test_missing_rule_in_prediction_is_an_error(self):
        t = m.Tally()
        with self.assertRaises(KeyError):
            t.add_claim('c', G, {'R001': 'FAIL'})

    def test_valid_claim_rates_need_all_fifteen_rules(self):
        t = m.Tally()
        ok = {r: 'PASS' for r in m.RULES}
        bad = dict(ok, R005='FAIL')
        unable = dict(ok, R009='UNABLE_TO_ASSESS')
        # clean claim, engine raises a FAIL -> false positive
        t.add_claim('c1', ok, bad)
        # clean claim, engine abstains -> false abstention
        t.add_claim('c2', ok, unable)
        # claim with an UNABLE in gold but no FAIL: valid-without-FAIL, not clean; engine quiet
        t.add_claim('c3', unable, unable)
        # claim with a FAIL in gold: not a valid claim
        t.add_claim('c4', bad, bad)
        v = t.summary()['valid_claims']
        self.assertEqual((v['claims_without_fail']['n'], v['claims_without_fail']['k']), (3, 1))
        self.assertEqual((v['clean_claims']['n'], v['clean_claims']['k']), (2, 1))
        self.assertEqual(v['clean_claim_false_abstention']['k'], 1)
        self.assertEqual(v['clean_result_false_alarm']['k'], 1)
        self.assertEqual(v['clean_result_false_alarm']['n'], 30)

    def test_partial_claims_are_excluded_from_valid_claim_rates(self):
        t = m.Tally()
        t.add_claim('c1', {'R001': 'PASS'}, {'R001': 'PASS'})
        v = t.summary()['valid_claims']
        self.assertEqual(v['claims_without_fail']['n'], 0)
        self.assertIsNone(v['claims_without_fail']['rate'])


if __name__ == '__main__':
    unittest.main()
```
Run: `.venv/Scripts/python.exe -m unittest discover -s tests -p test_eval_metrics.py`. Expected: ERROR (`ModuleNotFoundError: eval_metrics`).

- [ ] **Step 2: Write `src/eval_metrics.py`**
```python
"""Streaming detection metrics for the Phase 2 evaluation (docs/29). Pure functions, no I/O, no engine imports.

The positive class is FAIL, as in src/evaluate.py. A Tally consumes one claim at a time so even 100,000+ generated
claims never have to be held in memory.
"""
import math
from collections import Counter

POSITIVE = 'FAIL'
CLEAN_STATUSES = ('PASS', 'NOT_APPLICABLE')
RULES = tuple(f'R{i:03d}' for i in range(1, 16))
CATEGORIES = {
    'completeness_and_arithmetic': ('R001', 'R007', 'R012'),
    'eligibility_and_coverage': ('R003', 'R004', 'R005', 'R015'),
    'timing': ('R002', 'R014'),
    'authorization_and_documentation': ('R008', 'R009', 'R010'),
    'catalogue_pricing_and_duplicates': ('R006', 'R011', 'R013'),
}


def severity_groups(rules):
    groups = {}
    for r in rules:
        groups.setdefault(r['severity'], []).append(r['rule_id'])
    return {k: tuple(v) for k, v in groups.items()}


def prf(counts):
    tp, fp, fn = counts['tp'], counts['fp'], counts['fn']
    return {
        'precision': tp / (tp + fp) if tp + fp else None,
        'recall': tp / (tp + fn) if tp + fn else None,
        'f1': 2 * tp / (2 * tp + fp + fn) if tp + fp + fn else None,
    }


def macro(values):
    defined = {k: v for k, v in values.items() if v is not None}
    return {
        'macro_f1': sum(defined.values()) / len(defined) if defined else None,
        'defined': len(defined),
        'excluded': sorted(k for k, v in values.items() if v is None),
    }


def _binom_cdf(k, n, p):
    if p <= 0:
        return 1.0
    if p >= 1:
        return 1.0 if k >= n else 0.0
    lp, lq = math.log(p), math.log1p(-p)
    total = 0.0
    for i in range(k + 1):
        total += math.exp(math.lgamma(n + 1) - math.lgamma(i + 1) - math.lgamma(n - i + 1) + i * lp + (n - i) * lq)
    return min(1.0, total)


def _wilson_upper(k, n, z=1.6448536269514722):
    p = k / n
    centre = p + z * z / (2 * n)
    spread = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return min(1.0, (centre + spread) / (1 + z * z / n))


def clopper_pearson_upper(k, n, confidence=0.95):
    """One-sided exact upper bound on an error rate after k errors in n trials. For k > 2000 the exact sum is slow
    and the Wilson bound is used instead (it is within a few per cent of the exact bound at that size)."""
    if n <= 0:
        return None
    if k >= n:
        return 1.0
    alpha = 1 - confidence
    if k == 0:
        return 1 - alpha ** (1 / n)
    if k > 2000:
        return _wilson_upper(k, n)
    lo, hi = k / n, 1.0
    for _ in range(100):
        mid = (lo + hi) / 2
        if _binom_cdf(k, n, mid) > alpha:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


def percentile(values, q):
    if not values:
        return None
    s = sorted(values)
    return s[max(0, math.ceil(q / 100 * len(s)) - 1)]


def latency_summary(values):
    return {'n': len(values), 'mean': sum(values) / len(values) if values else None,
            'p50': percentile(values, 50), 'p95': percentile(values, 95), 'p99': percentile(values, 99),
            'max': max(values) if values else None}


def _rate(k, n):
    return {'k': k, 'n': n, 'rate': k / n if n else None, 'upper95': clopper_pearson_upper(k, n)}


class Tally:
    def __init__(self):
        self.rule_counts = {}             # rule -> [tp, fp, fn, tn]
        self.pairs = Counter()            # (gold_status, predicted_status) over every scored result
        self.n_claims = 0
        self.nofail_n = self.nofail_fp = 0
        self.clean_n = self.clean_fp = self.clean_abstain = 0
        self.clean_results = self.clean_false_alarms = 0
        self.disagree = 0
        self.examples = []

    def add_claim(self, claim_id, gold, pred):
        self.n_claims += 1
        for rid, g in gold.items():
            p = pred[rid]                 # an engine that omits a rule is a bug worth surfacing, not skipping
            c = self.rule_counts.setdefault(rid, [0, 0, 0, 0])
            gp, pp = g == POSITIVE, p == POSITIVE
            c[0 if gp and pp else 1 if pp else 2 if gp else 3] += 1
            self.pairs[(g, p)] += 1
            if g != p:
                self.disagree += 1
                if len(self.examples) < 10:
                    self.examples.append({'claim_id': claim_id, 'rule_id': rid, 'gold': g, 'predicted': p})
        if len(gold) != len(RULES):       # whole-claim statistics need every rule
            return
        if POSITIVE not in gold.values():
            self.nofail_n += 1
            self.nofail_fp += any(pred[r] == POSITIVE for r in gold)
            if all(s in CLEAN_STATUSES for s in gold.values()):
                self.clean_n += 1
                self.clean_fp += any(pred[r] == POSITIVE for r in gold)
                self.clean_abstain += any(pred[r] == 'UNABLE_TO_ASSESS' for r in gold)
                self.clean_results += len(gold)
                self.clean_false_alarms += sum(pred[r] == POSITIVE for r in gold)

    def _counts(self, rules):
        tp = fp = fn = tn = 0
        for r in rules:
            c = self.rule_counts.get(r)
            if c:
                tp, fp, fn, tn = tp + c[0], fp + c[1], fn + c[2], tn + c[3]
        return {'tp': tp, 'fp': fp, 'fn': fn, 'tn': tn}

    def _group(self, groups):
        out = {}
        for name, rules in groups.items():
            counts = self._counts(rules)
            out[name] = {**counts, **prf(counts), 'rules': list(rules)}
        return out

    def summary(self, severity=None):
        per_rule = {}
        for rid in sorted(self.rule_counts):
            counts = self._counts([rid])
            per_rule[rid] = {**counts, **prf(counts)}
        total = sum(self.pairs.values())
        correct = sum(n for (g, p), n in self.pairs.items() if g == p)
        by_category = self._group(CATEGORIES)
        out = {
            'claims': self.n_claims, 'results': total,
            'overall': {**self._counts(self.rule_counts), **prf(self._counts(self.rule_counts))},
            'per_rule': per_rule,
            'by_category': by_category,
            'macro_category': macro({k: v['f1'] for k, v in by_category.items()}),
            'macro_rule': macro({k: v['f1'] for k, v in per_rule.items()}),
            'status_accuracy': correct / total if total else None,
            'multiclass': self._multiclass(),
            'valid_claims': {
                'claims_without_fail': _rate(self.nofail_fp, self.nofail_n),
                'clean_claims': _rate(self.clean_fp, self.clean_n),
                'clean_claim_false_abstention': _rate(self.clean_abstain, self.clean_n),
                'clean_result_false_alarm': _rate(self.clean_false_alarms, self.clean_results),
            },
            'disagreements': {'count': self.disagree, 'examples': list(self.examples)},
            'fail_error_rate': _rate(sum(c[1] + c[2] for c in self.rule_counts.values()), total),
        }
        if severity:
            by_sev = self._group(severity)
            out['by_severity'] = by_sev
            out['macro_severity'] = macro({k: v['f1'] for k, v in by_sev.items()})
        return out

    def _multiclass(self):
        statuses = sorted({s for pair in self.pairs for s in pair})
        f1s = {}
        for s in statuses:
            tp = self.pairs[(s, s)]
            fp = sum(n for (g, p), n in self.pairs.items() if p == s and g != s)
            fn = sum(n for (g, p), n in self.pairs.items() if g == s and p != s)
            f1s[s] = 2 * tp / (2 * tp + fp + fn) if tp + fp + fn else None
        return {**macro(f1s), 'per_status_f1': f1s}
```

- [ ] **Step 3: Run the tests**, expected PASS. If `test_valid_claim_rates_need_all_fifteen_rules` counts differ, recheck the hand count in the test comments (the code is the oracle only after you re-derive the numbers by hand), do not adjust numbers to match output.

- [ ] **Step 4: Security-sink and style check**
Run: `.venv/Scripts/python.exe -m unittest discover -s tests -p test_security_owasp.py`. Expected: PASS (no sink words in `src/eval_metrics.py`).

- [ ] **Step 5: Commit**
```bash
git add src/eval_metrics.py tests/test_eval_metrics.py
git commit -m "feat: streaming detection metrics for the Phase 2 evaluation" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Labelled sets from the organizers and by hand (S1 to S6, S9) with provenance

**Files:**
- Create: `scripts/eval_sets.py`
- Test: `tests/test_eval_sets.py`

**Interfaces:**
- Consumes: Task 1 constants (`RULES`); `engine_core.load_jsonl`, `validate_transport`, `config`; `ingest.ingest(path, fmt)`, `ingest.FHIR`, `ingest.CSV_FOLDER`; `audit.digest`; `tests/test_stress_boundaries.CASES` and `.CLEAN`.
- Produces in `scripts/eval_sets.py`:
  - `@dataclass EvalSet`: `set_id, tier, name, path, label_kind, label_source, generator, limitation, files, items, notes` where `items()` returns an iterator of `(claim: dict, gold: dict[rule_id -> status])` and `notes` is a dict filled as items are consumed
  - `SPLITS = ('development', 'validation', 'stress')`
  - `file_sha256(path) -> str`, `read_git_head(root) -> str` (never raises; returns `'unknown'`)
  - `organizer_sets(root) -> list[EvalSet]` (S1, S2, S3, S4 in that order; S4 skips claims already in S1 to S3 and records the overlap in `notes['duplicates_of_s1_s3']`)
  - `format_variant_sets(root) -> list[EvalSet]` (S5 FHIR then S6 CSV; each yields the *ingested* claim and the organizer gold for it; `notes` gets `ingested`, `quarantined`)
  - `boundary_set(root) -> EvalSet` (S9; claim ids `CG-BND-000`..; gold only for the rules each case names)
  - `provenance_entry(es, root, commit, claims, results) -> dict`

- [ ] **Step 1: Write the failing tests** `tests/test_eval_sets.py`
```python
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
```
Run: `.venv/Scripts/python.exe -m unittest discover -s tests -p test_eval_sets.py`. Expected: ERROR (`ModuleNotFoundError: eval_sets`).

- [ ] **Step 2: Write `scripts/eval_sets.py`** (organizer, format-variant and boundary loaders; the generated loaders are added in Task 3)
```python
"""Labelled example sets for the Phase 2 evaluation, each tagged with its evidence tier and label origin.

Tier A: organizer answer key.  Tier B: our independent oracle (agreement, not accuracy).  Tier C: hand-derived.
Every set carries what a reader needs to cite it; provenance_entry() turns that into outputs/evaluation/provenance.json.
"""
import copy
import hashlib
import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from audit import digest  # noqa: E402
from engine_core import load_jsonl, validate_transport  # noqa: E402
from eval_metrics import RULES  # noqa: E402,F401
from ingest import CSV_FOLDER, FHIR, ingest  # noqa: E402

SPLITS = ('development', 'validation', 'stress')
DATASET = 'organizers synthetic dataset v1.0.0 (data/dataset_manifest.json, generated 2026-09-17)'


@dataclass
class EvalSet:
    set_id: str
    tier: str
    name: str
    path: str                  # how the claim reaches the engine: 'engine', 'ingest_fhir' or 'ingest_csv'
    label_kind: str            # 'organizer_key', 'independent_oracle' or 'hand_derived'
    label_source: str
    generator: str
    limitation: str
    files: list
    items: object              # callable returning an iterator of (claim, {rule_id: status})
    notes: dict = field(default_factory=dict)


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def read_git_head(root):
    """The commit a checkout is at, read from .git without running git. 'unknown' if anything is missing."""
    try:
        git = Path(root) / '.git'
        if git.is_file():
            git = Path(root) / git.read_text(encoding='utf-8').split('gitdir:', 1)[1].strip()
        head = (git / 'HEAD').read_text(encoding='utf-8').strip()
        if not head.startswith('ref:'):
            return head
        ref = head.split(None, 1)[1]
        common = git
        if (git / 'commondir').exists():
            common = (git / (git / 'commondir').read_text(encoding='utf-8').strip()).resolve()
        for base in (git, common):
            if (base / ref).exists():
                return (base / ref).read_text(encoding='utf-8').strip()
            packed = base / 'packed-refs'
            if packed.exists():
                for line in packed.read_text(encoding='utf-8').splitlines():
                    if line.endswith(' ' + ref):
                        return line.split()[0]
    except (OSError, IndexError, ValueError):
        pass
    return 'unknown'


def organizer_gold(root, split):
    gold = defaultdict(dict)
    for r in load_jsonl(Path(root) / 'data' / split / 'expected_results.jsonl'):
        gold[r['claim_id']][r['rule_id']] = r['status']
    return gold


def _rel(root, p):
    return str(Path(p).resolve().relative_to(Path(root).resolve())).replace('\\', '/')


def organizer_sets(root):
    root = Path(root)
    sets, seen = [], set()
    for i, split in enumerate(SPLITS, start=1):
        claims_path = root / 'data' / split / 'claims.jsonl'
        key_path = root / 'data' / split / 'expected_results.jsonl'
        claims, gold = load_jsonl(claims_path), organizer_gold(root, split)
        seen.update(digest(c) for c in claims)
        sets.append(EvalSet(
            f'S{i}', 'A', f'data/{split}', 'engine', 'organizer_key',
            f'data/{split}/expected_results.jsonl, {DATASET}',
            'supplied by the organizers; not generated by us',
            'the answer key is deterministic and the rules were built against it, so a perfect score here cannot rank this system against a better one',
            [_rel(root, claims_path), _rel(root, key_path)],
            (lambda claims=claims, gold=gold: ((c, gold[c['claim_id']]) for c in claims))))
    hb_path = root / 'examples' / 'worked_cases.json'
    cases = json.loads(hb_path.read_text(encoding='utf-8'))
    s4 = EvalSet('S4', 'A', 'handbook worked cases', 'engine', 'organizer_key',
                 'examples/worked_cases.json, ClaimGuardAI_Student_Handbook.pdf',
                 'supplied by the organizers; ten cases', 'only ten claims', [_rel(root, hb_path)], None)

    def s4_items():
        s4.notes['duplicates_of_s1_s3'] = 0
        for case in cases:
            if digest(case['claim']) in seen:
                s4.notes['duplicates_of_s1_s3'] += 1
                continue
            yield case['claim'], {r['rule_id']: r['status'] for r in case['expected_results']}
    s4.items = s4_items
    sets.append(s4)
    return sets


def format_variant_sets(root):
    root = Path(root)
    gold_by_split = {sp: organizer_gold(root, sp) for sp in SPLITS}
    out = []
    for set_id, fmt, path_name, label, limit in (
            ('S5', FHIR, 'ingest_fhir', 'FHIR bundles',
             'FHIR carries no authorizations, so R009 becomes UNABLE_TO_ASSESS by design (docs/27, ingestformats)'),
            ('S6', CSV_FOLDER, 'ingest_csv', 'CSV folders', 'a lossless re-encoding; any difference is an ingestion bug')):
        files = []
        for sp in SPLITS:
            if fmt == FHIR:
                files.append(_rel(root, root / 'data' / sp / 'fhir_bundles.jsonl'))
            else:
                files.extend(_rel(root, p) for p in sorted((root / 'data' / sp / 'csv').glob('*.csv')))
        s = EvalSet(set_id, 'A', f'the 600 public claims as {label}', path_name, 'organizer_key',
                    'organizer answer key for the same claim_id (data/*/expected_results.jsonl)',
                    'src/ingest.py reads the file; the claim that results is scored by the engine', limit, files, None)

        def items(s=s, fmt=fmt):
            s.notes.update(ingested=0, quarantined=0)
            for sp in SPLITS:
                src = root / 'data' / sp / ('fhir_bundles.jsonl' if fmt == FHIR else 'csv')
                for it in ingest(src, fmt):
                    gold = gold_by_split[sp].get(it.claim['claim_id']) if it.accepted else None
                    if it.accepted and gold:
                        s.notes['ingested'] += 1
                        yield it.claim, gold
                    else:
                        s.notes['quarantined'] += 1
        s.items = items
        out.append(s)
    return out


def boundary_set(root):
    root = Path(root)
    import test_stress_boundaries as tb

    s = EvalSet('S9', 'C', 'boundary table (123 cases)', 'engine', 'hand_derived',
                'tests/test_stress_boundaries.py: expected statuses derived by hand from docs/04_Rulebook.md',
                'tests/test_stress_boundaries.py, CASES applied to CLEAN', 'written by us, only for the rules each case names (140 labelled results)',
                [_rel(root, root / 'tests' / 'test_stress_boundaries.py')], None)

    def items():
        for n, (label, mutate, expected) in enumerate(tb.CASES):
            c = copy.deepcopy(tb.CLEAN)
            mutate(c)
            c['claim_id'] = f'CG-BND-{n:03d}'
            validate_transport(c)
            yield c, dict(expected)
    s.items = items
    return s


def provenance_entry(es, root, commit, claims, results):
    root = Path(root)
    return {
        'set_id': es.set_id, 'tier': es.tier, 'name': es.name, 'path': es.path, 'label_kind': es.label_kind,
        'label_source': es.label_source, 'generator': es.generator, 'limitation': es.limitation,
        'files': [{'path': f, 'sha256': file_sha256(root / f)} for f in es.files],
        'claims': claims, 'results': results, 'notes': dict(es.notes), 'commit': commit,
    }
```
In the CSV branch of `items`, `src` is a folder; `ingest` accepts a folder for `CSV_FOLDER`.

- [ ] **Step 3: Run** `.venv/Scripts/python.exe -m unittest discover -s tests -p test_eval_sets.py`. Expected PASS. Likely snags, to be diagnosed by reading output and not by loosening tests: (a) S4 may be entirely duplicates of S1 to S3 (then `len(items)` is 0 and `duplicates == 10`, which is a valid, reportable outcome); (b) if FHIR `claim_id`s differ from the organizer key, `quarantined` will be large: investigate `src/fhir_adapter.py` id mapping and record the finding in the report rather than patching ids in the evaluator.

- [ ] **Step 4: Sink check** `.venv/Scripts/python.exe -m unittest discover -s tests -p test_security_owasp.py` (scans `scripts/`). Expected PASS.

- [ ] **Step 5: Commit** `git add scripts/eval_sets.py tests/test_eval_sets.py && git commit -m "feat: labelled evaluation sets from the organizers and by hand, with provenance" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"`

---

### Task 3: Generated sets labelled by the independent oracle (S7, S8)

**Files:**
- Modify: `scripts/eval_sets.py` (append two loaders)
- Test: `tests/test_eval_sets_generated.py`

**Interfaces:**
- Consumes: Task 2 `EvalSet`, `file_sha256`; `oracle.load_rules_pack(root)`, `oracle.evaluate(claim, pack) -> {rule_id: status}`; `claim_gen.random_claim(rng)`; `test_stress_differential.mutate(claim, rng)`; `engine_core.validate_transport`, `load_jsonl`.
- Produces:
  - `generated_set(root, n_claims=107635, seed=20260927) -> EvalSet` (S7). Reproduces `scripts/status_coverage.py`'s generation loop exactly (same rng stream, same transport filter), then renames each kept claim to `CG-GEN-{n:06d}`; `notes` gets `attempted` and `scored`.
  - `mutant_set(root, attempts=37000, seed=20261004) -> EvalSet` (S8). Pool is all claims from the three public splits; each attempt is `mutate(rng.choice(pool), rng)`; transport-invalid mutants are skipped; kept ones are renamed `CG-MUT-{n:06d}`; `notes` gets `attempted`, `scored`.

- [ ] **Step 1: Write the failing tests** `tests/test_eval_sets_generated.py`
```python
import random
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
import eval_sets as es
import oracle
from claim_gen import random_claim
from engine_core import config, validate_transport
from yara_engine import evaluate


class GeneratedSetTests(unittest.TestCase):
    def test_matches_the_status_coverage_stream(self):
        # the first kept claim must be the first claim status_coverage.py would keep for the same seed
        rng = random.Random(20260927)
        while True:
            c = random_claim(rng)
            try:
                validate_transport(c)
                break
            except Exception:
                continue
        s = es.generated_set(ROOT, n_claims=5, seed=20260927)
        first, _ = next(iter(s.items()))
        c['claim_id'] = 'CG-GEN-000000'
        self.assertEqual(first, c)

    def test_ids_unique_gold_from_oracle_and_notes(self):
        s = es.generated_set(ROOT, n_claims=300, seed=20260927)
        items = list(s.items())
        ids = [c['claim_id'] for c, _ in items]
        self.assertEqual(len(ids), 300)
        self.assertEqual(len(set(ids)), 300)
        pack = oracle.load_rules_pack(ROOT)
        claim, gold = items[0]
        self.assertEqual(gold, oracle.evaluate(claim, pack))
        self.assertEqual(s.notes['scored'], 300)
        self.assertGreaterEqual(s.notes['attempted'], 300)
        self.assertEqual((s.tier, s.label_kind), ('B', 'independent_oracle'))

    def test_renaming_does_not_change_any_verdict(self):
        cfg = config(ROOT)
        rng = random.Random(1)
        for _ in range(100):
            c = random_claim(rng)
            a = {r['rule_id']: r['status'] for r in evaluate(c, cfg)}
            b = {r['rule_id']: r['status'] for r in evaluate(dict(c, claim_id='CG-RENAMED'), cfg)}
            self.assertEqual(a, b)

    def test_deterministic(self):
        a = [c for c, _ in es.generated_set(ROOT, n_claims=20, seed=7).items()]
        b = [c for c, _ in es.generated_set(ROOT, n_claims=20, seed=7).items()]
        self.assertEqual(a, b)


class MutantSetTests(unittest.TestCase):
    def test_unique_ids_valid_transport_and_oracle_gold(self):
        s = es.mutant_set(ROOT, attempts=400, seed=20261004)
        items = list(s.items())
        self.assertGreater(len(items), 100)
        ids = [c['claim_id'] for c, _ in items]
        self.assertEqual(len(ids), len(set(ids)))
        pack = oracle.load_rules_pack(ROOT)
        for claim, gold in items[:50]:
            validate_transport(claim)
            self.assertEqual(gold, oracle.evaluate(claim, pack))
        self.assertEqual(s.notes['attempted'], 400)
        self.assertEqual(s.notes['scored'], len(items))

    def test_deterministic(self):
        a = [c for c, _ in es.mutant_set(ROOT, attempts=60, seed=3).items()]
        b = [c for c, _ in es.mutant_set(ROOT, attempts=60, seed=3).items()]
        self.assertEqual(a, b)


if __name__ == '__main__':
    unittest.main()
```
Run: expected ERROR/FAIL (`generated_set` missing).

- [ ] **Step 2: Append to `scripts/eval_sets.py`**
```python
def generated_set(root, n_claims=107635, seed=20260927):
    root = Path(root)
    import oracle
    from claim_gen import random_claim
    import random

    s = EvalSet('S7', 'B', 'generated claims', 'engine', 'independent_oracle',
                'tests/oracle.py: a second implementation of the 15 rules written from docs/04 alone',
                f'tests/claim_gen.py random_claim, seed {seed}, same loop as scripts/status_coverage.py',
                'the oracle shares our reading of the rulebook, so this measures agreement between two implementations, not accuracy',
                [_rel(root, root / 'tests' / 'claim_gen.py'), _rel(root, root / 'tests' / 'oracle.py')], None)

    def items():
        pack = oracle.load_rules_pack(root)
        rng = random.Random(seed)
        attempted = kept = 0
        s.notes.update(attempted=0, scored=0)
        while kept < n_claims:
            c = random_claim(rng)
            attempted += 1
            try:
                validate_transport(c)
            except Exception:
                continue                      # ingestion would quarantine it; the engine never scores it
            c['claim_id'] = f'CG-GEN-{kept:06d}'   # random ids collide at this size, and a collision would overwrite a label
            kept += 1
            s.notes.update(attempted=attempted, scored=kept)
            yield c, oracle.evaluate(c, pack)
    s.items = items
    return s


def mutant_set(root, attempts=37000, seed=20261004):
    root = Path(root)
    import oracle
    import random
    from test_stress_differential import mutate

    s = EvalSet('S8', 'B', 'boundary-aware mutants of the public claims', 'engine', 'independent_oracle',
                'tests/oracle.py (see S7)',
                f'mutate() from tests/test_stress_differential.py applied to the 600 public claims, seed {seed}, {attempts} attempts',
                'mutants of real claims; same shared-reading caveat as S7',
                [_rel(root, root / 'tests' / 'test_stress_differential.py'), _rel(root, root / 'tests' / 'oracle.py')], None)

    def items():
        pack = oracle.load_rules_pack(root)
        pool = [c for sp in SPLITS for c in load_jsonl(root / 'data' / sp / 'claims.jsonl')]
        rng = random.Random(seed)
        kept = 0
        s.notes.update(attempted=attempts, scored=0)
        for _ in range(attempts):
            m = mutate(rng.choice(pool), rng)
            try:
                validate_transport(m)
            except Exception:
                continue
            m['claim_id'] = f'CG-MUT-{kept:06d}'
            kept += 1
            s.notes['scored'] = kept
            yield m, oracle.evaluate(m, pack)
    s.items = items
    return s
```

- [ ] **Step 3: Run** `.venv/Scripts/python.exe -m unittest discover -s tests -p test_eval_sets_generated.py`. Expected PASS. If `test_matches_the_status_coverage_stream` fails, the loop differs from `status_coverage.run`: re-read it (`scripts/status_coverage.py` lines 46 to 76) and match it; do not change the test.

- [ ] **Step 4: Sink check** `-p test_security_owasp.py`; **Step 5: Commit** `git add scripts/eval_sets.py tests/test_eval_sets_generated.py && git commit -m "feat: oracle-labelled generated and mutant evaluation sets" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"`

---

### Task 4: The evaluation runner

**Files:**
- Create: `scripts/evaluate_phase2.py`
- Test: `tests/test_evaluate_phase2.py`

**Interfaces:**
- Consumes: Tasks 1 to 3 (`Tally`, `severity_groups`, `latency_summary`, all loaders, `provenance_entry`, `read_git_head`); `yara_engine.evaluate(claim, cfg, tool_errors)`; `engine_core.config`, `baseline`; `audit_log.AuditLog`, `audited_review`; `llm_adapter.MockExplanationProvider`; `ingest.ingest`.
- Produces:
  - `score_set(es, cfg, severity, baselines=False) -> dict` returning `{'summary': Tally.summary(...), 'engine_crashes': int, 'engine_seconds': list[float], 'baselines': {...} | None}`; `baselines` has `always_pass` and `starter_baseline`, each a `Tally.summary()`.
  - `measure_latency(root, cfg, repeats=3, audited_claims=100) -> dict` with keys `engine_per_claim`, `ingest_fhir_per_claim`, `ingest_csv_per_claim`, `audited_template_per_claim` (each `latency_summary` in milliseconds), plus `environment`.
  - `run_all(root, out_dir, generated=107635, mutant_attempts=37000, repeats=3, audited_claims=100) -> dict` writing `metrics.json` and `provenance.json` and returning the metrics dict. `metrics['sets'][set_id]` holds `{'meta': {tier, name, path, label_kind}, 'summary', 'engine_crashes', 'baselines'}`; `metrics['latency']`; `metrics['ai_step_recorded']` (copied from `outputs/defense/load.json`, key `ai_step_seconds`, with its source path); `metrics['commit']`.
  - CLI: `python scripts/evaluate_phase2.py [--out-dir outputs/evaluation] [--generated N] [--mutants N]`.

- [ ] **Step 1: Write the failing tests** `tests/test_evaluate_phase2.py`
```python
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
sys.path.insert(0, str(ROOT / 'src'))
import eval_sets as es
import evaluate_phase2 as ev
from engine_core import config
from eval_metrics import severity_groups


class ScoreSetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cfg = config(ROOT)
        cls.sev = severity_groups(cls.cfg['rules'])

    def test_the_stress_split_scores_perfectly_and_baselines_do_not(self):
        s3 = es.organizer_sets(ROOT)[2]
        r = ev.score_set(s3, self.cfg, self.sev, baselines=True)
        self.assertEqual(r['summary']['claims'], 50)
        self.assertEqual(r['summary']['results'], 750)
        self.assertEqual(r['summary']['disagreements']['count'], 0)
        self.assertEqual(r['engine_crashes'], 0)
        self.assertEqual(r['summary']['overall']['f1'], 1.0)
        self.assertEqual(r['baselines']['always_pass']['overall']['f1'], 0.0)
        self.assertLess(r['baselines']['starter_baseline']['overall']['f1'], 0.9)
        self.assertIn('macro_severity', r['summary'])

    def test_a_wrong_label_shows_up_as_a_disagreement(self):
        s3 = es.organizer_sets(ROOT)[2]
        real = s3.items

        def flipped():
            for n, (c, g) in enumerate(real()):
                if n == 0:
                    g = dict(g, R001='FAIL' if g['R001'] != 'FAIL' else 'PASS')
                yield c, g
        s3.items = flipped
        r = ev.score_set(s3, self.cfg, self.sev)
        self.assertEqual(r['summary']['disagreements']['count'], 1)

    def test_an_isolated_rule_crash_is_counted_not_skipped(self):
        import logging
        import yara_engine
        original = yara_engine.DETAIL_FUNCS['R007']
        yara_engine.DETAIL_FUNCS['R007'] = lambda claim, cfg: 1 / 0
        logging.disable(logging.WARNING)
        try:
            r = ev.score_set(es.organizer_sets(ROOT)[2], self.cfg, self.sev)
        finally:
            yara_engine.DETAIL_FUNCS['R007'] = original
            logging.disable(logging.NOTSET)
        self.assertEqual(r['engine_crashes'], 50)
        self.assertGreater(r['summary']['disagreements']['count'], 0)

    def test_boundary_set_scores_only_the_named_rules(self):
        r = ev.score_set(es.boundary_set(ROOT), self.cfg, self.sev)
        self.assertEqual(r['summary']['results'], 140)
        self.assertEqual(r['summary']['disagreements']['count'], 0)


class RunAllTests(unittest.TestCase):
    def test_small_run_writes_both_files_with_every_set_cited(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = ev.run_all(ROOT, tmp, generated=60, mutant_attempts=60, repeats=1, audited_claims=3)
            metrics = json.loads((Path(tmp) / 'metrics.json').read_text(encoding='utf-8'))
            prov = json.loads((Path(tmp) / 'provenance.json').read_text(encoding='utf-8'))
            self.assertEqual(sorted(metrics['sets']), ['S1', 'S2', 'S3', 'S4', 'S5', 'S6', 'S7', 'S8', 'S9'])
            self.assertEqual(sorted(e['set_id'] for e in prov['sets']), sorted(metrics['sets']))
            for e in prov['sets']:
                self.assertEqual(e['claims'], metrics['sets'][e['set_id']]['summary']['claims'])
            self.assertEqual(metrics['commit'], prov['commit'])
            for key in ('engine_per_claim', 'ingest_fhir_per_claim', 'ingest_csv_per_claim', 'audited_template_per_claim'):
                self.assertGreater(metrics['latency'][key]['n'], 0)
            self.assertIn('median', metrics['ai_step_recorded']['seconds'])
            self.assertEqual(m['commit'], metrics['commit'])

    def test_accuracy_sections_are_deterministic(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            x = ev.run_all(ROOT, a, generated=40, mutant_attempts=40, repeats=1, audited_claims=2)
            y = ev.run_all(ROOT, b, generated=40, mutant_attempts=40, repeats=1, audited_claims=2)
            self.assertEqual({k: v['summary'] for k, v in x['sets'].items()},
                             {k: v['summary'] for k, v in y['sets'].items()})

    def test_does_not_touch_the_committed_evidence_directory(self):
        before = (ROOT / 'outputs' / 'evaluation').exists()
        with tempfile.TemporaryDirectory() as tmp:
            ev.run_all(ROOT, tmp, generated=10, mutant_attempts=10, repeats=1, audited_claims=1)
        self.assertEqual((ROOT / 'outputs' / 'evaluation').exists(), before)


if __name__ == '__main__':
    unittest.main()
```
Run: expected ERROR (`ModuleNotFoundError: evaluate_phase2`).

- [ ] **Step 2: Write `scripts/evaluate_phase2.py`**
```python
"""Phase 2 detection evaluation: score every labelled set, measure latency, write outputs/evaluation/*.json.

    python scripts/evaluate_phase2.py --out-dir outputs/evaluation

Accuracy results are deterministic (fixed seeds). Latency results depend on the machine and are labelled as such.
"""
import argparse
import json
import logging
import os
import platform
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'scripts'))
import eval_sets as sets  # noqa: E402
from audit_log import AuditLog, audited_review  # noqa: E402
from engine_core import baseline, config  # noqa: E402
from eval_metrics import Tally, latency_summary, severity_groups  # noqa: E402
from ingest import CSV_FOLDER, FHIR, ingest  # noqa: E402
from llm_adapter import MockExplanationProvider  # noqa: E402
from yara_engine import evaluate  # noqa: E402


def score_set(es, cfg, severity, baselines=False):
    tally, always_pass, starter = Tally(), Tally(), Tally()
    crashes, seconds = 0, []
    for claim, gold in es.items():
        errors = []
        t0 = time.perf_counter()
        results = evaluate(claim, cfg, errors)
        seconds.append(time.perf_counter() - t0)
        pred = {r['rule_id']: r['status'] for r in results}
        crashes += bool(errors)
        tally.add_claim(claim['claim_id'], gold, pred)
        if baselines:
            always_pass.add_claim(claim['claim_id'], gold, {r: 'PASS' for r in gold})
            starter.add_claim(claim['claim_id'], gold, {r['rule_id']: r['status'] for r in baseline(claim, cfg)})
    return {
        'summary': tally.summary(severity), 'engine_crashes': crashes, 'engine_seconds': seconds,
        'baselines': {'always_pass': always_pass.summary(), 'starter_baseline': starter.summary()} if baselines else None,
    }


def _ms(values):
    return latency_summary([v * 1000 for v in values])


def measure_latency(root, cfg, repeats=3, audited_claims=100):
    root = Path(root)
    claims = [c for es in sets.organizer_sets(root)[:3] for c, _ in es.items()]
    engine = []
    for _ in range(repeats):
        for c in claims:
            t0 = time.perf_counter()
            evaluate(c, cfg, [])
            engine.append(time.perf_counter() - t0)
    per_format = {}
    for key, fmt, name in (('ingest_fhir_per_claim', FHIR, 'fhir_bundles.jsonl'), ('ingest_csv_per_claim', CSV_FOLDER, 'csv')):
        times = []
        for _ in range(repeats):
            it = iter(ingest(root / 'data' / 'development' / name, fmt))
            while True:
                t0 = time.perf_counter()
                item = next(it, None)
                if item is None:
                    break
                if item.accepted:
                    evaluate(item.claim, cfg, [])
                times.append(time.perf_counter() - t0)
        per_format[key] = _ms(times)
    audited = []
    with tempfile.TemporaryDirectory() as tmp:
        log = AuditLog(Path(tmp) / 'audit.jsonl')
        provider = MockExplanationProvider()
        for c in claims[:audited_claims]:
            t0 = time.perf_counter()
            audited_review(log, c, cfg, provider=provider, fallback=provider)
            audited.append(time.perf_counter() - t0)
    return {
        'unit': 'milliseconds', 'deterministic': False,
        'environment': {'platform': platform.platform(), 'python': platform.python_version(), 'cpus': os.cpu_count()},
        'engine_per_claim': _ms(engine), **per_format, 'audited_template_per_claim': _ms(audited),
    }


def run_all(root, out_dir, generated=107635, mutant_attempts=37000, repeats=3, audited_claims=100):
    root, out_dir = Path(root), Path(out_dir)
    cfg = config(root)
    severity = severity_groups(cfg['rules'])
    commit = sets.read_git_head(root)
    all_sets = [*sets.organizer_sets(root), *sets.format_variant_sets(root), sets.generated_set(root, generated),
                sets.mutant_set(root, mutant_attempts), sets.boundary_set(root)]
    all_sets.sort(key=lambda s: int(s.set_id[1:]))
    logging.disable(logging.WARNING)           # the engine logs each isolated rule crash; it is counted instead
    metrics, provenance = {}, []
    try:
        for es in all_sets:
            r = score_set(es, cfg, severity, baselines=(es.tier == 'A' and es.path == 'engine'))
            metrics[es.set_id] = {'meta': {'tier': es.tier, 'name': es.name, 'path': es.path, 'label_kind': es.label_kind},
                                  'summary': r['summary'], 'engine_crashes': r['engine_crashes'], 'baselines': r['baselines'],
                                  'engine_ms': _ms(r['engine_seconds'])}
            provenance.append(sets.provenance_entry(es, root, commit, r['summary']['claims'], r['summary']['results']))
        latency = measure_latency(root, cfg, repeats, audited_claims)
    finally:
        logging.disable(logging.NOTSET)
    load_path = root / 'outputs' / 'defense' / 'load.json'
    ai = json.loads(load_path.read_text(encoding='utf-8'))['ai_step_seconds'] if load_path.exists() else None
    out = {'commit': commit, 'sets': metrics, 'latency': latency,
           'ai_step_recorded': {'seconds': ai, 'source': 'outputs/defense/load.json (live Mistral-Nemo calls, recorded earlier; not re-run here)'}}
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / 'metrics.json').write_text(json.dumps(out, indent=2), encoding='utf-8')
    (out_dir / 'provenance.json').write_text(json.dumps({'commit': commit, 'sets': provenance}, indent=2), encoding='utf-8')
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out-dir', default='outputs/evaluation')
    ap.add_argument('--generated', type=int, default=107635)
    ap.add_argument('--mutants', type=int, default=37000)
    a = ap.parse_args(argv)
    m = run_all(ROOT, a.out_dir, a.generated, a.mutants)
    for sid, s in m['sets'].items():
        o = s['summary']['overall']
        print(sid, s['meta']['name'], f"claims={s['summary']['claims']}", f"f1={o['f1']}", f"disagreements={s['summary']['disagreements']['count']}")
    return 0


if __name__ == '__main__':
    sys.exit(main())
```
Note on `run_all` accessing `s.items` of S5/S6 in `measure_latency`: it uses `ingest` directly, so S5/S6 notes are filled by `score_set` only.

- [ ] **Step 3: Run** `.venv/Scripts/python.exe -m unittest discover -s tests -p test_evaluate_phase2.py`. Expected PASS (about a minute). If `audited_review` raises about a signature, read `src/audit_log.py:498` and call it exactly as `tests/test_stress_ai_boundary.py` does.

- [ ] **Step 4: Sink check and bandit.** `-p test_security_owasp.py`; then `.venv/Scripts/python.exe -m pip install bandit==1.9.4` is NOT needed in the project venv: use `uv venv "$TEMP/sec" && uv pip install --python "$TEMP/sec/Scripts/python.exe" bandit==1.9.4 && "$TEMP/sec/Scripts/bandit.exe" -q -r src scripts -ll`. Expected: exit 0.

- [ ] **Step 5: Commit** `git add scripts/evaluate_phase2.py tests/test_evaluate_phase2.py && git commit -m "feat: Phase 2 evaluation runner with baselines, latency and provenance" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"`

---

### Task 5: Real run, report renderer, the report, and wiring into the docs

**Files:**
- Create: `scripts/render_eval_report.py`, `tests/test_eval_report.py`, `docs/29_Test_Evaluation_Report.md`, `outputs/evaluation/metrics.json`, `outputs/evaluation/provenance.json`
- Modify: `README.md`, `SPECS.md` (section 10, link and one row), `docs/27_Decisions_Proofs_and_Defense.md` (one row)

**Interfaces:**
- Consumes: Task 4 `metrics.json`, `provenance.json`.
- Produces: `render_eval_report.render_tables(metrics, provenance) -> dict[str, str]` with keys `provenance`, `tier_a`, `categories`, `per_rule`, `valid_claims`, `baselines`, `tier_b_c`, `latency`; `apply_tables(doc: str, tables: dict[str, str]) -> str` replacing every `<!-- TABLE:name -->...<!-- /TABLE:name -->` block (raises `KeyError` if a marker is missing); CLI `--check` exits 1 when the committed report is stale.

- [ ] **Step 1: Run the real evaluation** (about 5 to 8 minutes; run in the background and read the file when done)
```bash
.venv/Scripts/python.exe scripts/evaluate_phase2.py --out-dir outputs/evaluation > "$TEMP/eval_run.log" 2>&1; tail -12 "$TEMP/eval_run.log"
```
Read `outputs/evaluation/metrics.json`. Expected: S1 to S3 disagreements 0; S5 shows R009 differences (FHIR); S6 identical to the key; S7/S8/S9 agreement. **If any tier-A set other than S5 disagrees, or any tier B/C set does, stop and investigate with superpowers:systematic-debugging; report it, do not hide it.** Record the actual numbers; the report must quote them.

- [ ] **Step 2: Write the failing tests** `tests/test_eval_report.py`
```python
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
import render_eval_report as r

TINY_SUMMARY = {
    'claims': 2, 'results': 30, 'overall': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 29, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0},
    'per_rule': {'R001': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 1, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0}},
    'by_category': {'timing': {'tp': 0, 'fp': 0, 'fn': 0, 'tn': 4, 'precision': None, 'recall': None, 'f1': None, 'rules': ['R002']}},
    'macro_category': {'macro_f1': 1.0, 'defined': 1, 'excluded': ['timing']},
    'macro_rule': {'macro_f1': 1.0, 'defined': 1, 'excluded': []},
    'macro_severity': {'macro_f1': 1.0, 'defined': 1, 'excluded': []},
    'by_severity': {'high': {'tp': 1, 'fp': 0, 'fn': 0, 'tn': 0, 'precision': 1.0, 'recall': 1.0, 'f1': 1.0, 'rules': ['R001']}},
    'status_accuracy': 1.0, 'multiclass': {'macro_f1': 1.0, 'defined': 2, 'excluded': [], 'per_status_f1': {}},
    'valid_claims': {k: {'k': 0, 'n': 5, 'rate': 0.0, 'upper95': 0.45} for k in
                     ('claims_without_fail', 'clean_claims', 'clean_claim_false_abstention', 'clean_result_false_alarm')},
    'disagreements': {'count': 0, 'examples': []}, 'fail_error_rate': {'k': 0, 'n': 30, 'rate': 0.0, 'upper95': 0.09},
}
METRICS = {'commit': 'abc', 'sets': {'S3': {'meta': {'tier': 'A', 'name': 'data/stress', 'path': 'engine', 'label_kind': 'organizer_key'},
                                           'summary': TINY_SUMMARY, 'engine_crashes': 0, 'engine_ms': {'n': 2, 'mean': 1, 'p50': 1, 'p95': 1, 'p99': 1, 'max': 1},
                                           'baselines': {'always_pass': TINY_SUMMARY, 'starter_baseline': TINY_SUMMARY}}},
           'latency': {'unit': 'milliseconds', 'environment': {'platform': 'x', 'python': '3.10', 'cpus': 1},
                       **{k: {'n': 1, 'mean': 1, 'p50': 1, 'p95': 1, 'p99': 1, 'max': 1} for k in
                          ('engine_per_claim', 'ingest_fhir_per_claim', 'ingest_csv_per_claim', 'audited_template_per_claim')}},
           'ai_step_recorded': {'seconds': {'median': 2.86, 'p95': 28.89, 'calls': 117}, 'source': 'outputs/defense/load.json'}}
PROV = {'commit': 'abc', 'sets': [{'set_id': 'S3', 'tier': 'A', 'name': 'data/stress', 'label_kind': 'organizer_key',
                                   'label_source': 'k', 'generator': 'g', 'limitation': 'l', 'claims': 2, 'results': 30,
                                   'files': [{'path': 'data/stress/claims.jsonl', 'sha256': 'a' * 64}], 'notes': {}, 'commit': 'abc', 'path': 'engine'}]}


class RenderTests(unittest.TestCase):
    def test_undefined_values_print_as_na_not_zero(self):
        t = r.render_tables(METRICS, PROV)
        self.assertIn('n/a', t['categories'])

    def test_provenance_table_cites_the_hash_prefix(self):
        self.assertIn('aaaaaaaa', r.render_tables(METRICS, PROV)['provenance'])

    def test_apply_replaces_blocks_and_keeps_prose(self):
        doc = 'intro\n<!-- TABLE:latency -->\nold\n<!-- /TABLE:latency -->\noutro'
        out = r.apply_tables(doc, {'latency': 'NEW'})
        self.assertIn('NEW', out)
        self.assertNotIn('old', out)
        self.assertTrue(out.startswith('intro') and out.endswith('outro'))

    def test_missing_marker_is_an_error(self):
        with self.assertRaises(KeyError):
            r.apply_tables('no markers', {'latency': 'x'})


class CommittedReportTests(unittest.TestCase):
    def test_the_committed_report_matches_the_committed_evidence(self):
        metrics = json.loads((ROOT / 'outputs' / 'evaluation' / 'metrics.json').read_text(encoding='utf-8'))
        prov = json.loads((ROOT / 'outputs' / 'evaluation' / 'provenance.json').read_text(encoding='utf-8'))
        doc = (ROOT / 'docs' / '29_Test_Evaluation_Report.md').read_text(encoding='utf-8')
        self.assertEqual(r.apply_tables(doc, r.render_tables(metrics, prov)).replace('\r\n', '\n'), doc.replace('\r\n', '\n'))


if __name__ == '__main__':
    unittest.main()
```
Run: expected ERROR (`ModuleNotFoundError: render_eval_report`).

- [ ] **Step 3: Write `scripts/render_eval_report.py`** with `fmt(x)` (None -> `n/a`, float -> `f'{x:.4f}'`, int -> thousands separators), `render_tables`, `apply_tables`, `main(--check)`. Tables, as markdown:
  - `provenance`: columns `Set | Tier | Name | Claims | Results | Label source | Generator | File SHA-256 (first 8) | Limitation`, one row per provenance entry (all files' hash prefixes joined by `, `).
  - `tier_a`: rows S1 to S6 (tier A): `Set | Claims | Results | Precision | Recall | F1 | Status accuracy | Disagreements | Macro F1 by category | Macro F1 by severity | Macro F1 over 15 rules`.
  - `categories`: for each tier-A engine set (S1 to S3) and S5, one row per category with pooled counts and F1; `n/a` where undefined.
  - `per_rule`: rule x set (S1, S2, S3) F1 with the FAIL count, so undefined cells are explained.
  - `valid_claims`: for sets that have them (S1 to S3, S5, S6, S7, S8): the four rates as `k/n (rate, 95% upper bound)`.
  - `baselines`: S1 to S3 rows for always-PASS and the starter baseline: F1, recall.
  - `tier_b_c`: S7, S8, S9 agreement table: claims, results, disagreements, macro F1 by category, label kind.
  - `latency`: p50, p95, p99, mean in ms per path, `n`, environment line, and the recorded AI step (median, p95, calls, source).
  Every table iterates over the sets actually present in `metrics` and skips absent ones (the unit test passes only S3). `apply_tables` uses `re.sub` per name with the exact marker pair and `re.DOTALL`, raising `KeyError(name)` if the pair is absent. `--check` renders from `outputs/evaluation/*.json`, compares to the doc, and exits 1 on difference. Avoid the sink words in the docstring.

- [ ] **Step 4: Write `docs/29_Test_Evaluation_Report.md`.** Sections and marker blocks (prose is yours, written from the real numbers in `metrics.json`; every claim in prose must be a number that appears in a table):
  1. **Summary** (5 to 8 lines): the headline for tier A (organizer key), the valid-claim result, latency, and the one-line caveat.
  2. **What was measured and on what**: the tier model; the five rule categories and why; positive class FAIL; `<!-- TABLE:provenance -->`.
  3. **Detection quality on the organizer data (tier A)**: `<!-- TABLE:tier_a -->`, `<!-- TABLE:categories -->`, `<!-- TABLE:per_rule -->`; explain S5's R009 drop and S6; name the 50-claim split (S3) explicitly.
  4. **Preserving valid claims**: `<!-- TABLE:valid_claims -->` with the definition of each rate and how to read the upper bound, and the sample size needed to claim a lower error rate (by the rule of three, about 3 divided by the target rate in error-free claims, so roughly 300 clean claims to claim below 1%).
  5. **Baselines**: `<!-- TABLE:baselines -->`; why they show the metric discriminates.
  6. **Harder, larger sets (tier B and C)**: `<!-- TABLE:tier_b_c -->`; stress that this is agreement with our own oracle.
  7. **Latency**: `<!-- TABLE:latency -->`; the AI step is quoted from earlier live runs.
  8. **Limitations** (each as its own bullet): a perfect score on the public splits cannot rank this system against a better one; the oracle and the engine were written by the same team from the same text and could share a misreading; tier C is small and ours; the rule categories are our grouping; FHIR loses authorizations by design; the claims are synthetic and say nothing about real claim mix; engine latency is on one laptop; the AI step is not part of the detection metrics; nothing here evaluates explanation correctness (see `docs/17`, `docs/21`).
  9. **Reproduce**: `python scripts/evaluate_phase2.py` then `python scripts/render_eval_report.py` (and `--check`).
  Run `.venv/Scripts/python.exe scripts/render_eval_report.py` to fill the tables.

- [ ] **Step 5: Run the tests** `-p test_eval_report.py`, expected PASS. Then wire the docs: in `README.md` add a row to the Documents table for `docs/29_Test_Evaluation_Report.md` and update the Results table with the S3 (50-claim) and valid-claim numbers from the report; in `SPECS.md` section 10 add a row "Phase 2 evaluation" pointing to `docs/29` and `outputs/evaluation/`; in `docs/27` add one row to the experiment map with the real headline numbers and the stated limits. Do not state any number that is not in `metrics.json`.

- [ ] **Step 6: Full verification and commit**
```bash
.venv/Scripts/python.exe -m unittest discover -s tests 2>&1 | tail -4
.venv/Scripts/python.exe scripts/render_eval_report.py --check; echo "stale check exit: $?"
git add -f outputs/evaluation/metrics.json outputs/evaluation/provenance.json
git add scripts/render_eval_report.py tests/test_eval_report.py docs/29_Test_Evaluation_Report.md README.md SPECS.md docs/27_Decisions_Proofs_and_Defense.md
git commit -m "docs: Phase 2 test evaluation report with provenance-cited metrics" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```
Expected: all tests pass (523 plus the new ones), `--check` exit 0. Update the test counts in README, SPECS, `docs/17` and `docs/27` to the new total with `grep -rn "523"`; confirm with `git status` that the tree is clean, listing files explicitly (never `git add` a path that may not exist, and never hide `git add` errors).
