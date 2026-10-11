"""The Celery adapter: thin on purpose. The work is in worker.py, pipeline.py and the dispatcher; this file only schedules it.

Redis is the broker and nothing else: MongoDB holds every claim, so losing Redis loses no claim (the outbox relay republishes what
was not delivered). Messages are acknowledged after the work (acks_late) and requeued if a worker dies (reject_on_worker_lost), so
delivery is at least once; process_claim is idempotent, so a duplicate does nothing. Workers need Linux (the Docker image); on
Windows the demo runs the same tasks eagerly.
"""
from celery import Celery

from . import reconcile as reconcile_mod
from . import relay
from .jobs import run_job
from .explain import backoff
from .worker import dead_letter, process

PROCESS = 'workqueue.process_claim'
PUBLISH_RETRY = {'max_retries': 1, 'interval_start': 0, 'interval_step': 0.1, 'interval_max': 0.2}


def make_app(broker_url, backend=None, *, eager=False, runtime=None):
    """Build the Celery app. `runtime` (worker.Runtime) can also be attached later as app.runtime."""
    app = Celery('claimguard_queue', broker=broker_url, backend=backend)
    app.conf.update(
        task_acks_late=True,
        task_reject_on_worker_lost=True,
        broker_transport_options={'visibility_timeout': 3600},     # longer than the 90 s AI ceiling plus its retries
        task_serializer='json', result_serializer='json', accept_content=['json'],
        task_always_eager=bool(eager), task_eager_propagates=False,
        broker_connection_retry_on_startup=True,
        timezone='UTC', enable_utc=True,
        beat_schedule={
            'deal': {'task': 'workqueue.deal', 'schedule': 15.0},
            'expire': {'task': 'workqueue.expire', 'schedule': 60.0},
            'relay': {'task': 'workqueue.relay_sweep', 'schedule': 10.0},
            'reconcile': {'task': 'workqueue.reconcile', 'schedule': 300.0},
        },
    )
    app.runtime = runtime

    @app.task(bind=True, name=PROCESS, shared=False)
    def process_claim(self, claim_id, version, input_hash):
        rt = app.runtime
        try:
            return process(rt, claim_id, version, input_hash)
        except Exception as exc:  # noqa: BLE001 - any failure is retried, then dead-lettered
            attempt = self.request.retries
            if attempt >= rt.max_retries:
                dead_letter(rt, claim_id, version, input_hash, exc, attempt + 1)
                return 'dead_lettered'
            raise self.retry(exc=exc, countdown=backoff(attempt, rng=rt.rng), max_retries=rt.max_retries)

    @app.task(name='workqueue.deal', shared=False)
    def deal():
        rt = app.runtime
        return run_job(rt.store, rt.clock, 'deal', lambda: len(rt.dispatcher.deal(full=False).assigned))

    @app.task(name='workqueue.expire', shared=False)
    def expire():
        rt = app.runtime
        return run_job(rt.store, rt.clock, 'expire', rt.dispatcher.expire)

    @app.task(name='workqueue.relay_sweep', shared=False)
    def relay_sweep():
        rt = app.runtime
        return run_job(rt.store, rt.clock, 'relay', lambda: relay.sweep(rt.store, publisher(app), rt.clock()))

    @app.task(name='workqueue.reconcile', shared=False)
    def reconcile():
        rt = app.runtime
        report = run_job(rt.store, rt.clock, 'reconcile', lambda: reconcile_mod.reconcile(rt.store, rt.clock()))
        return {'ok': report.ok, 'findings': len(report.findings)}

    return app


def publisher(app):
    """publish(claim_id, version, input_hash) for the relay. Fails fast when the broker is down so the marker stays for the next sweep."""
    def publish(claim_id, version, input_hash):
        app.tasks[PROCESS].apply_async(args=(claim_id, version, input_hash), retry=True, retry_policy=PUBLISH_RETRY)
    return publish
