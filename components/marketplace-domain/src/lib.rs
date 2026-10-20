//! The ROUTER of a 5-part decomposed goal (ADR-0086, CONTRACT.md); no part may write it.
//! Parts meet only in records:store and fsm:workflow instances, never via each other's functions.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../fsm-workflow/wit",
            "../ledger/wit",
            "../jev-decision/wit",
            "wit",
        ],
        world: "marketplace:domain/marketplace-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod catalog;
mod fulfillment;
mod ledger;
mod messaging;
mod trust;

pub use guestauth::Route;

pub const TENANT: &str = "marketplace";
const ROLES: &[&str] = &["vendor", "buyer", "admin"];

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

guestauth::guest_auth_reply!();
guestauth::guest_introspect!();
guestauth::guest_role_check!(is_admin, "admin");
guestauth::guest_p3_audit!(TENANT);
guestauth::guest_accounts_endpoints!(TENANT, ROLES, "buyer");
guestauth::guest_p3_emit!();

use bindings::fsm::workflow::engine as fsm;
use bindings::records::store::store as records;

/// records:store's own page ceiling (record-store's `MAX_LIMIT`).
const PAGE_SIZE: u32 = 500;

/// Every page: an aggregate reading one page silently ignores the rest.
pub(crate) fn list_all(collection: &str) -> Result<Vec<records::Entry>, ()> {
    drain_pages(|after| {
        records::list_records(collection, PAGE_SIZE, after)
            .map(|page| (page.entries, page.next))
            .map_err(|_| ())
    })
}

/// Stops on an empty `next`, not a short page: record-store may return fewer than asked while having more.
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

/// Idempotent (`define` replaces). CONTRACT.md is the source of truth for each machine.
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

/// Written straight to the store and fsm, so a part is judgeable before its upstream exists.
/// Orders: 1 placed, 2 paid, 3 delivered; `body` may name real subjects for ownership-gated routes.
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

    let Some(order1) = new_order(&buyer, &listing_a, 1000) else {
        return Reply::err(500, "seed_failed");
    };
    if fsm::create_instance("order", &order1).is_err() {
        return Reply::err(500, "seed_failed");
    }
    order_ids.push(order1.clone());

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

    // order2's sale entry, written directly: ledger's gate needs a real balance and fulfillment's `pay` may be a stub.
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

/// Any collection, so a gate can inspect another part's writes without its GET route.
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
            // The payment fsm instance is keyed by the ORDER id (`value` here), not by this document's store id.
            if let Ok(status) = fsm::get_status(machine, value) {
                m.insert("status".to_string(), serde_json::json!(status.state));
            }
        }
    }
    Reply::json(200, v)
}

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

impl bindings::p3::handler::Guest for Component {
    async fn handle(
        request: bindings::p3::http::types::Request,
    ) -> Result<bindings::p3::http::types::Response, bindings::p3::http::types::ErrorCode> {
        use bindings::p3::http::types::Method;
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let raw_path = path.split('?').next().unwrap_or("/").to_string();
        let bearer = bearer(&request).unwrap_or_default();
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => read_body(request).await,
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

            (Method::Get, ["api", "vendors", _, "reputation"]) => {
                trust::handle(&method, &route, &body)
            }
            (Method::Get, ["api", "vendors", _, "balance"])
            | (Method::Post, ["api", "vendors", _, "payout"])
            | (Method::Get, ["api", "ledger", ..]) => ledger::handle(&method, &route, &body),

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

            (_, ["api", "listings", ..]) | (_, ["api", "orders", ..]) => {
                catalog::handle(&method, &route, &body)
            }

            _ => Reply::err(404, "not_found"),
        };
        emit(reply)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::drain_pages;

    /// cursor = last id handed out (record-store's cursor shape).
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
        // Index drift: page one has fewer entries than asked but the store says there is more.
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
