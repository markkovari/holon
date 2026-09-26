//! `marketplace-domain` — a 5-part DECOMPOSED goal (ADR-0086). See
//! `CONTRACT.md` for what the parts must agree on.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: real auth (register/
//! login/RBAC via `guestauth`, three roles this time — `vendor`/`buyer`/
//! `admin` — rather than the usual two), dispatch to `catalog`, `fulfillment`,
//! `trust`, `ledger` and `messaging`, `/health`, and a `/test/seed` fixture
//! plus raw-store read routes so any one part can be judged before the other
//! four exist. `src/catalog.rs`, `src/fulfillment.rs`, `src/trust.rs`,
//! `src/ledger.rs` and `src/messaging.rs` are the goal. `CONTRACT.md` is what
//! they must agree on.
//!
//! Unlike `dispatch-domain` (the other decomposed goal in this repo), this
//! one has real auth AND real shared mutable state across parts that isn't
//! just one collection — five collections plus five `fsm:workflow` machines,
//! two of which (`order` and `shipment`) are fired by TWO different parts in
//! the same request. Nothing here lets a part call another part's Rust
//! function; they meet only in `records:store` and `fsm:workflow` instances.

#[allow(warnings)]
mod bindings;
mod catalog;
mod fulfillment;
mod ledger;
mod messaging;
mod trust;

pub use guestauth::Route;

pub const TENANT: &str = "marketplace";
/// The only roles this app grants. Anything else in a register request falls
/// back to `buyer` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["vendor", "buyer", "admin"];

guestio::guest_write_all!();
guestio::guest_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "buyer");
guestauth::guest_emit!();

use bindings::fsm::workflow::engine as fsm;
use bindings::records::store::store as records;

/// `records:store`'s own ceiling on one `list_records` page (record-store's
/// `MAX_LIMIT`); asking for more just gets this many back.
const PAGE_SIZE: u32 = 500;

/// Every entry in `collection`, not one page of it. `list_records` is
/// cursor-paginated: a single call answers at most `PAGE_SIZE` entries, so an
/// aggregate (a balance, a reputation, a fraud flag) or an admin listing that
/// reads one page silently ignores every document past it.
pub(crate) fn list_all(collection: &str) -> Result<Vec<records::Entry>, ()> {
    drain_pages(|after| {
        records::list_records(collection, PAGE_SIZE, after)
            .map(|page| (page.entries, page.next))
            .map_err(|_| ())
    })
}

/// The paging loop behind [`list_all`], over any `fetch(after) -> (entries,
/// next)`. Stops on the store's own "exhausted" signal — an empty `next` —
/// not on a short page: record-store may hand back fewer entries than asked
/// (index drift) while still having more. A `next` equal to the cursor just
/// used would loop forever, so that stops too.
fn drain_pages<T, E>(
    mut fetch: impl FnMut(&str) -> Result<(Vec<T>, String), E>,
) -> Result<Vec<T>, E> {
    let mut all = Vec::new();
    let mut after = String::new();
    loop {
        let (entries, next) = fetch(&after)?;
        all.extend(entries);
        if next.is_empty() || next == after {
            return Ok(all);
        }
        after = next;
    }
}

