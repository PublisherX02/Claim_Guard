"""The AI step's prompts: Rust against Python, byte for byte, for every rule and every failure shape.

    python rust/tools/compare_model.py
"""
import json, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'src'))
from workqueue import model_adapter  # noqa: E402
from workqueue.bootstrap import rulebook_rules  # noqa: E402

rules = rulebook_rules()
model = model_adapter.TemplateModel(lambda p, t: '', rules)
requests = [{'op': 'pass1_prompt', 'root': str(ROOT), 'request': {'rule_id': rid, 'failure_shape': f'{status}/{sev}/{n}', 'prompt_version': '2.1.0-template',
                                                                    'placeholders': ['{value}', '{line}']}}
            for rid in rules for status in ('FAIL', 'UNABLE_TO_ASSESS') for sev in ('high', 'medium') for n in ('0', '1', '3')]
requests.append({'op': 'pass1_prompt', 'root': str(ROOT), 'request': {'rule_id': 'R001', 'failure_shape': 'weird'}})
proc = subprocess.run([str(ROOT / 'rust' / 'target' / 'release' / 'cg.exe'), 'xcheck'], input='\n'.join(json.dumps(r) for r in requests) + '\n', capture_output=True,
                      text=True, encoding='utf-8')
answers = [json.loads(line) for line in proc.stdout.splitlines()]
bad = 0
for r, a in zip(requests, answers):
    want = model.pass1_prompt(r['request'])
    if a != want:
        bad += 1
        print('DIFF', r['request'])
print(f'{len(requests)} prompts compared, {bad} differences')
sys.exit(1 if bad or len(answers) != len(requests) else 0)
