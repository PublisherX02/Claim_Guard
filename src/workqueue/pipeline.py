"""The processing pipeline for one stored claim: triaged -> (explained | explanation_skipped) -> ready.

advance() is idempotent and safe to run twice, out of order or after a crash. It reads the stored state and does the work still
owed: a green claim goes straight to `ready`; a lane A or B claim gets its explanation first (a step that never raises for model
behaviour), then goes to `ready`. A claim that is already past those states (ready, leased, decided, dead-lettered) is left alone.
If a worker died between the explanation and the move to `ready`, the redelivery sees `explained`, does not call the model again,
and finishes the move. Dealing is not a step here: the dispatcher owns the `ready` pool.
"""
from . import states

PIPELINE_STATES = ('triaged', 'explained', 'explanation_skipped')


def advance(store, claim_id, version, steps, now):
    """Run the claim up to `ready`. Returns 'ready' when this call moved it there, otherwise 'noop'."""
    doc = store.get(claim_id, version)
    if doc is None or doc['state'] not in PIPELINE_STATES:
        return 'noop'
    if doc['state'] == 'triaged':
        if doc['receipt']['lane'] == 'green':
            return _ready(store, claim_id, version, 'triaged', now, {'why': 'green'})
        if steps.explain.run(claim_id, version) == 'noop':
            return 'noop'                                     # another worker is handling it
        doc = store.get(claim_id, version)
        if doc is None or doc['state'] not in ('explained', 'explanation_skipped'):
            return 'noop'
    return _ready(store, claim_id, version, doc['state'], now, {'why': 'explained' if doc['state'] == 'explained' else 'skipped'})


def _ready(store, claim_id, version, frm, now, detail):
    states.check_transition(frm, 'ready')
    moved = store.transition(claim_id, version, frm, 'ready', 'system:pipeline', now, detail=detail)
    return 'ready' if moved is not None else 'noop'