/// Idempotent — `define` on an already-defined name just replaces it with the
/// same definition, which is harmless (same discipline
/// `guest_owner_or_admin_policy!` uses for idempotent policy-rule setup
/// elsewhere in this repo). Called by `seed` here because nothing else exists
/// yet to call it; a real part's own code path (whichever fires first) is
/// expected to call the SAME definitions, not new ones — CONTRACT.md is the
/// source of truth for what each machine's states/transitions are, and this
/// function is just that table typed into Rust.
fn define_machines() {
    let def =
        |states: &[&str], initial: &str, transitions: &[(&str, &str, &str)], terminal: &[&str]| {
            fsm::Definition {
                states: states.iter().map(|s| s.to_string()).collect(),
                initial: initial.to_string(),
                transitions: transitions
                    .iter()
                    .map(|(event, source, target)| fsm::Transition {
                        event: event.to_string(),
                        source: source.to_string(),
                        target: target.to_string(),
                    })
                    .collect(),
                terminal: terminal.iter().map(|s| s.to_string()).collect(),
            }
        };
    let _ = fsm::define(
        "order",
        &def(
            &["placed", "paid", "shipped", "delivered", "cancelled"],
            "placed",
            &[
                ("pay", "placed", "paid"),
                ("ship", "paid", "shipped"),
                ("deliver", "shipped", "delivered"),
                ("cancel", "placed", "cancelled"),
                ("cancel", "paid", "cancelled"),
            ],
            &["delivered", "cancelled"],
        ),
    );
    let _ = fsm::define(
        "payment",
        &def(
            &["authorized", "captured", "refunded"],
            "authorized",
            &[("capture", "authorized", "captured"), ("refund", "captured", "refunded")],
            &["refunded"],
        ),
    );
    let _ = fsm::define(
        "shipment",
        &def(
            &["label_created", "in_transit", "delivered"],
            "label_created",
            &[("dispatch", "label_created", "in_transit"), ("deliver", "in_transit", "delivered")],
            &["delivered"],
        ),
    );
    let _ = fsm::define(
        "return",
        &def(
            &["requested", "approved", "rejected"],
            "requested",
            &[("approve", "requested", "approved"), ("reject", "requested", "rejected")],
            &["approved", "rejected"],
        ),
    );
    let _ = fsm::define(
        "dispute",
        &def(&["open", "resolved"], "open", &[("resolve", "open", "resolved")], &["resolved"]),
    );
}

