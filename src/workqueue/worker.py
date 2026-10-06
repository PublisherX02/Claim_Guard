"""What a queue worker does, with no Celery in it, so the same code is tested directly, eagerly and behind a real broker.

process() handles one claim: it checks that the delivered message still matches the stored claim (a stale or forged message is a
no-op) and runs the pipeline. When the retries run out, dead_letter() records one dead letter and parks the claim in
`dead_lettered`, where only an administrator replay can move it on.
"""
import random
import uuid
from dataclasses import dataclass, field

from . import pipeline, states


@dataclass
class Runtime:
    store: object
    steps: object                       # has .explain, an ExplainStep
    dispatcher: object
    clock: object
    max_retries: int = 3
    rng: random.Random = field(default_factory=random.Random)


def process(rt, claim_id, version, input_hash):
    """Returns 'ready' or 'noop'. Raises whatever the pipeline raises, so the task can retry it."""
    doc = rt.store.get(claim_id, version)
    if doc is None or doc['input_hash'] != input_hash:
        return 'noop'
    return pipeline.advance(rt.store, claim_id, version, rt.steps, rt.clock())


def dead_letter(rt, claim_id, version, input_hash, exc, attempts):
    """Record one dead letter and park the claim. The reason is the error's type only: its message could quote claim text."""
    now = rt.clock()
    reason = type(exc).__name__
    rt.store.add_dead_letter({'dead_id': uuid.uuid4().hex, 'claim_id': claim_id, 'version': version, 'input_hash': input_hash,
                              'reason': reason, 'attempts': attempts, 'at': now})
    doc = rt.store.get(claim_id, version)
    if doc is not None and 'dead_lettered' in states.TRANSITIONS.get(doc['state'], ()):
        rt.store.transition(claim_id, version, doc['state'], 'dead_lettered', 'system:worker', now,
                            detail={'reason': reason, 'attempts': attempts})
