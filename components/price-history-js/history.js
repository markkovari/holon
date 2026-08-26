// `price:history`, in JavaScript — the same WIT `components/price-history` exports.
//
// One of four languages this interface is implemented in. It goes into the SAME
// binder composition and is judged by the SAME unedited e2e; see
// ../portfolio-value-go/README.md for what that is meant to prove.
//
// Two things about the JS bindings that are easy to get wrong and silently:
//
//   u64 is a BigInt, not a Number. `at`, `since`, `until`, `step`, `observedAt`
//   and `ageSeconds` all cross as BigInt, and mixing one into arithmetic with a
//   Number throws rather than coercing. That is a mercy — a u64 timestamp rounded
//   through a double is a bug that shows up years later.
//
//   `result<T, E>` is return-or-throw. The ok value is returned; the error is
//   THROWN as the variant's own shape, `{ tag, val }`.
//
// Deliberately not a one-liner over `Array.prototype.findLast`: this has to agree
// with three other implementations on ties, on carry-forward and on what happens
// before the first quote, and a standard library that quietly differs on any of
// them would prove less, not more.

const notYetPriced = () => ({ tag: "not-yet-priced" });
const mixedCurrency = (expected, found) => ({ tag: "mixed-currency", val: [expected, found] });
const zeroStep = () => ({ tag: "zero-step" });

/** Two currencies for one card are refused, never converted. */
function checkCurrency(quotes) {
  let expected;
  for (const q of quotes) {
    if (expected === undefined) expected = q.currency;
    else if (q.currency !== expected) throw mixedCurrency(expected, q.currency);
  }
}

/**
 * The latest quote of `kind` at or before `instant`, carried forward if older.
 *
 * Ties on `at` go to the lower source name, so the same inputs always give the
 * same answer whichever two sources disagreed.
 */
function priceAt(quotes, kind, instant) {
  const matching = quotes.filter((q) => q.kind === kind);
  checkCurrency(matching);

  let best;
  for (const q of matching) {
    if (q.at > instant) continue;
    if (best === undefined || q.at > best.at || (q.at === best.at && q.source < best.source)) best = q;
  }
  if (best === undefined) throw notYetPriced();

  return {
    unitMinor: best.unitMinor,
    currency: best.currency,
    source: best.source,
    observedAt: best.at,
    ageSeconds: instant - best.at,
    carried: best.at !== instant,
  };
}

export const history = {
  at: priceAt,

  series(quotes, kind, since, until, step) {
    if (step === 0n) throw zeroStep();

    // `until` is always sampled, even when the step does not land on it.
    const times = [];
    let t = since;
    for (;;) {
      times.push(t);
      if (t > until) break;
      const next = t + step;
      if (next > until) break;
      t = next;
    }
    if (times[times.length - 1] !== until) times.push(until);

    const points = [];
    for (const ts of times) {
      let obs;
      try {
        obs = priceAt(quotes, kind, ts);
      } catch (e) {
        // Before the first quote there is no price. ABSENT, not zero — a chart
        // that starts at zero and jumps shows a gain nobody made.
        if (e && e.tag === "not-yet-priced") continue;
        throw e;
      }
      points.push({ at: ts, unitMinor: obs.unitMinor, carried: obs.carried });
    }
    return points;
  },
};