/// Entries written straight to the store and to `fsm:workflow` instances, so
/// a part can be judged before the part upstream of it exists — the same
/// trick `dispatch-domain::seed` uses for its one collection, extended here
/// across five collections and three machines.
///
/// Three orders, each a different point in the lifecycle, and each choice is
/// scaffold saying what it is:
///
///   * order 1 stays at `placed` — the state a real `POST /api/orders` would
///     leave it in, so `fulfillment`'s gate can pay/ship it itself and prove
///     ITS OWN transitions rather than starting from a state only `catalog`
///     could have produced by a route that doesn't exist yet.
///   * order 2 is pre-paid (a `payments` document AND a `payment` machine
///     instance already at `captured`) — `fulfillment`'s gate needs to ship
///     something already paid without depending on its OWN `pay` route
///     working first, and `trust`'s gate needs a non-delivered order to open
///     a dispute against.
///   * order 3 is pre-delivered (payment captured, a `shipments` document,
///     `shipment` machine at `delivered`, `order` machine at `delivered`) —
///     `fulfillment`'s gate needs a delivered order to request a return
///     against, and `trust`'s gate needs one to prove disputes are REFUSED
///     once delivered.
///
/// Two listings, different vendors. `body` may be `{"vendor_a","vendor_b",
/// "buyer"}` — REAL subjects a gate already registered and logged in as, so
/// ownership-gated routes (pay, ship, return, message a vendor) are
/// testable with an account that actually owns the seeded row, not just a
/// placeholder nothing can authenticate as. Any field left out (or the whole
/// body empty) falls back to a placeholder subject — fine for the gates that
/// only need SHAPE and cross-part reads, not a specific real caller's write
/// permissions.
fn seed(body: &str) -> Reply {
    define_machines();
    let params: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::json!({}));
    let subject_or = |key: &str, default: &str| {
        params[key].as_str().filter(|s| !s.is_empty()).unwrap_or(default).to_string()
    };
    let vendor_a = subject_or("vendor_a", "usr_vendor_a");
    let vendor_b = subject_or("vendor_b", "usr_vendor_b");
    let buyer = subject_or("buyer", "usr_seed_buyer");

    let mut listing_ids = Vec::new();
    for (vendor, title, description, price, stock, category) in [
        (vendor_a.as_str(), "Widget", "A perfectly ordinary widget", 1000u32, 50u32, "electronics"),
        (vendor_b.as_str(), "Gadget", "A perfectly ordinary gadget", 2000u32, 5u32, "home"),
    ] {
        match records::create(
            "listings",
            &serde_json::json!({
                "vendor": vendor, "title": title, "description": description,
                "price": price, "stock": stock, "category": category,
                "category_confidence": 0, "status": "active",
            })
            .to_string(),
            &["vendor".to_string(), "status".to_string()],
        ) {
            Ok(e) => listing_ids.push(e.id),
            Err(_) => return Reply::err(500, "seed_failed"),
        }
    }
    let listing_a = listing_ids[0].clone();

    let new_order = |buyer: &str, listing_id: &str, total: u32| -> Option<String> {
        records::create(
            "orders",
            &serde_json::json!({
                "buyer": buyer,
                "items": [{"listing_id": listing_id, "quantity": 1, "unit_price": total}],
                "total": total,
            })
            .to_string(),
            &["buyer".to_string()],
        )
        .ok()
        .map(|e| e.id)
    };

    let mut order_ids = Vec::new();

    // order 1: placed, nothing else.
    let Some(order1) = new_order(&buyer, &listing_a, 1000) else {
        return Reply::err(500, "seed_failed");
    };
    if fsm::create_instance("order", &order1).is_err() {
        return Reply::err(500, "seed_failed");
    }
    order_ids.push(order1.clone());

    // order 2: paid.
    let Some(order2) = new_order(&buyer, &listing_a, 1000) else {
        return Reply::err(500, "seed_failed");
    };
    if fsm::create_instance("order", &order2).is_err()
        || fsm::fire("order", &order2, "pay").is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    if records::create(
        "payments",
        &serde_json::json!({"order_id": order2, "refunded_amount": 0}).to_string(),
        &["order_id".to_string()],
    )
    .is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    if fsm::create_instance("payment", &order2).is_err()
        || fsm::fire("payment", &order2, "capture").is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    order_ids.push(order2.clone());

    // order 3: delivered.
    let Some(order3) = new_order(&buyer, &listing_a, 1000) else {
        return Reply::err(500, "seed_failed");
    };
    if fsm::create_instance("order", &order3).is_err()
        || fsm::fire("order", &order3, "pay").is_err()
        || fsm::fire("order", &order3, "ship").is_err()
        || fsm::fire("order", &order3, "deliver").is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    if records::create(
        "payments",
        &serde_json::json!({"order_id": order3, "refunded_amount": 0}).to_string(),
        &["order_id".to_string()],
    )
    .is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    if fsm::create_instance("payment", &order3).is_err()
        || fsm::fire("payment", &order3, "capture").is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    let shipment3 = match records::create(
        "shipments",
        &serde_json::json!({"order_id": order3, "carrier": "seed-carrier", "tracking": "SEED123"})
            .to_string(),
        &["order_id".to_string()],
    ) {
        Ok(e) => e.id,
        Err(_) => return Reply::err(500, "seed_failed"),
    };
    if fsm::create_instance("shipment", &shipment3).is_err()
        || fsm::fire("shipment", &shipment3, "dispatch").is_err()
        || fsm::fire("shipment", &shipment3, "deliver").is_err()
    {
        return Reply::err(500, "seed_failed");
    }
    order_ids.push(order3.clone());

    // The sale entry `order2`'s `pay` would have posted, written directly —
    // `ledger`'s own gate needs a REAL non-zero balance to aggregate, not
    // just the zero-entries case, and posting it is `fulfillment`'s route,
    // a stub at judgment time (same reason `dispatch-domain::seed` pre-
    // assigns its third request rather than leaving `manifest` nothing to
    // count).
    let vendor_account = format!("vendor:{vendor_a}");
    let _ = records::create(
        "ledger_entries",
        &serde_json::json!({
            "memo": format!("order {order2} sale"),
            "lines": [
                {"account": "platform:cash", "amount": 1000, "side": "debit"},
                {"account": vendor_account, "amount": 900, "side": "credit"},
                {"account": "platform:fees", "amount": 100, "side": "credit"},
            ],
        })
        .to_string(),
        &[],
    );

    Reply::json(
        201,
        serde_json::json!({
            "listing_ids": listing_ids,
            "order_ids": order_ids,
            "shipment_id": shipment3,
            "vendor_a": vendor_a,
        }),
    )
}

