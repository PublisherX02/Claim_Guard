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
        self.breakdown = Counter()        # (rule, gold, predicted) -> how many times they disagreed

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
                self.breakdown[(rid, g, p)] += 1
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
            'disagreements': {'count': self.disagree, 'examples': list(self.examples),
                              'breakdown': [{'rule_id': r, 'gold': g, 'predicted': p, 'count': n}
                                            for (r, g, p), n in sorted(self.breakdown.items())]},
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
