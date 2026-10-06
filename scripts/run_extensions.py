"""Run the advisory extension rules (E001 to E005, E101 to E103) on a JSONL file of claims.

Extension findings are advisory: they are written to their own file and never touch the official fifteen-rule results.
Rules E101 to E103 compare a claim with earlier claims of the same patient; by default the history is the claim file itself
(use --history for another file, or --no-history to run without any, which makes those three rules UNABLE_TO_ASSESS).

    python scripts/run_extensions.py --claims data/development/claims.jsonl --out outputs/extensions/development.jsonl
"""
import argparse
import json
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
from claim_history import InMemoryHistory
from engine_core import load_jsonl
import extension_rules as ex


def run(claims, history_claims=None):
    """The advisory results for every claim, in claim order then rule order. history_claims=None means no history."""
    history = None if history_claims is None else InMemoryHistory(history_claims)
    rows, errors = [], []
    for c in claims:
        rows.extend(ex.evaluate_extensions(c, history, tool_errors=errors))
    return rows, errors


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--claims', required=True)
    group = ap.add_mutually_exclusive_group()
    group.add_argument('--history', help='JSONL of earlier claims (default: the claims file itself)')
    group.add_argument('--no-history', action='store_true')
    ap.add_argument('--out', default='outputs/extensions/results.jsonl')
    a = ap.parse_args(argv)
    try:
        claims = load_jsonl(a.claims)
        history = None if a.no_history else (load_jsonl(a.history) if a.history else claims)
        rows, errors = run(claims, history)
    except (OSError, ValueError) as e:
        ap.exit(2, f'Extension run rejected: {e}\n')
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(''.join(json.dumps(r, sort_keys=True, ensure_ascii=False) + '\n' for r in rows), encoding='utf-8')
    counts = Counter((r['rule_id'], r['status']) for r in rows)
    for rid in sorted({k[0] for k in counts}):
        print(rid, {s: n for (r, s), n in sorted(counts.items()) if r == rid})
    if errors:
        print(f'{len(errors)} rule exception(s) were isolated as UNABLE_TO_ASSESS', file=sys.stderr)
    print('Results:', out)
    return 0


if __name__ == '__main__':
    sys.exit(main())
