"""Builds the question set for the KAG / CAG experiment (docs/60) and its gold answers.

Every question is about how the rulebook's parts connect, the kind a single passage does not answer. The gold is NOT taken from the graph code: it is
read from the data files (policy values, the field dictionary's `used_by`) and from a hand-made table of which rule reads which policy attribute,
written by reading the fifteen rule texts (rules/rules.json), so the graph is checked against something it was not built from.

    python experiments/kag_cag/build_questions.py        writes questions.json (+ questions.jsonl for `cg ask --file`)

A question passes when every gold token (a rule id, a value, a policy id) is in the answer. Evidence tier: C (hand-derived from the rule texts) for
the rule-to-attribute links; B/A-style direct reads for the values. The questions were written by the same author as the planner, so the planner's
rule-based pass over them is NOT an out-of-sample figure; the French and Arabic questions and the regression sets (experiments/rag_v2) are the
ones the planner was not written against.
"""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
rules = {r['rule_id']: r for r in json.loads((ROOT / 'rules/rules.json').read_text(encoding='utf8'))}
policies = json.loads((ROOT / 'rules/policies.json').read_text(encoding='utf8'))
services = json.loads((ROOT / 'rules/services.json').read_text(encoding='utf8'))
glossary = json.loads((ROOT / 'rust/crates/cg-core/data/field_glossary.json').read_text(encoding='utf8'))

# Hand-made from reading rules/rules.json: which rule reads which policy attribute (and which rule is named as the follow-up of another).
READS = {'allowed_providers': ['R005'], 'auth_required_services': ['R008'], 'required_documents': ['R010'], 'max_unit_price': ['R013'],
         'max_quantity_per_line': ['R013'], 'submission_window_days': ['R014'], 'currency': ['R015']}
FOLLOWS = {'R008': ['R009']}  # R008's text says "R009 checks the record"

Q = []


def add(cat, q, gold, lang='en', needs_model=False):
    Q.append({'id': f'K{len(Q) + 1:03d}', 'cat': cat, 'q': q, 'gold': gold, 'lang': lang, 'needs_model': needs_model})


# 1. a service and a policy: which rules depend on them (multi-hop through the policy)
T = ['Which rules can fire for {s} under {p}?', 'What do I need to watch for when a claim bills {s} to policy {p}?', 'If a claim has a {s} line on {p}, which checks apply to it?']
for i, (s, svc) in enumerate(sorted(services.items())):
    for j, (p, pol) in enumerate(sorted(policies.items())):
        gold = {'R011'}  # the catalogue check is for every code
        if s in pol['auth_required_services']:
            gold |= {'R008', 'R009'}
        if s in pol['required_documents']:
            gold.add('R010')
        if s in pol['max_unit_price'] or s in pol['max_quantity_per_line']:
            gold.add('R013')
        add('service_rules', T[(i + j) % 3].format(s=s, p=p), sorted(gold))

# 2. which rule reads a policy attribute
for q, attr in [('Which rule checks that the provider is in the insurer network?', 'allowed_providers'),
                ('Which rules look at whether a service needs prior authorization?', 'auth_required_services'),
                ('Which rule makes sure the required supporting document is attached?', 'required_documents'),
                ('Which rule enforces the maximum price per unit?', 'max_unit_price'),
                ('Which rule limits how many units can be on one line?', 'max_quantity_per_line'),
                ('Which rule enforces the submission deadline in days?', 'submission_window_days'),
                ('Which rule compares the claim currency with the policy?', 'currency')]:
    gold = list(READS[attr])
    for r in list(gold):
        gold += FOLLOWS.get(r, [])
    add('attr_rules', q, sorted(set(gold)))

# 3. which rules read a field (the dictionary's used_by)
SC = {'claim': '', 'coverage': 'coverage.', 'line': 'lines[].', 'authorization': 'authorizations[].', 'attachment': 'attachments[].'}
for field, q in [('lines[].service_date', 'Which rules use the service date of a line?'), ('invoice_number', 'Which rules read the invoice number field?'),
                 ('currency', 'Which rules depend on the currency field of a claim?'), ('coverage.status', 'Which rules read coverage.status?'),
                 ('provider_id', 'Which rules read the provider_id field?'), ('total_amount', 'Which rules look at the total_amount?'),
                 ('lines[].modifier', 'Which rules read the modifier on a line?'), ('attachments[].type', 'Which rules use the attachment type?')]:
    found = None
    for sc, pre in SC.items():
        for k, f in glossary['scopes'][sc].items():
            if f'{pre}{k}' == field:
                found = f['used_by']
    add('field_rules', q, sorted(found))

