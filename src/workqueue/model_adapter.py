"""The production model for the queue's AI step: a real language model behind the ExplainStep contract.

ExplainStep (explain.py) hands a model a *request* (rule id, failure shape, prompt version, the two placeholders) and expects back a
template with {value} and {line} where claim values go. This module turns that request into a prompt, calls a model, and returns the
template. What it guarantees, each pinned by tests/test_wq_model_adapter.py:

- No claim value ever leaves the process. The prompt is built from the rule's own text (trusted, from the rulebook) and the failure
  shape (status, severity, line count). The request carries no claim data, and the adapter reads none.
- Every call has a deadline: the time left on the claim's 90-second ceiling is the call's timeout, and a call that starts with none
  left is refused as a Timeout.
- Errors map onto the breaker's classes: timeouts, rate limits, connection failures and 5xx are Transient (they count toward opening
  the breaker and are retried); rejected requests and missing credentials are Fatal (they never count and are never retried).
- Two-pass mode (off by default). Pass 1 asks for the explanation as plain text, free of any output format. Pass 2 asks the model only to
  put that exact text into a small JSON object. The adapter accepts pass 2 only if its text equals pass 1 apart from whitespace, so the
  second pass can fix the format but cannot add or change a word. Whether two passes produce better explanations than one has NOT been
  measured; scripts/two_pass_experiment.py does that once a model is available (docs/32).

The model is injected as complete(prompt, timeout_seconds) -> text, so everything above runs offline in tests. from_provider() wraps
the repository's existing OpenAI-compatible providers (Featherless, Ollama, NVIDIA).
"""
import copy
import json
import re
import time

from . import breaker as brk
from .explain import MAX_TEMPLATE_CHARS, PLACEHOLDERS

PROMPT_VERSION = '2.0.0-template'
PASS1_INSTRUCTIONS = (
    'You help a human reviewer of a synthetic, educational health-insurance claims exercise. A deterministic rule engine has flagged a '
    'claim. Write a short explanation (two or three sentences) of why the rule below flagged it, in plain English, as a TEMPLATE.\n'
    'Rules for the template:\n'
    '- Where the claim\'s own value belongs, write {value}. Where the affected line id belongs, write {line}. Use no other braces.\n'
    '- You are not shown any claim data. Never invent a value, a date, an amount, an identifier or a name.\n'
    '- Do not say a claim is approved, accepted, paid or payable; do not judge medical necessity; do not suggest fraud.\n'
    '- End with one sentence telling the reviewer what to check or request, based on the rule\'s corrective action.\n'
    'Reply with the template text only.')
PASS2_INSTRUCTIONS = (
    'Put the text below, word for word, into one JSON object of the form {"template": "<the text>"}. Do not change, add or remove any '
    'word. Reply with the JSON object only.')
_ALLOWED_BRACES = re.compile(r'\{value\}|\{line\}')
_WS = re.compile(r'\s+')


def _classify(exc):
    """Map a provider exception onto the breaker's classes by name and status code, so openai need not be imported here."""
    name = type(exc).__name__
    status = getattr(exc, 'status_code', None)
    if name in ('APITimeoutError', 'Timeout', 'TimeoutError', 'ReadTimeout', 'ConnectTimeout') or isinstance(exc, TimeoutError):
        return brk.Timeout(str(exc))
    if name in ('RateLimitError', 'APIConnectionError', 'InternalServerError', 'TransientProviderError', 'ConnectionError') \
            or (isinstance(status, int) and (status == 429 or status >= 500)):
        return brk.Transient(str(exc))
    return brk.Fatal(f'{name}: {exc}')


