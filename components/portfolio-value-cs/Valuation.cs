// `portfolio:value`, in C# — the same WIT `components/portfolio-value` exports.
//
// One of four languages this interface is implemented in. It goes into the SAME
// binder composition and is judged by the SAME unedited e2e; see
// ../portfolio-value-go/README.md for what that is meant to prove.
//
// `result<T, E>` is return-or-throw here: the ok value is returned, and the error
// is thrown as `WitException<ValueError>(e, 0)` — the generated interop catches it
// and lowers it into the error arm. The nesting level is 0 because this result is
// not inside another one.
//
// The rules, all of which the app's e2e checks:
//
//   FIFO. A sale consumes the OLDEST unsold lot, at that lot's own cost. Average
//   cost is easier and answers a different question than the one a collector asks.
//
//   Events are SORTED by `at`, never trusted in the order given, so backfilling an
//   old purchase does not change the answer. STABLE, so two events in the same
//   second keep their given order — `List.Sort` is NOT stable, which is why this
//   uses OrderBy: LINQ's is, and disagreeing with the other three implementations
//   on exactly the ties that matter is the failure mode.
//
//   An unpriced holding is carried at COST and counted, not valued at zero and not
//   dropped. Either of those puts a confident wrong number on the chart.
//
//   Money never converts. Two currencies are an error naming both.

using System.Collections.Generic;
using System.Linq;
using PortfolioValueWorld.wit;

namespace PortfolioValueWorld.wit.Exports.portfolio.value;

public class ValuationExportsImpl : IValuationExports {

    /// One open lot: what is left of a purchase, and what that purchase cost.
    private sealed class Lot {
        public uint Quantity;
        public long UnitMinor;
        public Lot(uint quantity, long unitMinor) { Quantity = quantity; UnitMinor = unitMinor; }
    }

    private readonly record struct Totals(long CostBasis, long MarketValue, long Realised, uint Unquoted);

    private static WitException<IValuationExports.ValueError> Fail(IValuationExports.ValueError e) =>
        new WitException<IValuationExports.ValueError>(e, 0);

    /// The numbers, without the currency — so `Series` can sample without
    /// re-deriving the string every time.
    private static Totals TotalsAt(
        List<IValuationExports.Event> events,
        List<IValuationExports.Quote> quotes,
        ulong at) {

        if (events.Count == 0)
            // No events means no currency to report a zero IN, so there is no answer.
            throw Fail(IValuationExports.ValueError.Empty());

        string currency = events[0].currency;
        foreach (var e in events) {
            if (e.quantity == 0)
                throw Fail(IValuationExports.ValueError.ZeroQuantity((e.itemId, e.at)));
            if (e.currency != currency)
                throw Fail(IValuationExports.ValueError.MixedCurrency((currency, e.currency)));
        }

        // Insertion order for the walk below, so the answer never depends on a
        // dictionary's iteration order.
        var order = new List<string>();
        var lots = new Dictionary<string, List<Lot>>();
        var heads = new Dictionary<string, int>();

        long realised = 0;
        foreach (var e in events.OrderBy(e => e.at)) {   // OrderBy is stable; List.Sort is not
            if (e.at > at) continue;
            if (!lots.ContainsKey(e.itemId)) {
                lots[e.itemId] = new List<Lot>();
                heads[e.itemId] = 0;
                order.Add(e.itemId);
            }
            var dq = lots[e.itemId];

            if (e.kind == IValuationExports.EventKind.ACQUIRED) {
                dq.Add(new Lot(e.quantity, e.unitMinor));
                continue;
            }

            uint held = 0;
            for (int i = heads[e.itemId]; i < dq.Count; i++) held += dq[i].Quantity;
            if (held < e.quantity)
                // The event log is wrong, and guessing which event is the lie is a
                // bigger lie than refusing.
                throw Fail(IValuationExports.ValueError.OversoldAt((e.itemId, e.at, held, e.quantity)));

            uint remaining = e.quantity;
            long cost = 0;
            while (remaining > 0) {
                var front = dq[heads[e.itemId]];
                if (front.Quantity <= remaining) {
                    cost += (long)front.Quantity * front.UnitMinor;
                    remaining -= front.Quantity;
                    heads[e.itemId]++;
                } else {
                    cost += (long)remaining * front.UnitMinor;
                    front.Quantity -= remaining;
                    remaining = 0;
                }
            }
            realised += (long)e.quantity * e.unitMinor - cost;
        }

        long costBasis = 0, marketValue = 0;
        uint unquoted = 0;
        foreach (var itemId in order) {
            var dq = lots[itemId];
            uint held = 0;
            long lotCost = 0;
            for (int i = heads[itemId]; i < dq.Count; i++) {
                held += dq[i].Quantity;
                lotCost += (long)dq[i].Quantity * dq[i].UnitMinor;
            }
            if (held == 0) continue;
            costBasis += lotCost;

            // The latest quote at or before `at`, in this currency. `>=` so ties
            // resolve to the LAST one given, matching the other implementations.
            IValuationExports.Quote? best = null;
            foreach (var q in quotes) {
                if (q.itemId != itemId || q.at > at || q.currency != currency) continue;
                if (best is null || q.at >= best.Value.at) best = q;
            }
            if (best is not null) {
                marketValue += (long)held * best.Value.unitMinor;
            } else {
                // Carried at COST and counted: zero makes the chart lie downward,
                // dropping it lies upward.
                marketValue += lotCost;
                unquoted += held;
            }
        }

        return new Totals(costBasis, marketValue, realised, unquoted);
    }

    public static IValuationExports.Valuation ValueAt(
        List<IValuationExports.Event> events,
        List<IValuationExports.Quote> quotes,
        ulong at) {

        var t = TotalsAt(events, quotes, at);
        return new IValuationExports.Valuation(
            t.CostBasis, t.MarketValue, t.MarketValue - t.CostBasis, t.Realised,
            events[0].currency, t.Unquoted);
    }

    public static List<IValuationExports.Point> Series(
        List<IValuationExports.Event> events,
        List<IValuationExports.Quote> quotes,
        ulong since, ulong until, ulong step) {

        if (step == 0) throw Fail(IValuationExports.ValueError.ZeroStep());

        var points = new List<IValuationExports.Point>();
        ulong t = since;
        for (;;) {
            var v = TotalsAt(events, quotes, t);
            points.Add(new IValuationExports.Point(t, v.MarketValue, v.CostBasis, v.Realised, v.Unquoted));
            if (t >= until) break;
            ulong next = t + step;
            if (next < t) next = ulong.MaxValue;   // saturating: a wrapping step never terminates
            if (next > until) next = until;
            t = next;
        }
        return points;
    }
}
