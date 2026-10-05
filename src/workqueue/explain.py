"""The AI explanation step. It can only ever add words; it can never hold a claim up.

For each flagged finding the step asks a model for an explanation *template*: text with the placeholders {value} and {line} in
place of anything from the claim. The model never sees a claim value, which is also what makes the answer cacheable: the template is
keyed by (rule, failure shape, prompt version, model), claim values are filled in afterwards, and the existing grounding guard runs
on the *filled* text every time, cached or not. Only guard-approved text is cached.

Whatever goes wrong (budget spent, rate limit, breaker open, 90-second ceiling passed, model error, guard refusal) the claim keeps the
engine's own deterministic explanation, moves on to `explanation_skipped`, and the next step carries on. The step does not raise
for any model behaviour.
"""
import hashlib
import re
import time

from . import breaker as brk
from . import triage

OUTCOMES = ('ai_used', 'cache_hit', 'skipped_budget', 'skipped_rate', 'skipped_breaker', 'skipped_timeout', 'skipped_guard',
            'skipped_error', 'skipped_none')
PLACEHOLDERS = ('{value}', '{line}')
MAX_TEMPLATE_CHARS = 1500
MAX_VALUE_CHARS = 200
_PLACEHOLDER = re.compile(r'\{value\}|\{line\}')


def backoff(attempt, base=1.0, cap=30.0, *, rng):
    """Full jitter: a random wait between zero and the capped exponential."""
    return rng.uniform(0, min(cap, base * 2 ** attempt))


def failure_shape(result):
    lines = len(result.get('affected_line_ids') or [])
    return f"{result.get('status')}/{result.get('severity')}/{'0' if lines == 0 else '1' if lines == 1 else 'n'}"


def template_key(rule_id, shape, prompt_version, model_name):
    return hashlib.sha256('|'.join([str(rule_id), shape, prompt_version, model_name]).encode('utf-8')).hexdigest()


def fill(template, result):
    """Put this finding's own values into a template. Plain replacement, never format(): a value may contain braces."""
    evidence = result.get('evidence') or []
    value = str(evidence[0].get('value')) if evidence and isinstance(evidence[0], dict) else ''
    lines = result.get('affected_line_ids') or []
    swap = {'{value}': value[:MAX_VALUE_CHARS], '{line}': str(lines[0]) if lines else ''}
    return _PLACEHOLDER.sub(lambda m: swap[m.group(0)], template)       # one pass: a value is never searched for placeholders


class ExplainStep:
    def __init__(self, store, model, guard, breaker, clock, rng, cfg_source, templates, *, sleep=time.sleep, max_attempts=3,
                 prompt_version='1.6.0', model_name='model', deadline_seconds=90.0):
        """model(request, deadline) -> template text; may raise breaker.Transient / Timeout / Fatal.
        guard(filled_text, result) -> True when the text is allowed. templates(result) -> the deterministic explanation.
        cfg_source() -> RoutingConfig. rng is a random.Random (backoff jitter)."""
        self.store, self.model, self.guard, self.breaker, self.clock, self.rng = store, model, guard, breaker, clock, rng
        self.cfg_source, self.templates, self.sleep, self.max_attempts = cfg_source, templates, sleep, max_attempts
        self.prompt_version, self.model_name, self.deadline_seconds = prompt_version, model_name, deadline_seconds

    # ---- one finding
    def _limits_ok(self, cfg, now):
        if not self.store.bump('ai_day', str(int(now // 86400)), cfg.ai_daily_budget):
            return 'skipped_budget'
        if not self.store.bump('ai_minute', str(int(now // 60)), cfg.ai_per_minute):
            return 'skipped_rate'
        return None

    def _ask(self, result, deadline):
        """Returns (template_text, None) or (None, skip_reason)."""
        request = {'rule_id': result['rule_id'], 'failure_shape': failure_shape(result), 'prompt_version': self.prompt_version,
                   'placeholders': list(PLACEHOLDERS)}
        for attempt in range(self.max_attempts):
            if self.clock() >= deadline:
                return None, 'skipped_timeout'
            try:
                text = self.breaker.call(lambda: self.model(request, deadline))
            except brk.Open:
                return None, 'skipped_breaker'
            except brk.Fatal:
                return None, 'skipped_error'
            except brk.Transient as e:
                if attempt + 1 >= self.max_attempts:
                    return None, 'skipped_timeout' if isinstance(e, brk.Timeout) else 'skipped_error'
                wait = backoff(attempt, rng=self.rng)
                self.sleep(max(0.0, min(wait, deadline - self.clock())))
                continue
            except Exception:  # noqa: BLE001 - an unexpected model-side failure must not stop the claim
                return None, 'skipped_error'
            if self.clock() > deadline:
                return None, 'skipped_timeout'
            if type(text) is not str or not text.strip() or len(text) > MAX_TEMPLATE_CHARS:
                return None, 'skipped_error'
            return text.strip(), None
        return None, 'skipped_error'

    def _one(self, result, cfg, deadline, stopped):
        """-> (text, source, outcome, stop) where stop is a reason that applies to the rest of the claim."""
        fallback = self.templates(result)
        key = template_key(result['rule_id'], failure_shape(result), self.prompt_version, self.model_name)
        cached = self.store.cache_get(key)
        if cached is not None:
            filled = fill(cached, result)
            if self.guard(filled, result):
                return filled, 'cache', 'cache_hit', stopped
            return fallback, 'template', 'skipped_guard', stopped
        if stopped:
            return fallback, 'template', stopped, stopped
        limited = self._limits_ok(cfg, self.clock())
        if limited:
            return fallback, 'template', limited, limited
        template, why = self._ask(result, deadline)
        if template is None:
            stop = why if why in ('skipped_breaker', 'skipped_timeout') else None
            return fallback, 'template', why, stop
        filled = fill(template, result)
        if not self.guard(filled, result):
            return fallback, 'template', 'skipped_guard', stopped
        self.store.cache_put(key, template)
        return filled, 'ai', 'ai_used', stopped

    # ---- one claim
    def run(self, claim_id, version):
        doc = self.store.get(claim_id, version)
        if doc is None or doc['state'] != 'triaged' or doc['receipt'].get('lane') == 'green':
            return 'noop'
        cfg = self.cfg_source()
        deadline = self.clock() + self.deadline_seconds
        rows = triage.flagged(doc['results'])
        findings, outcomes, stopped = {}, [], None
        for result in rows:
            if not isinstance(result.get('rule_id'), str):
                continue
            text, source, outcome, stopped = self._one(result, cfg, deadline, stopped)
            findings[result['rule_id']] = {'text': text, 'source': source}
            outcomes.append(outcome)
        skipped = [o for o in outcomes if o.startswith('skipped')]
        if not findings:
            final, to = 'skipped_none', 'explanation_skipped'
        elif skipped:
            final, to = skipped[0], 'explanation_skipped'
        else:
            final, to = ('ai_used' if 'ai_used' in outcomes else 'cache_hit'), 'explained'
        moved = self.store.transition(claim_id, version, 'triaged', to, 'system:explain', self.clock(),
                                      detail={'outcome': final, 'findings': len(findings), 'skipped': len(skipped)},
                                      set_fields={'explanation': {'outcome': final, 'findings': findings}})
        return to if moved is not None else 'noop'