# 4. which fields a rule reads
for rule, q in [('R003', 'What claim data does rule R003 look at?'), ('R004', 'Which fields does R004 compare?'), ('R010', 'Which fields does rule R010 read?'),
                ('R012', 'What inputs does R012 use?'), ('R014', 'Which fields does R014 read?'), ('R009', 'What data does R009 read?')]:
    paths = []
    for sc, pre in SC.items():
        for k, f in glossary['scopes'][sc].items():
            if rule in f['used_by']:
                paths.append(f'{pre}{k}')
    add('rule_fields', q, sorted(paths))

# 5. a value in a policy
for s, p, attr, q in [('SVC-THERAPY', 'EDU-BASIC', 'max_unit_price', 'What is the highest unit price the policy {p} allows for {s}?'),
                      ('SVC-LAB', 'EDU-PLUS', 'max_quantity_per_line', 'How many units of {s} can be billed on one line under {p}?'),
                      ('SVC-IMAGE', 'EDU-BASIC', 'required_documents', 'What document must support a {s} line under {p}?'),
                      ('SVC-DENTAL', 'EDU-PLUS', 'max_unit_price', 'What is the price ceiling for {s} on policy {p}?'),
                      ('SVC-PHARM', 'EDU-BASIC', 'max_quantity_per_line', 'What is the quantity limit per line for {s} under {p}?'),
                      ('SVC-DENTAL', 'EDU-BASIC', 'required_documents', 'Which supporting document does policy {p} require for {s}?')]:
    v = policies[p][attr][s]
    add('policy_value', q.format(s=s, p=p), [str(v), p])
for p in sorted(policies):
    add('policy_value', f'How many days after the service can a claim be submitted under {p}?', [str(policies[p]['submission_window_days']), p])
add('policy_value', 'Does policy EDU-BASIC require an authorization for SVC-LAB?', ['EDU-BASIC', 'SVC-LAB', 'does not need'])

# 6. providers, comparison, severity, related rules
add('provider', 'Which policies allow provider EDU-PROV-02?', ['EDU-BASIC', 'EDU-PLUS', 'R005'])
add('provider', 'Is EDU-PROV-03 in the network of EDU-PLUS, and which rule checks it?', ['EDU-PLUS', 'R005'])
add('compare', 'How do policies EDU-BASIC and EDU-PLUS differ?', ['30', '60', 'submission_window_days'])
add('compare', 'What is different between the EDU-BASIC and EDU-PLUS policies?', ['30', '60'])
sev = lambda s: sorted(k for k, r in rules.items() if r['severity'] == s)
add('severity', 'Which rules have medium severity?', sev('medium'))
add('severity', 'List the rules with high severity.', sev('high'))
add('related', 'How are R008 and R009 related?', ['R008', 'R009'])
add('related', 'Which rules depend on R007?', ['R007', 'R012'])
add('related', 'Which other rules work together with R014?', ['R014', 'R002'])
add('list', 'List all the service codes in the catalogue.', sorted(services))

# 7. other languages (need a model to choose the query; the planner reads English identifiers and keywords only)
add('multilingual', "Quelles règles dépendent de l'autorisation préalable d'un service ?", ['R008', 'R009'], 'fr', True)
add('multilingual', 'Quelle règle vérifie que le prestataire fait partie du réseau ?', ['R005'], 'fr', True)
add('multilingual', "Quelle règle contrôle le délai de soumission d'un dossier ?", ['R014'], 'fr', True)
add('multilingual', 'Quelles règles s’appliquent à SVC-IMAGE avec la politique EDU-PLUS ?', sorted({'R011', 'R008', 'R009', 'R010', 'R013'}), 'fr', False)
add('multilingual', 'ما هي القاعدة التي تتحقق من أن مقدم الخدمة ضمن الشبكة؟', ['R005'], 'ar', True)
add('multilingual', 'أي قاعدة تتحقق من وجود المستند الداعم المطلوب؟', ['R010'], 'ar', True)
add('multilingual', 'أي القواعد تعتمد على التفويض المسبق للخدمة؟', ['R008', 'R009'], 'ar', True)
add('multilingual', 'ما هو الحد الأقصى لسعر الوحدة للخدمة SVC-THERAPY في السياسة EDU-BASIC؟', ['450', 'EDU-BASIC'], 'ar', False)

out = ROOT / 'experiments/kag_cag'
(out / 'questions.json').write_text(json.dumps(Q, ensure_ascii=False, indent=1), encoding='utf8')
(out / 'questions.jsonl').write_text('\n'.join(json.dumps({'id': x['id'], 'q': x['q']}, ensure_ascii=False) for x in Q) + '\n', encoding='utf8')
from collections import Counter
print(len(Q), dict(Counter(x['cat'] for x in Q)))
