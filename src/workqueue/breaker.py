"""A circuit breaker for the AI call, so a failing model never slows the queue down.

closed    calls go through. The breaker opens after `consecutive` transient failures in a row, or when failures / calls over the last
          `window` seconds reaches `error_rate` with at least `min_calls` calls in that window.
open      calls are refused at once (Open) without running the function, for `cooldown` seconds.
half_open after the cooldown exactly one probe call is let through; its success closes the breaker, its failure reopens it for a
          new cooldown. A second caller during the probe gets Open.

Only Transient failures (timeouts, 5xx, rate limits) count. Fatal ones (a rejected request, 400 to 404) are the caller's bug, not the
model being down: they pass through without counting and are never retried.
"""
import threading
from collections import deque

CLOSED, OPEN, HALF_OPEN = 'closed', 'open', 'half_open'


class Open(Exception):
    """The breaker is open: the call was not made."""


class Transient(Exception):
    """A failure that may go away on its own and counts toward opening the breaker."""


class Timeout(Transient):
    """The model did not answer in time."""


class Fatal(Exception):
    """A failure that retrying cannot fix; it never counts toward opening."""


class NotConfigured(Fatal):
    """No model has been set up. Claims keep the engine's own explanation; nothing is wrong, nothing is retried."""


class CircuitBreaker:
    def __init__(self, clock, consecutive=5, error_rate=0.5, window=60.0, min_calls=20, cooldown=20.0):
        self._clock = clock
        self.consecutive, self.error_rate, self.window = consecutive, error_rate, window
        self.min_calls, self.cooldown = min_calls, cooldown
        self._lock = threading.Lock()
        self._state = CLOSED
        self._opened_at = 0.0
        self._streak = 0
        self._calls = deque()          # (time, ok)
        self._probing = False

    @property
    def state(self):
        with self._lock:
            if self._state == OPEN and self._clock() - self._opened_at >= self.cooldown:
                return HALF_OPEN
            return self._state

    def _prune(self, now):
        while self._calls and now - self._calls[0][0] > self.window:
            self._calls.popleft()

    def _open(self, now):
        self._state, self._opened_at, self._probing = OPEN, now, False

    def call(self, fn):
        with self._lock:
            now = self._clock()
            if self._state == OPEN:
                if now - self._opened_at < self.cooldown:
                    raise Open('the circuit is open')
                self._state, self._probing = HALF_OPEN, False
            probe = False
            if self._state == HALF_OPEN:
                if self._probing:
                    raise Open('a probe call is already running')
                self._probing = probe = True
        try:
            result = fn()
        except Transient:
            self._record(False, probe)
            raise
        except BaseException:
            if probe:
                with self._lock:
                    self._probing = False          # neither success nor failure: let the next caller probe again
            raise
        self._record(True, probe)
        return result

    def _record(self, ok, probe):
        with self._lock:
            now = self._clock()
            if probe:
                self._probing = False
                if ok:
                    self._state, self._streak = CLOSED, 0
                    self._calls.clear()
                else:
                    self._open(now)
                return
            if self._state != CLOSED:
                return                             # a straggler that finished after the breaker moved on
            self._calls.append((now, ok))
            self._prune(now)
            self._streak = 0 if ok else self._streak + 1
            if ok:
                return
            failures = sum(1 for _, good in self._calls if not good)
            if self._streak >= self.consecutive or (len(self._calls) >= self.min_calls and failures / len(self._calls) >= self.error_rate):
                self._open(now)
