#!/usr/bin/env python3
"""S4. Claims from PDFs and photos on the server: time per document and field accuracy against a known answer (docs/65).

    python rust/tools/make_docs.py --out <dir> --n 30        # once, on any machine with reportlab, pypdfium2, pillow, numpy
    CG_URL=... CG_BADGE=... python experiments/server/s04_documents.py --run <name> --docs <dir> [--limit 40]

Each document is sent to POST /api/v1/queue/import/file (the same route as the console's Import page) and the draft claim that comes back is
compared field by field with <dir>/truth.json. Reported per condition (digital PDF, scan, photo, image-only PDF): documents read, seconds per document,
and the share of fields read exactly. The reader allows two documents at a time; a 429 "busy" is retried after a short wait and counted.
The numbers of docs/45 (97.4 % of fields with the two readers) were measured on the build PC; this shows what the server does.
"""
import argparse
import json
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from common import Api, Run, summary  # noqa: E402

CONDITION_OF = {'digital': 'digital PDF', 'scan': 'scan', 'photo': 'photo', 'scanpdf': 'image-only PDF'}


def condition(name):
    stem = Path(name).stem.lower()
    for k in CONDITION_OF:
        if stem.endswith('_' + k) or ('_' + k + '_') in stem or stem.endswith(k):
            return CONDITION_OF[k]
    return 'other'


def draft_of(body):
    """The draft claim of a document import: {"kind": "document", "draft": {"claim": {...}, "warnings": [...]}}."""
    d = body.get('draft') if isinstance(body, dict) else None
    return (d.get('claim') if isinstance(d, dict) else None) or {}


def scalars(truth_row):
    """The single-valued fields the page shows (everything except the coverage block and the lines)."""
    return [k for k, v in truth_row.items() if not isinstance(v, (dict, list)) and v is not None]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--run', required=True)
    ap.add_argument('--docs', required=True)
    ap.add_argument('--limit', type=int, default=40)
    ap.add_argument('--dry', action='store_true')
    a = ap.parse_args()
    run, api, root = Run(a.run, a.dry), Api().login(), Path(a.docs)
    truth = json.loads((root / 'truth.json').read_text(encoding='utf-8'))
    files = sorted(p for p in root.iterdir() if p.suffix.lower() in ('.pdf', '.jpg', '.jpeg', '.png') and p.name in truth)[:a.limit]
    per, busy, failed = defaultdict(lambda: {'times': [], 'fields_ok': 0, 'fields': 0, 'docs': 0, 'docs_exact': 0}), 0, []
    for p in files:
        for attempt in range(6):
            status, body, dt = api.call('POST', '/api/v1/queue/import/file', raw=p.read_bytes(), headers={'x-file-name': p.name})
            if status == 429:
                busy += 1
                time.sleep(2)
                continue
            break
        if status != 200:
            failed.append({'file': p.name, 'status': status, 'detail': str(body)[:120]})
            continue
        got, want, c = draft_of(body), truth[p.name], per[condition(p.name)]
        keys = scalars(want)
        exact = sum(1 for f in keys if str(got.get(f, '')).strip() == str(want[f]).strip())
        n = len(keys)
        c['times'].append(dt)
        c['fields_ok'] += exact
        c['fields'] += n
        c['docs'] += 1
        c['docs_exact'] += exact == n
    rows = [{'condition': k, 'documents': v['docs'], 'seconds_per_document': summary(v['times']), 'field_accuracy': v['fields_ok'] / v['fields'] if v['fields'] else None,
             'documents_all_fields_right': v['docs_exact'] / v['docs'] if v['docs'] else None} for k, v in sorted(per.items())]
    allt = [t for v in per.values() for t in v['times']]
    tot_ok, tot = sum(v['fields_ok'] for v in per.values()), sum(v['fields'] for v in per.values())
    result = {'files_tried': len(files), 'failed': failed, 'busy_retries': busy, 'conditions': rows, 'seconds_per_document': summary(allt),
              'field_accuracy': tot_ok / tot if tot else None, 'reference': 'docs/45: 97.4 % of fields, 66.7 % of documents fully right, two readers, on the build PC'}
    result['checks'] = {'no_failed_documents': not failed, 'field_accuracy_at_least_0_95': bool(tot) and tot_ok / tot >= 0.95}
    run.write('s04_documents', result)


if __name__ == '__main__':
    main()
