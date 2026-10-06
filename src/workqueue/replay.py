"""Replay and rerun: prove what a stored receipt says, and apply a new rule pack without rewriting history.

replay_claim re-runs the stored claim through the engine and compares result hashes, and also checks that the stored claim and
results still match the hashes written on the receipt (a claim edited in the database after intake is caught here).
rerun finds the claims evaluated by an old rule pack, and only those whose results change under the new one become a new version
with a new receipt; the old version, its decisions and its audit trail stay exactly as they were.
"""
from . import states, triage
from .service import Conflict, NotFound

BIG = 100000


def _status_row(row):
    return (row.get('status'), row.get('severity'), tuple(row.get('affected_line_ids') or ()))


def replay_claim(store, engine, claim_id, version=None):
    doc = store.get(claim_id, version)
    if doc is None:
        raise NotFound(claim_id)
    results = engine(doc['claim'])
    receipt = doc['receipt']
    new_hash = triage.result_hash(results)
    old = {r.get('rule_id'): _status_row(r) for r in doc['results'] if isinstance(r, dict)}
    new = {r.get('rule_id'): _status_row(r) for r in results if isinstance(r, dict)}
    diff = [{'rule_id': rid, 'was': old.get(rid), 'now': new.get(rid)} for rid in sorted(set(old) | set(new), key=str)
            if old.get(rid) != new.get(rid)]
    return {'claim_id': claim_id, 'version': doc['version'], 'same': new_hash == receipt['result_hash'],
            'old_hash': receipt['result_hash'], 'new_hash': new_hash, 'diff': diff,
            'claim_intact': triage.input_hash(doc['claim']) == receipt['input_hash'],
            'results_intact': triage.result_hash(doc['results']) == receipt['result_hash']}


def latest_documents(store):
    """The newest version of every claim."""
    newest = {}
    for state in states.STATES:
        for d in store.by_state(state, BIG):
            if d['claim_id'] not in newest or d['version'] > newest[d['claim_id']]['version']:
                newest[d['claim_id']] = d
    return [newest[k] for k in sorted(newest)]


def rerun(store, engine, old_rule_pack_hash, intake, dry_run=True):
    if intake.rule_pack_hash == old_rule_pack_hash:
        raise ValueError('the running rule pack is the one to be replaced; there is nothing to rerun')
    checked, changed, errors = 0, [], []
    for doc in latest_documents(store):
        if doc['receipt'].get('rule_pack_hash') != old_rule_pack_hash:
            continue
        checked += 1
        try:
            if triage.result_hash(engine(doc['claim'])) != doc['receipt']['result_hash']:
                changed.append(doc['claim_id'])
        except Exception:  # noqa: BLE001 - one bad claim must not stop the rerun; it is reported
            errors.append(doc['claim_id'])
    created = []
    if not dry_run:
        for claim_id in changed:
            intake.submit(store.get(claim_id)['claim'])
            created.append({'claim_id': claim_id, 'version': store.get(claim_id)['version']})
    return {'dry_run': dry_run, 'checked': checked, 'changed': changed, 'unchanged': checked - len(changed) - len(errors),
            'errors': errors, 'created': created}


def replay_dead_letter(store, dead_id, now, actor):
    """Move a dead-lettered claim back to `triaged` (and re-arm its publish marker), once only."""
    letter = next((d for d in store.dead_letters(BIG) if d['dead_id'] == dead_id), None)
    if letter is None:
        raise NotFound(dead_id)
    moved = store.transition(letter['claim_id'], letter['version'], 'dead_lettered', 'triaged', actor, now,
                             detail={'event': 'dead_letter_replay', 'dead_id': dead_id}, set_fields={'enqueue_pending': True})
    if moved is None:
        raise Conflict('the claim is no longer dead-lettered')
    store.pop_dead_letter(dead_id)
    return moved
