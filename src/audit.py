"""Local tamper-evident audit demonstration; not immutable storage or authentication."""
from pathlib import Path
import json,hashlib,argparse
from datetime import datetime,timezone
def digest(obj):return hashlib.sha256(json.dumps(obj,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
def verify(path):
    previous='0'*64;count=0
    if not Path(path).exists():return previous,count
    for line in Path(path).read_text(encoding='utf-8').split('\n'):  # not splitlines(): U+2028 in a reviewer's reason must not split a record
        if not line.strip():continue
        row=json.loads(line)
        try:claimed=row.pop('hash');linked=row['previous_hash']==previous
        except (KeyError,TypeError,AttributeError) as e:raise ValueError(f'Audit chain invalid at event {count+1}: malformed record') from e  # valid JSON, not an audit row
        if not linked or digest(row)!=claimed:raise ValueError(f'Audit chain invalid at event {count+1}')
        previous=claimed;count+=1
    return previous,count
def append(path,events):
    previous,count=verify(path);out=Path(path);out.parent.mkdir(parents=True,exist_ok=True)
    allowed={'confirm_issue','dismiss_with_reason','request_information','mark_corrected_for_recheck'}
    for event in events:
        if event.get('action') not in allowed or not event.get('actor') or not event.get('claim_id') or not event.get('rule_id'):raise ValueError('Invalid review event')
        if not event.get('reason','').strip():raise ValueError('Review reason is required')
    with out.open('a',encoding='utf-8') as f:
        for event in events:
            row={'sequence':count+1,'recorded_at':datetime.now(timezone.utc).isoformat(),'previous_hash':previous,'event':event}
            previous=digest(row);f.write(json.dumps({**row,'hash':previous})+'\n');count+=1
    return previous,count
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--log',default='outputs/audit.jsonl');p.add_argument('--events');p.add_argument('--verify',action='store_true');a=p.parse_args()
    if a.events:
        events=[json.loads(l) for l in Path(a.events).read_text(encoding='utf-8').splitlines() if l.strip()];h,n=append(a.log,events)
    else:h,n=verify(a.log)
    print(f'Chain valid: {n} events; head {h}. Keep a trusted external copy of this head to detect whole-log replacement or truncation.')
