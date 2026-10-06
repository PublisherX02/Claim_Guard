"""The entry point for a Celery worker and its scheduler:

    celery -A workqueue.celery_app worker --beat --loglevel=info        (Linux; see docs/31)

It reads the same environment as the reviewer API (the secrets and MONGO_URI) plus REDIS_URL for the broker, builds the access and
queue stacks, and exposes `app`. Importing this module needs a complete, safe configuration: a missing secret stops the worker with the
same configuration error the server gives. Nothing here is imported by the offline engine, the evaluation or the tests of other modules.
"""
import os

from access import bootstrap as access_bootstrap
from access import config

from . import bootstrap, tasks

env = dict(os.environ)
if not (env.get('REDIS_URL') or '').strip():
    raise config.ConfigError('REDIS_URL is required for the work queue worker')
stack = access_bootstrap.build_stack(env)
queue = bootstrap.build_queue(stack, env)
app = tasks.make_app(env['REDIS_URL'].strip(), runtime=queue.runtime)
