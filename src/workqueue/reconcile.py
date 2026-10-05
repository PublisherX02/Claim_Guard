"""Reconciliation: a periodic audit that the queue's documents are in sane states, with one safe repair.

It reports, and repairs only what has one obviously correct repair: a lease past its expiry goes back to the pool through the
normal hand-back. Everything else (a document stuck in a state, a marker nobody published, a count that does not add up, two live
leases on one claim) is reported for a person to look at, never guessed at.
"""
from dataclasses import dataclass, field

from . import leases, states

DEFAULT_LIMITS = {'triaged_seconds': 60, 'explained_seconds': 60, 'ready_seconds': 86400, 'outbox_seconds': 60}
BIG = 100000


@dataclass
class Report:
    findings: list = field(default_factory=list)

    @property
    def ok(self):
        return all(f.get('repaired') for f in self.findings)


def _add(report, check, doc=None, repaired=False, **extra):
    row = {'check': check, 'repaired': repaired, **extra}
    if doc is not None:
        row.update(claim_id=doc['claim_id'], version=doc['version'])
    report.findings.append(row)


def reconcile(store, now, cfg_limits=None, actor='system:reconcile'):
    limits = {**DEFAULT_LIMITS, **(cfg_limits or {})}
    report = Report()
    counts = store.counts()
    total = sum(counts.values())
    for key, n in counts.items():
        if key.split('|', 1)[0] not in states.STATES:
            _add(report, 'unknown_state', state=key.split('|', 1)[0], count=n)
    listed, leased = 0, {}
    for state in states.STATES:
        docs = store.by_state(state, BIG)
        listed += len(docs)
        for d in docs:
            age = now - d['state_at']
            if state == 'triaged' and age > limits['triaged_seconds']:
                _add(report, 'stuck', d, state=state, age_seconds=age)
            elif state in ('explained', 'explanation_skipped') and age > limits['explained_seconds']:
                _add(report, 'stuck', d, state=state, age_seconds=age)
            elif state == 'ready' and age > limits['ready_seconds']:
                _add(report, 'waiting_too_long', d, state=state, age_seconds=age)
            elif state == 'decided' and not d.get('decided_by'):
                _add(report, 'decided_without_a_person', d)
            elif state == 'leased':
                lease = d.get('lease')
                if not lease:
                    _add(report, 'leased_without_a_lease', d)
                    continue
                leased.setdefault(d['claim_id'], []).append(d)
                if lease['expires_at'] <= now:
                    moved = leases.release(store, d, now, actor)
                    _add(report, 'lease_expired', d, repaired=moved is not None, badge_id=lease['badge_id'])
    for docs in leased.values():
        if len(docs) > 1:
            _add(report, 'two_live_leases', docs[0], versions=sorted(d['version'] for d in docs))
    if listed != total:
        _add(report, 'count_mismatch', documents=total, listed=listed)
    for d in store.pending_outbox(BIG):
        if now - d['receipt']['created_at'] > limits['outbox_seconds']:
            _add(report, 'unpublished_marker', d, age_seconds=now - d['receipt']['created_at'])
    return report
