"""`price:history`, in Python — the same WIT `components/price-history` exports.

One of four languages this interface is implemented in. It goes into the SAME
binder composition and is judged by the SAME unedited e2e; see
`../portfolio-value-go/README.md` for what that is meant to prove.

Built with `componentize-py`, which is the odd one out of the four: it does not use
`wit-bindgen` at all. It has its own generator, and it ships a CPython interpreter
plus this module's bytecode inside the artifact — so what runs in the composition is
an interpreter, not compiled code, and it still satisfies the same contract.

`result<T, E>` is return-or-raise here: the ok value is returned, and the error is
raised as `Err(PriceError_...)`.

Three rules, all of which the app's e2e checks:

  Carry forward, never interpolate. A market has no price on a day nobody traded
  and the last known price is still the price. The point says it was carried.

  Before the first quote there is no price. Samples are ABSENT, not zero — a chart
  that starts at zero and jumps shows a gain nobody made.

  Stale is returned and LABELLED, with its age. Refusing a four-month-old quote
  leaves a caller with nothing for every card that stopped trading; returning it
  silently puts a confident number on a dead listing.

Deliberately not `max(..., key=...)`: this has to agree with three other
implementations on ties, and `max` returns the FIRST maximum where the rule here is
the lowest source name.
"""

from componentize_py_types import Err
import wit_world
from wit_world.exports import history
from wit_world.exports.history import (
    Observed,
    Point,
    PriceError_MixedCurrency,
    PriceError_NotYetPriced,
    PriceError_ZeroStep,
    Quote,
    QuoteKind,
)


def _check_currency(quotes: list[Quote]) -> None:
    """Two currencies for one card are refused, never converted."""
    expected = None
    for q in quotes:
        if expected is None:
            expected = q.currency
        elif q.currency != expected:
            raise Err(PriceError_MixedCurrency((expected, q.currency)))


def _price_at(quotes: list[Quote], kind: QuoteKind, instant: int) -> Observed:
    matching = [q for q in quotes if q.kind == kind]
    _check_currency(matching)

    best = None
    for q in matching:
        if q.at > instant:
            continue
        # Ties on `at` go to the lower source name, so the answer does not depend
        # on the order two disagreeing sources were fetched in.
        if best is None or q.at > best.at or (q.at == best.at and q.source < best.source):
            best = q
    if best is None:
        raise Err(PriceError_NotYetPriced())

    return Observed(
        unit_minor=best.unit_minor,
        currency=best.currency,
        source=best.source,
        observed_at=best.at,
        age_seconds=instant - best.at,
        carried=best.at != instant,
    )


class History(wit_world.exports.History):
    def at(self, quotes: list[Quote], kind: QuoteKind, at: int) -> Observed:
        return _price_at(quotes, kind, at)

    def series(
        self, quotes: list[Quote], kind: QuoteKind, since: int, until: int, step: int
    ) -> list[Point]:
        if step == 0:
            raise Err(PriceError_ZeroStep())

        # `until` is always sampled, even when the step does not land on it.
        times = []
        t = since
        while True:
            times.append(t)
            if t > until:
                break
            nxt = t + step
            if nxt > until:
                break
            t = nxt
        if times[-1] != until:
            times.append(until)

        points = []
        for ts in times:
            try:
                obs = _price_at(quotes, kind, ts)
            except Err as e:
                if isinstance(e.value, PriceError_NotYetPriced):
                    continue  # absent, not zero
                raise
            points.append(Point(at=ts, unit_minor=obs.unit_minor, carried=obs.carried))
        return points
