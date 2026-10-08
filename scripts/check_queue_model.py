"""Check that the model the queue is configured to use works: ask it for the explanation template of each of the 15 rules and report
how many it produced in the required shape (plain text with only {value} and {line} as braces), and how long each took.

    QUEUE_AI_PROVIDER=ollama OLLAMA_MODEL=gemma3:4b python scripts/check_queue_model.py
    docker compose -f deploy/compose.yml --env-file .env exec worker python scripts/check_queue_model.py     # on the server

Exit 0 only if all 15 were accepted. No claim data is involved: the model sees the rulebook's own text. A rejected template is not an
error in production (the claim keeps the engine's own explanation); this tells you how often that would happen with this model.
"""
import os
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from workqueue import model_adapter  # noqa: E402
from workqueue.bootstrap import rulebook_rules  # noqa: E402


def main():
    rules = rulebook_rules()
    try:
        model = model_adapter.model_from_env(dict(os.environ), rules)
    except ValueError as e:
        print('error:', e)
        return 2
    if model is None:
        print('QUEUE_AI_PROVIDER is not set: the queue would use no model. Set it to ollama, featherless or nvidia.')
        return 2
    accepted = 0
    for rule_id in sorted(rules):
        shape = f"FAIL/{rules[rule_id].get('severity', 'high')}/1"
        start = time.time()
        try:
            text = model({'rule_id': rule_id, 'failure_shape': shape}, time.monotonic() + 90)
            accepted += 1
            print(f'{rule_id} ok {time.time() - start:5.1f}s  {text[:110]!r}')
        except Exception as e:  # noqa: BLE001 - report every failure, whatever its class
            print(f'{rule_id} REJECTED {time.time() - start:5.1f}s  {type(e).__name__}: {str(e)[:90]}')
    print(f'accepted {accepted} of {len(rules)} with {model.name}')
    return 0 if accepted == len(rules) else 1


if __name__ == '__main__':
    sys.exit(main())
