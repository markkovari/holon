// `portfolio:value`, in Go — the same WIT `components/portfolio-value` exports.
//
// This is the binder app's hardest piece of arithmetic (FIFO cost basis, realised
// and unrealised gain, the series a chart is drawn from) written a second time in a
// language nothing else in this repository uses, and dropped into the SAME
// composition. `examples/binder/tests` is the judge: it asserts the money as
// arithmetic — buy 2 @ 10.00, buy 1 @ 40.00, sell 1 @ 30.00 realises 20.00 under
// FIFO and 10.00 under average cost — so it fails for a plausible wrong answer and
// not only for a broken one.
//
// Deliberately a re-derivation, not a translation: the point is that two
// independent implementations agree at the boundary, which is a claim a
// transliteration cannot make.
//
// The rules it has to honour, all of which the specification checks:
//
//   - FIFO. A sale consumes the OLDEST unsold lot, at that lot's own cost. Average
//     cost is easier and answers a different question than the one a collector asks.
//   - Events are SORTED by `at`, never trusted in the order given, so backfilling
//     an old purchase does not change the answer. Stable, so two events in the same
//     second keep their given order — Rust's `sort_by_key` is stable too, and an
//     unstable sort here would disagree with it exactly on the ties that matter.
//   - An unpriced holding is carried at COST and counted, not valued at zero and
//     not dropped. Either of those puts a confident wrong number on the chart.
//   - Money never converts. Two currencies are an error naming both.
package export_portfolio_value_valuation

import (
	"sort"

	witTypes "go.bytecodealliance.org/pkg/wit/types"
	w "wit_component/portfolio_value_valuation"
)

type lot struct {
	quantity  uint32
	unitMinor int64
}

func errValuation(e w.ValueError) witTypes.Result[w.Valuation, w.ValueError] {
	return witTypes.Err[w.Valuation, w.ValueError](e)
}

// ValueAt is what the collection is worth at `at`, over `events` priced by `quotes`.
//
// Events after `at` are ignored, which is also how the history is walked.
func ValueAt(events []w.Event, quotes []w.Quote, at uint64) witTypes.Result[w.Valuation, w.ValueError] {
	if len(events) == 0 {
		// No events means no currency to report a zero IN, so there is no answer.
		return errValuation(w.MakeValueErrorEmpty())
	}

	currency := events[0].Currency
	for _, e := range events {
		if e.Quantity == 0 {
			return errValuation(w.MakeValueErrorZeroQuantity(
				witTypes.Tuple2[string, uint64]{F0: e.ItemId, F1: e.At}))
		}
		if e.Currency != currency {
			return errValuation(w.MakeValueErrorMixedCurrency(
				witTypes.Tuple2[string, string]{F0: currency, F1: e.Currency}))
		}
	}

	sorted := make([]w.Event, len(events))
	copy(sorted, events)
	sort.SliceStable(sorted, func(i, j int) bool { return sorted[i].At < sorted[j].At })

	lots := map[string][]lot{}
	// Insertion order, so the walk below is deterministic. A Go map's range order
	// is randomised on purpose; the sums are order-independent but `unquoted` and
	// any future short-circuit are not, and relying on that is how a test starts
	// passing four times in five.
	var ids []string
	seen := map[string]bool{}
	touch := func(id string) {
		if !seen[id] {
			seen[id] = true
			ids = append(ids, id)
		}
	}

	var realised int64
	for _, e := range sorted {
		if e.At > at {
			continue
		}
		touch(e.ItemId)
		switch e.Kind {
		case w.EventKindAcquired:
			lots[e.ItemId] = append(lots[e.ItemId], lot{e.Quantity, e.UnitMinor})
		case w.EventKindDisposed:
			var held uint32
			for _, l := range lots[e.ItemId] {
				held += l.quantity
			}
			if held < e.Quantity {
				// The event log is wrong. Guessing which event is the lie is a
				// bigger lie than refusing.
				return errValuation(w.MakeValueErrorOversoldAt(
					witTypes.Tuple4[string, uint64, uint32, uint32]{
						F0: e.ItemId, F1: e.At, F2: held, F3: e.Quantity}))
			}
			remaining := e.Quantity
			var cost int64
			dq := lots[e.ItemId]
			for remaining > 0 {
				if dq[0].quantity <= remaining {
					cost += int64(dq[0].quantity) * dq[0].unitMinor
					remaining -= dq[0].quantity
					dq = dq[1:]
				} else {
					cost += int64(remaining) * dq[0].unitMinor
					dq[0].quantity -= remaining
					remaining = 0
				}
			}
			lots[e.ItemId] = dq
			realised += int64(e.Quantity)*e.UnitMinor - cost
		}
	}

	var costBasis, marketValue int64
	var unquoted uint32
	for _, id := range ids {
		var held uint32
		var lotCost int64
		for _, l := range lots[id] {
			held += l.quantity
			lotCost += int64(l.quantity) * l.unitMinor
		}
		if held == 0 {
			continue
		}
		costBasis += lotCost

		// The latest quote at or before `at`, in the portfolio's currency. `>=`
		// rather than `>` so ties resolve to the last one given, which is what
		// Rust's `max_by_key` does.
		var best *w.Quote
		for i := range quotes {
			q := &quotes[i]
			if q.ItemId != id || q.At > at || q.Currency != currency {
				continue
			}
			if best == nil || q.At >= best.At {
				best = q
			}
		}
		if best != nil {
			marketValue += int64(held) * best.UnitMinor
		} else {
			marketValue += lotCost
			unquoted += held
		}
	}

	return witTypes.Ok[w.Valuation, w.ValueError](w.Valuation{
		CostBasisMinor:   costBasis,
		MarketValueMinor: marketValue,
		UnrealisedMinor:  marketValue - costBasis,
		RealisedMinor:    realised,
		Currency:         currency,
		Unquoted:         unquoted,
	})
}

// Series is ValueAt sampled every `step` seconds over `since..=until`, for a chart.
func Series(events []w.Event, quotes []w.Quote, since, until, step uint64) witTypes.Result[[]w.Point, w.ValueError] {
	if step == 0 {
		return witTypes.Err[[]w.Point, w.ValueError](w.MakeValueErrorZeroStep())
	}

	points := []w.Point{}
	t := since
	for {
		r := ValueAt(events, quotes, t)
		if r.IsErr() {
			return witTypes.Err[[]w.Point, w.ValueError](r.Err())
		}
		v := r.Ok()
		points = append(points, w.Point{
			At:               t,
			MarketValueMinor: v.MarketValueMinor,
			CostBasisMinor:   v.CostBasisMinor,
			RealisedMinor:    v.RealisedMinor,
			Unquoted:         v.Unquoted,
		})
		if t >= until {
			break
		}
		next := t + step
		if next < t { // saturating: a step that wraps u64 would loop forever
			next = ^uint64(0)
		}
		if next > until {
			next = until
		}
		t = next
	}
	return witTypes.Ok[[]w.Point, w.ValueError](points)
}
