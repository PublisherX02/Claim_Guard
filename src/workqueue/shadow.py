"""Shadow mode: record what an automatic clearer would have done, and compare it with what the people decided. It acts on nothing.

The baseline predictor is a plain rule: a claim with no flagged findings would be cleared, every other claim would not. It stands in
for whatever a later, trained model would be; the interface is just `predict(receipt) -> probability that the claim is clear`. Whether
any trained model may ever sit in the decision path is a question for the mentor, and nothing here lets it: the prediction is stored
beside the claim, never used for routing, and written once so it cannot be adjusted after the people have decided.

agreement() reports how often the people's outcome matched the prediction, with an exact (Clopper-Pearson) 95 % interval, so a small
sample is not mistaken for a result. The interval is computed from the binomial distribution directly; no statistics package is used.
"""
import math

from . import triage

MODEL = 'baseline-green-clears'
BIG = 100000


def predict_clear(receipt):
    """1.0 for a claim with nothing flagged and a sound result set, 0.0 for every other claim."""
    return 1.0 if receipt.get('lane') == 'green' and not receipt.get('degraded') else 0.0


def record(store, claim_id, version, prediction, now):
    """Store the prediction once. Returns False if one was already stored (or the claim is unknown); never raises for that."""
    if type(prediction) is not float or not 0.0 <= prediction <= 1.0:
        raise ValueError('a prediction must be a probability between 0 and 1')
    return store.set_shadow(claim_id, version, {'predicted_clear': prediction, 'model': MODEL, 'at': now})


def human_cleared(doc):
    """True when the people decided the claim has nothing wrong: no flagged finding was confirmed (a green claim was verified clear)."""
    latest = {}
    for d in doc.get('decisions') or []:
        latest[d['rule_id']] = d['action']
    return not any(action == 'confirm_issue' for action in latest.values())


def _cdf(k, n, p):
    """P(X <= k) for X ~ Binomial(n, p), summed from log-probabilities so large n does not overflow."""
    if k < 0:
        return 0.0
    if k >= n:
        return 1.0
    if p <= 0.0:
        return 1.0
    if p >= 1.0:
        return 0.0
    log_p, log_q = math.log(p), math.log1p(-p)
    total = 0.0
    for i in range(k + 1):
        total += math.exp(math.lgamma(n + 1) - math.lgamma(i + 1) - math.lgamma(n - i + 1) + i * log_p + (n - i) * log_q)
    return min(1.0, total)


def _bisect(f, target, increasing):
    low, high = 0.0, 1.0
    for _ in range(80):
        mid = (low + high) / 2
        if (f(mid) < target) == increasing:
            low = mid
        else:
            high = mid
    return (low + high) / 2


def clopper_pearson(k, n, confidence=0.95):
    """Exact two-sided interval for a proportion k/n, or (None, None) when n is 0."""
    if n == 0:
        return None, None
    if not 0 <= k <= n:
        raise ValueError('k must be between 0 and n')
    alpha = 1 - confidence
    lower = 0.0 if k == 0 else _bisect(lambda p: 1.0 - _cdf(k - 1, n, p), alpha / 2, increasing=True)
    upper = 1.0 if k == n else _bisect(lambda p: _cdf(k, n, p), alpha / 2, increasing=False)
    return lower, upper


def agreement(store, confidence=0.95):
    """Over the decided claims that carry a shadow prediction: how often prediction and people agreed."""
    n = agree = 0
    for doc in store.by_state('decided', BIG):
        shadow = doc.get('shadow')
        if not shadow:
            continue
        n += 1
        agree += (shadow['predicted_clear'] >= 0.5) == human_cleared(doc)
    lower, upper = clopper_pearson(agree, n, confidence)
    return {'n': n, 'agree': agree, 'rate': (agree / n) if n else None, 'confidence': confidence, 'lower': lower, 'upper': upper,
            'model': MODEL}
