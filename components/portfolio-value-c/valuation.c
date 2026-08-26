// `portfolio:value`, in C — the same WIT `components/portfolio-value` exports.
//
// One of four languages this interface is implemented in. It goes into the SAME
// binder composition and is judged by the SAME unedited e2e; see
// ../portfolio-value-go/README.md for what that is meant to prove.
//
// C is here for one number: whether the 19 WASI imports Go forces onto a
// pure-compute capability are a COMPONENT MODEL cost or a LANGUAGE cost. The WIT
// world imports nothing; if a C build's import section is empty, they are the
// language's.
//
// The canonical ABI's memory rule, which is the whole of the difficulty here:
// everything returned is handed to the runtime, which frees it after lifting via
// the generated `cabi_post_*`. So returned strings must be `malloc`'d — `string_set`
// (borrow) is wrong for a return value and `string_dup` (copy) is right. Arguments
// are borrowed for the duration of the call and must not be freed or kept.

#include "portfolio_value.h"
#include <stdlib.h>
#include <string.h>

typedef exports_portfolio_value_valuation_event_t event_t;
typedef exports_portfolio_value_valuation_quote_t quote_t;
typedef exports_portfolio_value_valuation_value_error_t err_t;
typedef portfolio_value_string_t str_t;

#define ERR_MIXED_CURRENCY EXPORTS_PORTFOLIO_VALUE_VALUATION_VALUE_ERROR_MIXED_CURRENCY
#define ERR_OVERSOLD_AT EXPORTS_PORTFOLIO_VALUE_VALUATION_VALUE_ERROR_OVERSOLD_AT
#define ERR_ZERO_QUANTITY EXPORTS_PORTFOLIO_VALUE_VALUATION_VALUE_ERROR_ZERO_QUANTITY
#define ERR_ZERO_STEP EXPORTS_PORTFOLIO_VALUE_VALUATION_VALUE_ERROR_ZERO_STEP
#define ERR_EMPTY EXPORTS_PORTFOLIO_VALUE_VALUATION_VALUE_ERROR_EMPTY
#define KIND_ACQUIRED EXPORTS_PORTFOLIO_VALUE_VALUATION_EVENT_KIND_ACQUIRED

static bool str_eq(const str_t *a, const str_t *b) {
    return a->len == b->len && memcmp(a->ptr, b->ptr, a->len) == 0;
}

/// Copy a borrowed argument string into memory the runtime will free.
static void str_own(str_t *out, const str_t *in) {
    portfolio_value_string_dup_n(out, (const char *)in->ptr, in->len);
}

typedef struct {
    uint32_t quantity;
    int64_t unit_minor;
} lot_t;

/// The open lots of one item, oldest first. FIFO consumes from the front.
typedef struct {
    const str_t *item_id;
    lot_t *lots;
    size_t len, cap, head; /// `head` is the front, so a consumed lot is a bump not a memmove
} holding_t;

static holding_t *find_holding(holding_t *hs, size_t n, const str_t *id) {
    // ponytail: linear scan. A deck is tens of items, not thousands; a hash map
    // here is more code than the whole file saves.
    for (size_t i = 0; i < n; i++)
        if (str_eq(hs[i].item_id, id)) return &hs[i];
    return NULL;
}

static bool push_lot(holding_t *h, uint32_t quantity, int64_t unit_minor) {
    if (h->len == h->cap) {
        size_t cap = h->cap ? h->cap * 2 : 4;
        lot_t *grown = realloc(h->lots, cap * sizeof(lot_t));
        if (!grown) return false;
        h->lots = grown;
        h->cap = cap;
    }
    h->lots[h->len++] = (lot_t){quantity, unit_minor};
    return true;
}

/// The numbers, without the currency string — so `series` can call this once per
/// sample without allocating and freeing a copy of the currency each time.
typedef struct {
    int64_t cost_basis, market_value, realised;
    uint32_t unquoted;
} totals_t;

