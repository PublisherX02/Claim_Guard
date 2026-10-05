"""The outbox relay: publishes the claims whose task has not been handed to the broker yet.

Intake writes a claim and its "publish pending" marker in one atomic write, so a crash between that write and the publish
loses nothing: the marker is still there and the next sweep publishes it. The marker is cleared only after the publish
returned, so a broker outage leaves it for the next sweep. A claim can therefore be published twice (publish succeeded, the
clear failed); the consuming task is idempotent on (claim_id, input_hash), so that is harmless.
"""
BATCH = 500


def sweep(store, publish, now, max_age=60):
    """publish(claim_id, version, input_hash). Returns counts of published, failed and stale (older than max_age) markers."""
    published = failed = stale = 0
    for doc in store.pending_outbox(BATCH):
        if now - doc['receipt']['created_at'] > max_age:
            stale += 1
        try:
            publish(doc['claim_id'], doc['version'], doc['input_hash'])
        except Exception:  # noqa: BLE001 - any broker failure leaves the marker for the next sweep
            failed += 1
            continue
        store.clear_outbox(doc['claim_id'], doc['version'])
        published += 1
    return {'published': published, 'failed': failed, 'stale': stale}
