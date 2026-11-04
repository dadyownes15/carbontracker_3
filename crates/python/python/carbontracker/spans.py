"""Spans: measure any part of your code, as ``with``, a decorator, or by hand.

    with tracker.span("eval") as span:
        span.mark("samples", 64)

    @tracker.span("eval")
    def evaluate(model): ...

A span nests under the innermost span running in this thread or task, else
under the open epoch.
"""

from __future__ import annotations

import functools
import inspect
from contextvars import ContextVar, Token
from typing import Dict, Optional

_running: ContextVar[Optional[RunningSpan]] = ContextVar("carbontracker_span", default=None)
"""The innermost span running in this thread or task."""


class Span:
    """A span not started yet. Each ``with``, decorated call or ``start()``
    starts a new one, so the same ``Span`` can be used again and again."""

    def __init__(self, tracker, name: str, parent: Optional[RunningSpan]):
        self._tracker = tracker
        self.name = name
        self._parent = parent

    def start(self) -> RunningSpan:
        """Start a span now; stop it yourself."""
        return self._tracker._start_span(self.name, self._parent)

    def __enter__(self) -> RunningSpan:
        running = self.start()
        running._token = _running.set(running)
        return running

    def __exit__(self, *_exception) -> None:
        # Stopped even on an exception, which is not swallowed.
        running = _running.get()
        _running.reset(running._token)
        running.stop()

    def __call__(self, function):
        """Use as a decorator: each call is a span of its own. Once tracking
        has stopped, the function runs untracked."""
        if inspect.iscoroutinefunction(function):

            @functools.wraps(function)
            async def spanned(*args, **kwargs):
                if not self._tracker._tracking:
                    return await function(*args, **kwargs)
                with self:
                    return await function(*args, **kwargs)

        else:

            @functools.wraps(function)
            def spanned(*args, **kwargs):
                if not self._tracker._tracking:
                    return function(*args, **kwargs)
                with self:
                    return function(*args, **kwargs)

        return spanned

    def __repr__(self) -> str:
        return f"Span(name={self.name!r})"


class RunningSpan:
    """An open span. Stop it, or let its ``with`` stop it."""

    def __init__(self, native, tracker, id: Optional[int], name: str):
        self._native = native
        self._tracker = tracker
        self.id = id
        self.name = name
        self._stopped = native is None
        self._token: Optional[Token] = None

    def mark(self, kind: str, amount: float = 1.0) -> None:
        """Report work done in this span, e.g. ``mark("samples", 64)``."""
        if not self._stopped:
            self._native.mark(self.id, kind, float(amount))

    def stop(self, work: Optional[Dict[str, float]] = None) -> None:
        """Stop the span, with the work it did. Stopping twice does nothing."""
        if self._stopped:
            return
        self._stopped = True
        pairs = [(kind, float(amount)) for kind, amount in (work or {}).items()]
        self._tracker._stop_span(self.id, pairs)

    def __repr__(self) -> str:
        return f"RunningSpan(id={self.id}, name={self.name!r})"


def untracked(tracker, name: str) -> RunningSpan:
    """A span that measures nothing: for a forked child, which has the
    tracker but not its threads."""
    return RunningSpan(None, tracker, None, name)


def innermost(tracker) -> Optional[RunningSpan]:
    """The innermost span ``tracker`` is running here, if any."""
    running = _running.get()
    return running if running is not None and running._tracker is tracker and not running._stopped else None