static bool totals_at(
    const exports_portfolio_value_valuation_list_event_t *events,
    const exports_portfolio_value_valuation_list_quote_t *quotes,
    uint64_t at,
    totals_t *out,
    err_t *err) {

    if (events->len == 0) {
        // No events means no currency to report a zero IN, so there is no answer.
        err->tag = ERR_EMPTY;
        return false;
    }

    const str_t *currency = &events->ptr[0].currency;
    for (size_t i = 0; i < events->len; i++) {
        const event_t *e = &events->ptr[i];
        if (e->quantity == 0) {
            err->tag = ERR_ZERO_QUANTITY;
            str_own(&err->val.zero_quantity.f0, &e->item_id);
            err->val.zero_quantity.f1 = e->at;
            return false;
        }
        if (!str_eq(&e->currency, currency)) {
            err->tag = ERR_MIXED_CURRENCY;
            str_own(&err->val.mixed_currency.f0, currency);
            str_own(&err->val.mixed_currency.f1, &e->currency);
            return false;
        }
    }

    // Events are SORTED by `at`, never trusted in the order given: backfilling an
    // old purchase must not change the answer. Insertion sort over indices —
    // STABLE, which matters because two events in the same second must keep their
    // given order to agree with the other three implementations.
    size_t *order = malloc(events->len * sizeof(size_t));
    holding_t *holdings = calloc(events->len, sizeof(holding_t));
    if (!order || !holdings) {
        free(order);
        free(holdings);
        err->tag = ERR_EMPTY;
        return false;
    }
    for (size_t i = 0; i < events->len; i++) {
        size_t j = i;
        while (j > 0 && events->ptr[order[j - 1]].at > events->ptr[i].at) {
            order[j] = order[j - 1];
            j--;
        }
        order[j] = i;
    }

    size_t held_count = 0;
    int64_t realised = 0;
    bool failed = false;

    for (size_t k = 0; k < events->len && !failed; k++) {
        const event_t *e = &events->ptr[order[k]];
        if (e->at > at) continue;

        holding_t *h = find_holding(holdings, held_count, &e->item_id);
        if (!h) {
            h = &holdings[held_count++];
            h->item_id = &e->item_id;
        }

        if (e->kind == KIND_ACQUIRED) {
            if (!push_lot(h, e->quantity, e->unit_minor)) failed = true;
            continue;
        }

        uint32_t held = 0;
        for (size_t i = h->head; i < h->len; i++) held += h->lots[i].quantity;
        if (held < e->quantity) {
            // The event log is wrong. Guessing which event is the lie is a bigger
            // lie than refusing.
            err->tag = ERR_OVERSOLD_AT;
            str_own(&err->val.oversold_at.f0, &e->item_id);
            err->val.oversold_at.f1 = e->at;
            err->val.oversold_at.f2 = held;
            err->val.oversold_at.f3 = e->quantity;
            failed = true;
            break;
        }

        // FIFO: the OLDEST unsold lot goes first, at its own cost.
        uint32_t remaining = e->quantity;
        int64_t cost = 0;
        while (remaining > 0) {
            lot_t *front = &h->lots[h->head];
            if (front->quantity <= remaining) {
                cost += (int64_t)front->quantity * front->unit_minor;
                remaining -= front->quantity;
                h->head++;
            } else {
                cost += (int64_t)remaining * front->unit_minor;
                front->quantity -= remaining;
                remaining = 0;
            }
        }
        realised += (int64_t)e->quantity * e->unit_minor - cost;
    }

    if (!failed) {
        int64_t cost_basis = 0, market_value = 0;
        uint32_t unquoted = 0;
        for (size_t i = 0; i < held_count; i++) {
            holding_t *h = &holdings[i];
            uint32_t held = 0;
            int64_t lot_cost = 0;
            for (size_t j = h->head; j < h->len; j++) {
                held += h->lots[j].quantity;
                lot_cost += (int64_t)h->lots[j].quantity * h->lots[j].unit_minor;
            }
            if (held == 0) continue;
            cost_basis += lot_cost;

            // The latest quote at or before `at`, in this currency. `>=` so ties
            // resolve to the LAST one given, matching the other implementations.
            const quote_t *best = NULL;
            for (size_t j = 0; j < quotes->len; j++) {
                const quote_t *q = &quotes->ptr[j];
                if (!str_eq(&q->item_id, h->item_id) || q->at > at || !str_eq(&q->currency, currency))
                    continue;
                if (!best || q->at >= best->at) best = q;
            }
            if (best) {
                market_value += (int64_t)held * best->unit_minor;
            } else {
                // An unpriced holding is carried at COST and counted — valuing it
                // at zero makes the chart lie downward, dropping it lies upward.
                market_value += lot_cost;
                unquoted += held;
            }
        }
        out->cost_basis = cost_basis;
        out->market_value = market_value;
        out->realised = realised;
        out->unquoted = unquoted;
    }

    for (size_t i = 0; i < held_count; i++) free(holdings[i].lots);
    free(holdings);
    free(order);
    return !failed;
}

bool exports_portfolio_value_valuation_value_at(
    exports_portfolio_value_valuation_list_event_t *events,
    exports_portfolio_value_valuation_list_quote_t *quotes,
    uint64_t at,
    exports_portfolio_value_valuation_valuation_t *ret,
    err_t *err) {

    totals_t t;
    if (!totals_at(events, quotes, at, &t, err)) return false;

    ret->cost_basis_minor = t.cost_basis;
    ret->market_value_minor = t.market_value;
    ret->unrealised_minor = t.market_value - t.cost_basis;
    ret->realised_minor = t.realised;
    ret->unquoted = t.unquoted;
    str_own(&ret->currency, &events->ptr[0].currency);
    return true;
}

bool exports_portfolio_value_valuation_series(
    exports_portfolio_value_valuation_list_event_t *events,
    exports_portfolio_value_valuation_list_quote_t *quotes,
    uint64_t since, uint64_t until, uint64_t step,
    exports_portfolio_value_valuation_list_point_t *ret,
    err_t *err) {

    if (step == 0) {
        err->tag = ERR_ZERO_STEP;
        return false;
    }

    exports_portfolio_value_valuation_point_t *points = NULL;
    size_t len = 0, cap = 0;
    uint64_t t = since;

    for (;;) {
        totals_t v;
        if (!totals_at(events, quotes, t, &v, err)) {
            free(points);
            return false;
        }
        if (len == cap) {
            cap = cap ? cap * 2 : 16;
            exports_portfolio_value_valuation_point_t *grown =
                realloc(points, cap * sizeof(*points));
            if (!grown) {
                free(points);
                err->tag = ERR_EMPTY;
                return false;
            }
            points = grown;
        }
        points[len++] = (exports_portfolio_value_valuation_point_t){
            .at = t,
            .market_value_minor = v.market_value,
            .cost_basis_minor = v.cost_basis,
            .realised_minor = v.realised,
            .unquoted = v.unquoted,
        };
        if (t >= until) break;
        uint64_t next = t + step;
        if (next < t) next = UINT64_MAX; // saturating: a wrapping step never terminates
        if (next > until) next = until;
        t = next;
    }

    ret->ptr = points;
    ret->len = len;
    return true;
}
