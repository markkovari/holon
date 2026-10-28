// u64 crosses as BigInt: mixing it with a Number throws, which beats a timestamp rounded through a double.
// `result<T, E>` is return-or-throw (`{ tag, val }`). Not `findLast`: ties and carry-forward must match the other three.

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

// Ties on `at` go to the lower source name, so the same inputs always give the same answer.
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
        // ABSENT, not zero: a chart that starts at zero and jumps shows a gain nobody made.
        if (e && e.tag === "not-yet-priced") continue;
        throw e;
      }
      points.push({ at: ts, unitMinor: obs.unitMinor, carried: obs.carried });
    }
    return points;
  },
};