/// A stored document with its fsm status (if any) merged in, or 404 — the
/// same shape `dispatch-domain`'s `["test","request",id]` route answers,
/// generalised to any collection so a gate can inspect what ANOTHER part
/// wrote without depending on that part's own (still-501) GET route.
fn test_doc(collection: &str, id: &str, machine: Option<&str>) -> Reply {
    let entry = match records::get(collection, id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut v: serde_json::Value =
        serde_json::from_str(&entry.data).unwrap_or(serde_json::json!({}));
    if let serde_json::Value::Object(ref mut m) = v {
        m.insert("id".to_string(), serde_json::json!(entry.id));
        if let Some(machine) = machine {
            if let Ok(status) = fsm::get_status(machine, id) {
                m.insert("status".to_string(), serde_json::json!(status.state));
            }
        }
    }
    Reply::json(200, v)
}

/// A document found by an indexed field rather than its own store id, with
/// its fsm status merged in — `payments` is looked up by `order_id`
/// (CONTRACT.md: "one payment per order", but the STORED document's own id
/// is whatever `records::create` generated, not the order's id — only the
/// fsm INSTANCE is deliberately keyed by the order id). Answers the first
/// match, or 404.
fn test_doc_by(collection: &str, field: &str, value: &str, machine: Option<&str>) -> Reply {
    let value_json = serde_json::to_string(&value.to_string()).unwrap_or_default();
    let entry = match records::find_by(collection, field, &value_json) {
        Ok(entries) if !entries.is_empty() => entries.into_iter().next().unwrap(),
        _ => return Reply::err(404, "not_found"),
    };
    let mut v: serde_json::Value =
        serde_json::from_str(&entry.data).unwrap_or(serde_json::json!({}));
    if let serde_json::Value::Object(ref mut m) = v {
        m.insert("id".to_string(), serde_json::json!(entry.id));
        if let Some(machine) = machine {
            // The payment fsm instance is keyed by the ORDER id (`value`
            // here), not by this document's own store id.
            if let Ok(status) = fsm::get_status(machine, value) {
                m.insert("status".to_string(), serde_json::json!(status.state));
            }
        }
    }
    Reply::json(200, v)
}

/// The FSM's own status for any machine/instance — the general fallback when
/// a part's fixture-inspection need isn't one of the specific `test_doc`
/// shapes above (e.g. checking a `dispute` or `return` instance a gate
/// created itself, mid-scenario, rather than one `seed` pre-wrote).
fn test_fsm(machine: &str, instance: &str) -> Reply {
    match fsm::get_status(machine, instance) {
        Ok(status) => Reply::json(
            200,
            serde_json::json!({"machine": status.machine, "instance": status.instance, "state": status.state, "done": status.done}),
        ),
        Err(_) => Reply::err(404, "not_found"),
    }
}

struct Component;

impl bindings::exports::wasi::http::incoming_handler::Guest for Component {
    fn handle(
        request: bindings::wasi::http::types::IncomingRequest,
        response_out: bindings::wasi::http::types::ResponseOutparam,
    ) {
        use bindings::wasi::http::types::Method;
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let raw_path = path.split('?').next().unwrap_or("/").to_string();
        let bearer = bearer(&request).unwrap_or_default();
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => read_body(&request),
            _ => String::new(),
        };
        let segments: Vec<String> =
            raw_path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let route = Route { segments, bearer };
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

        let reply = match (&method, seg.as_slice()) {
            (_, ["health"]) => Reply::json(200, serde_json::json!({"ok": true})),
            (Method::Post, ["register"]) => register(&body),
            (Method::Post, ["login"]) => login(&body),
            (Method::Post, ["logout"]) => logout(&route),
            (Method::Get, ["me"]) => me(&route),

            (Method::Post, ["test", "seed"]) => seed(&body),
            (Method::Get, ["test", "listing", id]) => test_doc("listings", id, None),
            (Method::Get, ["test", "order", id]) => test_doc("orders", id, Some("order")),
            (Method::Get, ["test", "payment", order_id]) => {
                test_doc_by("payments", "order_id", order_id, Some("payment"))
            }
            (Method::Get, ["test", "shipment", id]) => test_doc("shipments", id, Some("shipment")),
            (Method::Get, ["test", "fsm", machine, instance]) => test_fsm(machine, instance),

            // Shared `/api/vendors/{subject}/...` prefix — disambiguate on the
            // trailing segment before it ever reaches a part's own match.
            (Method::Get, ["api", "vendors", _, "reputation"]) => {
                trust::handle(&method, &route, &body)
            }
            (Method::Get, ["api", "vendors", _, "balance"])
            | (Method::Post, ["api", "vendors", _, "payout"])
            | (Method::Get, ["api", "ledger", ..]) => ledger::handle(&method, &route, &body),

            // `/api/orders/{id}/...` sub-routes belong to whichever part owns
            // that action, not to `catalog` (which owns only bare
            // `/api/orders` and `/api/orders/{id}` with no further segment).
            (Method::Post, ["api", "orders", _, "pay"])
            | (Method::Post, ["api", "orders", _, "ship"])
            | (Method::Post, ["api", "shipments", ..])
            | (Method::Post, ["api", "orders", _, "refund"])
            | (Method::Post, ["api", "orders", _, "returns"])
            | (Method::Post, ["api", "returns", ..]) => fulfillment::handle(&method, &route, &body),

            (Method::Post, ["api", "orders", _, "disputes"])
            | (Method::Get, ["api", "disputes"])
            | (Method::Post, ["api", "disputes", ..]) => trust::handle(&method, &route, &body),

            (Method::Post, ["api", "listings", _, "inquiries"])
            | (Method::Get, ["api", "inquiries"])
            | (Method::Get, ["api", "inquiries", _, "messages"])
            | (Method::Post, ["api", "inquiries", _, "messages"]) => {
                messaging::handle(&method, &route, &body)
            }

            // Everything else under `/api/listings` and bare `/api/orders`:
            // catalog's.
            (_, ["api", "listings", ..]) | (_, ["api", "orders", ..]) => {
                catalog::handle(&method, &route, &body)
            }

            _ => Reply::err(404, "not_found"),
        };
        emit(response_out, reply);
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::drain_pages;

    /// A fake `list_records` over `0..total`, `size` per page, cursor = the
    /// last id handed out — record-store's own cursor shape.
    fn fake_store(total: u32, size: u32) -> impl FnMut(&str) -> Result<(Vec<u32>, String), ()> {
        move |after: &str| {
            let start = if after.is_empty() { 0 } else { after.parse::<u32>().unwrap() + 1 };
            let end = (start + size).min(total);
            let entries: Vec<u32> = (start..end).collect();
            let next = if end < total { (end - 1).to_string() } else { String::new() };
            Ok((entries, next))
        }
    }

    #[test]
    fn reads_every_page_not_just_the_first() {
        let all = drain_pages(fake_store(2_345, 500)).unwrap();
        assert_eq!(all.len(), 2_345);
        assert_eq!(all, (0..2_345).collect::<Vec<_>>());
    }

    #[test]
    fn an_empty_collection_is_one_empty_page() {
        assert!(drain_pages(fake_store(0, 500)).unwrap().is_empty());
    }

    #[test]
    fn a_short_page_with_a_cursor_keeps_going() {
        // Index drift: page one comes back with fewer entries than asked but
        // the store still says there is more.
        let mut calls = 0;
        let all = drain_pages(|after: &str| {
            calls += 1;
            Ok::<_, ()>(match after {
                "" => (vec![1, 2], "c1".to_string()),
                "c1" => (vec![3], String::new()),
                _ => unreachable!(),
            })
        })
        .unwrap();
        assert_eq!(all, vec![1, 2, 3]);
        assert_eq!(calls, 2);
    }

    #[test]
    fn a_cursor_that_does_not_advance_stops() {
        let all = drain_pages(|_after: &str| Ok::<_, ()>((vec![7], "stuck".to_string()))).unwrap();
        assert_eq!(all, vec![7, 7]);
    }

    #[test]
    fn a_store_error_is_an_error() {
        assert!(drain_pages(|_after: &str| Err::<(Vec<u32>, String), _>(())).is_err());
    }
}
