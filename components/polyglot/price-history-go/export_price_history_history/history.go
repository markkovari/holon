// Rules: carry forward, NEVER interpolate (the point says it was carried); before the first quote samples are
// ABSENT, not zero; stale quotes are returned and LABELLED with their age.
package export_price_history_history

import (
	witTypes "go.bytecodealliance.org/pkg/wit/types"
	w "wit_component/price_history_history"
)

// checkCurrency refuses two currencies for one card: the rate on the day is not knowable here.
func checkCurrency(quotes []w.Quote) (w.PriceError, bool) {
	var expected string
	var have bool
	for _, q := range quotes {
		if !have {
			expected, have = q.Currency, true
		} else if q.Currency != expected {
			return w.MakePriceErrorMixedCurrency(
				witTypes.Tuple2[string, string]{F0: expected, F1: q.Currency}), true
		}
	}
	return w.PriceError{}, false
}

func at(quotes []w.Quote, kind w.QuoteKind, instant uint64) (w.Observed, w.PriceError, bool) {
	matching := make([]w.Quote, 0, len(quotes))
	for _, q := range quotes {
		if q.Kind == kind {
			matching = append(matching, q)
		}
	}
	if e, bad := checkCurrency(matching); bad {
		return w.Observed{}, e, true
	}

	var best *w.Quote
	for i := range matching {
		q := &matching[i]
		if q.At > instant {
			continue
		}
		switch {
		case best == nil, q.At > best.At:
			best = q
		// Two sources for the same instant: the lower name wins, so the same
		// inputs always give the same answer.
		case q.At == best.At && q.Source < best.Source:
			best = q
		}
	}
	if best == nil {
		return w.Observed{}, w.MakePriceErrorNotYetPriced(), true
	}
	return w.Observed{
		UnitMinor:  best.UnitMinor,
		Currency:   best.Currency,
		Source:     best.Source,
		ObservedAt: best.At,
		AgeSeconds: instant - best.At,
		Carried:    best.At != instant,
	}, w.PriceError{}, false
}

func At(quotes []w.Quote, kind w.QuoteKind, instant uint64) witTypes.Result[w.Observed, w.PriceError] {
	obs, err, bad := at(quotes, kind, instant)
	if bad {
		return witTypes.Err[w.Observed, w.PriceError](err)
	}
	return witTypes.Ok[w.Observed, w.PriceError](obs)
}

// Series DROPS samples before the first quote rather than zeroing them; `until` is always sampled.
func Series(quotes []w.Quote, kind w.QuoteKind, since, until, step uint64) witTypes.Result[[]w.Point, w.PriceError] {
	if step == 0 {
		return witTypes.Err[[]w.Point, w.PriceError](w.MakePriceErrorZeroStep())
	}

	var times []uint64
	t := since
	for {
		times = append(times, t)
		if t > until {
			break
		}
		next := t + step
		if next < t || next > until { // overflow, or past the end
			break
		}
		t = next
	}
	if times[len(times)-1] != until {
		times = append(times, until)
	}

	points := []w.Point{}
	for _, ts := range times {
		obs, err, bad := at(quotes, kind, ts)
		if bad {
			if err.Tag() == w.PriceErrorNotYetPriced {
				continue // absent, not zero
			}
			return witTypes.Err[[]w.Point, w.PriceError](err)
		}
		points = append(points, w.Point{At: ts, UnitMinor: obs.UnitMinor, Carried: obs.Carried})
	}
	return witTypes.Ok[[]w.Point, w.PriceError](points)
}
