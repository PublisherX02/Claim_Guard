"""The feedback report: what the people decided about the engine's findings, summed up so the rules can be reviewed.

It reads the queue store (the source of truth for decisions: a decision is written there before it is copied to the review log) and
changes nothing. Per rule it counts how often a flagged finding was finally confirmed or dismissed by a person, how often a reviewer
asked for information or marked a claim corrected, and how often the second senior disagreed with the first. A rule that people
keep dismissing is a candidate for a human review of the rule, never an automatic change: the engine's rules stay exactly as they are.

Only counts leave this module. A dismissal's reason is free text typed by a person and may contain personal data, so the report
carries how many reasons were recorded, not what they said; no claim id, badge id or patient value appears either.

A rate on a few decisions is not a finding: a rule is only marked a review candidate when it has at least `min_decisions` final
decisions and the exact lower bound (Clopper-Pearson, 95 %) of its dismissal rate is above `threshold`.
"""
from . import shadow

BIG = 100000
MIN_DECISIONS = 20
THRESHOLD = 0.5
FLAGGED = ('FAIL', 'UNABLE_TO_ASSESS')


def _final_actions(doc):
    """The last resolving action per rule (what finally stood), and how many decisions of each other kind were recorded."""
    final, extra = {}, {'request_information': 0, 'mark_corrected_for_recheck': 0}
    for d in doc.get('decisions') or []:
        action = d.get('action')
        if action in ('confirm_issue', 'dismiss_with_reason'):
            final[d['rule_id']] = action
        elif action in extra:
            extra[action] += 1
    return final, extra


def report(store, min_decisions=MIN_DECISIONS, threshold=THRESHOLD, confidence=0.95):
    if not isinstance(min_decisions, int) or isinstance(min_decisions, bool) or min_decisions < 1:
        raise ValueError('min_decisions must be a positive integer')
    if not isinstance(threshold, (int, float)) or isinstance(threshold, bool) or not 0 <= threshold <= 1:
        raise ValueError('threshold must be between 0 and 1')
    rules = {}

    def row(rule_id):
        return rules.setdefault(rule_id, {'flagged': 0, 'confirmed': 0, 'dismissed': 0, 'request_information': 0,
                                          'mark_corrected_for_recheck': 0, 'dismissals_with_reason': 0})

    decided = escalated = verified_clear = 0
    agreed = tiebroken = 0
    for doc in store.by_state('decided', BIG):
        decided += 1
        flagged = [r['rule_id'] for r in doc.get('results') or [] if r.get('status') in FLAGGED]
        for rule_id in flagged:
            row(rule_id)['flagged'] += 1
        if not flagged:
            verified_clear += 1
        final, extra = _final_actions(doc)
        for d in doc.get('decisions') or []:
            if d.get('action') == 'dismiss_with_reason' and (d.get('reason') or '').strip():
                row(d['rule_id'])['dismissals_with_reason'] += 1
        for rule_id, action in final.items():
            row(rule_id)['confirmed' if action == 'confirm_issue' else 'dismissed'] += 1
        for d in doc.get('decisions') or []:
            if d.get('action') in extra:
                row(d['rule_id'])[d['action']] += 1
        escalated += bool(doc.get('escalated'))
        outcome = (doc.get('signoff') or {}).get('outcome')
        agreed += outcome == 'agreed'
        tiebroken += outcome == 'tiebreak'
    for stats in rules.values():
        n = stats['confirmed'] + stats['dismissed']
        lower, upper = shadow.clopper_pearson(stats['dismissed'], n, confidence)
        stats['decisions'] = n
        stats['dismissal_rate'] = (stats['dismissed'] / n) if n else None
        stats['dismissal_rate_lower'], stats['dismissal_rate_upper'] = lower, upper
        stats['review_candidate'] = bool(n >= min_decisions and lower is not None and lower > threshold)
    countersigned = agreed + tiebroken
    return {'claims_decided': decided, 'verified_clear': verified_clear, 'escalated': escalated,
            'signoff': {'countersigned': countersigned, 'agreed': agreed, 'disagreed': tiebroken,
                        'disagreement_rate': (tiebroken / countersigned) if countersigned else None},
            'rules': {rid: rules[rid] for rid in sorted(rules)},
            'parameters': {'min_decisions': min_decisions, 'threshold': threshold, 'confidence': confidence},
            'note': 'counts only; dismissal reasons are not included. A review candidate is a prompt for a human to look at the rule, '
                    'never a change to it.'}
