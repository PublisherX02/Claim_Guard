"""Handing a lease back. One place, so the dispatcher's expiry, the reconciler's repair and an administrator's tools all do it
the same way: one conditional move leased -> ready that clears the lease and records who held it and why it ended."""
from .store import check_text


def release(store, doc, now, actor, reason='lease_expired'):
    """Return the leased document to the pool. None if it was no longer leased (someone decided or released it first)."""
    holder = (doc.get('lease') or {}).get('badge_id')
    check_text(actor, 'actor')
    return store.transition(doc['claim_id'], doc['version'], 'leased', 'ready', actor, now,
                            detail={'event': reason, 'badge_id': holder or ''}, set_fields={'lease': None})
