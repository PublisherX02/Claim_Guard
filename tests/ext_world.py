"""Builders for extension-rule scenarios: a clean official claim, reshaped line by line."""
import copy
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))

CLEAN = json.loads((ROOT / 'examples' / 'worked_cases.json').read_text(encoding='utf-8'))[0]['claim']
PRIMARY, COMPONENT, NEVER, RX = 'SVC-EXT-PRIMARY', 'SVC-EXT-COMPONENT', 'SVC-EXT-NEVER', 'SVC-EXT-RX'
SEPARATE = 'EDU-SEPARATE'
IDS = ['E001', 'E002', 'E003', 'E004', 'E005', 'E101', 'E102', 'E103']


def line(n, code, date='2026-03-02', modifier=None, quantity=1, price=100, auth=None):
    return {'line_id': f'L{n}', 'service_code': code, 'service_date': date, 'modifier': modifier, 'quantity': quantity,
            'unit_price': price, 'net_amount': quantity * price, 'authorization_id': auth}


def claim(lines, claim_id='C-1', patient='PAT-1', provider='EDU-PROV-01', date='2026-03-10', diagnosis='DX-EDU-01', notes=None,
          attachments=None, authorizations=None):
    c = copy.deepcopy(CLEAN)
    c.update(claim_id=claim_id, patient_id=patient, provider_id=provider, submission_date=date, diagnosis_code=diagnosis)
    c['lines'] = lines
    c['total_amount'] = sum(x['net_amount'] for x in lines)
    c['notes'] = 'Synthetic claim.' if notes is None else notes
    c['attachments'] = attachments if attachments is not None else []
    c['authorizations'] = authorizations if authorizations is not None else []
    return c


def by_rule(results):
    return {r['rule_id']: r for r in results}


def statuses(results):
    return {r['rule_id']: r['status'] for r in results}