class TemplateModel:
    def __init__(self, complete, rules, *, name='model', two_pass=False, clock=time.monotonic):
        """complete(prompt, timeout_seconds) -> text. rules is {rule_id: rule dict} from the rulebook (trusted text)."""
        self.complete, self.rules, self.two_pass, self.clock = complete, rules, two_pass, clock
        self.name = f'{name}+2pass' if two_pass else name
        self.prompt_version = PROMPT_VERSION + ('-2pass' if two_pass else '')
        self.calls = 0

    # ---- prompts: built only from the rulebook and the failure shape
    def pass1_prompt(self, request):
        rule = self.rules.get(request['rule_id'])
        if rule is None:
            raise brk.Fatal(f"unknown rule {request['rule_id']!r}")
        status, severity, lines = (request['failure_shape'].split('/') + ['', '', ''])[:3]
        excerpt = {k: rule[k] for k in ('rule_id', 'title', 'severity', 'logic', 'corrective_action') if k in rule}
        return (PASS1_INSTRUCTIONS + '\n\n## Rule (from the rulebook)\n' + json.dumps(excerpt, indent=2, ensure_ascii=False) +
                f'\n\n## Outcome\nstatus: {status}\nseverity: {severity}\naffected lines: {"none" if lines == "0" else "one" if lines == "1" else "several"}\n'
                f'\nAllowed placeholders: {", ".join(request.get("placeholders") or PLACEHOLDERS)}\n')

    @staticmethod
    def pass2_prompt(text):
        return PASS2_INSTRUCTIONS + '\n\n## Text\n' + text + '\n'

    # ---- the call
    def _timeout(self, deadline):
        left = deadline - self.clock()
        if left <= 0:
            raise brk.Timeout('no time left before the claim deadline')
        return left

    def _call(self, prompt, deadline):
        timeout = self._timeout(deadline)
        self.calls += 1
        try:
            out = self.complete(prompt, timeout)
        except brk.Transient:
            raise
        except brk.Fatal:
            raise
        except Exception as exc:  # noqa: BLE001 - provider libraries raise their own classes; map them
            raise _classify(exc) from exc
        if type(out) is not str or not out.strip():
            raise brk.Transient('the model returned no text')
        return out.strip()

    @staticmethod
    def _check_template(text):
        if len(text) > MAX_TEMPLATE_CHARS:
            raise brk.Transient('the template is too long')
        rest = _ALLOWED_BRACES.sub('', text)
        if '{' in rest or '}' in rest:
            raise brk.Transient('the template uses braces other than {value} and {line}')
        return text

    def __call__(self, request, deadline):
        first = self._check_template(self._call(self.pass1_prompt(request), deadline))
        if not self.two_pass:
            return first
        raw = self._call(self.pass2_prompt(first), deadline)
        try:
            obj = json.loads(raw[raw.index('{'):raw.rindex('}') + 1])
        except ValueError as exc:
            raise brk.Transient('pass 2 did not return JSON') from exc
        if not isinstance(obj, dict) or set(obj) != {'template'} or type(obj['template']) is not str:
            raise brk.Transient('pass 2 JSON has the wrong shape')
        second = obj['template']
        if _WS.sub(' ', second).strip() != _WS.sub(' ', first).strip():
            raise brk.Transient('pass 2 changed the text')
        return self._check_template(first)


def from_provider(provider, rules, *, two_pass=False, name=None, clock=time.monotonic):
    """Wrap an OpenAICompatibleProvider (llm_adapter) as a TemplateModel. The provider's own client does the HTTP call."""
    def complete(prompt, timeout):
        scoped = copy.copy(provider)  # a copy, so concurrent workers each get their own timeout without touching shared state
        if hasattr(provider.client, 'with_options'):
            scoped.client = provider.client.with_options(timeout=timeout)
        return scoped._complete(prompt)
    return TemplateModel(complete, rules, name=name or getattr(provider, 'model', 'model'), two_pass=two_pass, clock=clock)


def model_from_env(env, rules):
    """The model the queue should use, chosen by environment variables; None keeps the default (no model: every claim keeps the
    engine's own explanation). QUEUE_AI_PROVIDER = featherless | ollama | nvidia turns it on and needs that provider's own key or
    endpoint settings (llm_adapter). QUEUE_AI_TWO_PASS=1 enables the two-pass mode. The wiring is tested; a live model has not been run."""
    which = (env.get('QUEUE_AI_PROVIDER') or '').strip().lower()
    if not which:
        return None
    import llm_adapter
    classes = {'featherless': llm_adapter.FeatherlessExplanationProvider, 'ollama': llm_adapter.OllamaExplanationProvider,
               'nvidia': llm_adapter.NvidiaExplanationProvider}
    if which not in classes:
        raise ValueError(f'QUEUE_AI_PROVIDER must be one of {sorted(classes)}, not {which!r}')
    kwargs = {}
    base = (env.get('OLLAMA_BASE_URL') or '').strip().rstrip('/')
    if which == 'ollama' and base:
        kwargs['base_url'] = base if base.endswith('/v1') else base + '/v1'      # e.g. http://ollama:11434 in the compose stack
    return from_provider(classes[which](**kwargs), rules() if callable(rules) else rules, two_pass=env.get('QUEUE_AI_TWO_PASS') == '1')
