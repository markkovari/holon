use crate::bindings::auth::identity::accounts;
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::rbac;
use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::ledger::doubleentry::ledger;
use crate::bindings::payment::stripe::gateway as stripe;
use crate::bindings::records::store::store;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
struct CreateOrderReq {
    product_id: String,
    qty: u32,
    stripe_source: String,
}

#[derive(Deserialize)]
struct AuthReq {
    email: String,
    password: String,
    tenant: Option<String>,
    role: Option<String>,
}

pub fn register(method: &Method, body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(405, "method_not_allowed");
    }
    let req: AuthReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => return Reply::err(400, "bad_request"),
    };
    let tenant = req.tenant.unwrap_or_else(|| "nexus".into());
    match accounts::register(&req.email, &req.password, &tenant) {
        Ok(subject) => {
            if let Some(r) = req.role {
                let _ = rbac::assign_role(&tenant, &subject.subject, &r);
            }
            Reply::json(201, json!({ "subject": subject.subject, "tenant": tenant }))
        }
        Err(e) => Reply::err(400, &format!("{:?}", e)),
    }
}

pub fn login(method: &Method, body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(405, "method_not_allowed");
    }
    let req: AuthReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => return Reply::err(400, "bad_request"),
    };
    let tenant = req.tenant.unwrap_or_else(|| "nexus".into());
    match accounts::login(&req.email, &req.password, &tenant) {
        Ok(tp) => Reply::json(
            200,
            json!({
                "access_token": tp.access_token,
                "refresh_token": tp.refresh_token,
                "expires_in": tp.expires_in,
                "token_type": "Bearer"
            }),
        ),
        Err(e) => Reply::err(401, &format!("{:?}", e)),
    }
}

pub fn orders(method: &Method, route: &Route, body: &str) -> Reply {
    match method {
        Method::Post => create_order(route, body),
        Method::Get => list_orders(route),
        _ => Reply::err(405, "method_not_allowed"),
    }
}

fn create_order(route: &Route, body: &str) -> Reply {
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthorized");
    }

    let principal = match authz::introspect(&route.bearer) {
        Ok(p) => p,
        Err(_) => return Reply::err(401, "invalid_token"),
    };

    if !principal.roles.contains(&"customer".to_string()) {
        return Reply::err(403, "forbidden");
    }

    let req: CreateOrderReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => return Reply::err(400, "bad_request"),
    };
    if req.qty == 0 {
        return Reply::err(400, "qty_must_be_positive");
    }

    let price_per_unit = 2999;
    let total_amount = price_per_unit * (req.qty as i64);
    let desc = format!("Order for {} (qty: {})", req.product_id, req.qty);

    let charge_id = match stripe::charge(total_amount, "USD", &req.stripe_source, Some(&desc)) {
        Ok(id) => id,
        Err(e) => {
            return Reply::err(500, &format!("payment_failed: {:?}", e));
        }
    };

    let order_id = Uuid::new_v4().to_string();

    // Create ledger entry instead of posting
    let ledger_entry = ledger::Entry {
        id: format!("tx-{}", order_id),
        memo: format!("Order {}", order_id),
        lines: vec![
            ledger::Line {
                account: "asset:cash:stripe".into(),
                amount: total_amount,
                side: ledger::Side::Debit,
            },
            ledger::Line {
                account: format!("revenue:sales:{}", req.product_id),
                amount: total_amount, // Line amount is always positive, side is credit
                side: ledger::Side::Credit,
            },
        ],
    };

    if ledger::validate(&ledger_entry).is_err() {
        return Reply::err(500, "ledger_validation_failed");
    }

    let ledger_data = json!({
        "id": ledger_entry.id,
        "memo": ledger_entry.memo,
        "lines": [
            { "account": "asset:cash:stripe", "amount": total_amount, "side": "debit" },
            { "account": format!("revenue:sales:{}", req.product_id), "amount": total_amount, "side": "credit" }
        ]
    });

    // Store ledger entry
    if store::create("ledger_entries", &serde_json::to_string(&ledger_data).unwrap(), &[]).is_err()
    {
        return Reply::err(500, "store_ledger_failed");
    }

    let order_data = json!({
        "customer": principal.subject,
        "product_id": req.product_id,
        "qty": req.qty,
        "status": "paid",
        "charge_id": charge_id,
    });

    let order_entry = match store::create(
        "orders",
        &serde_json::to_string(&order_data).unwrap(),
        &["customer".into(), "status".into()],
    ) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_order_failed"),
    };

    let _ = fsm::define(
        "fulfillment_workflow",
        &fsm::Definition {
            states: vec!["paid".into(), "shipped".into()],
            initial: "paid".into(),
            transitions: vec![fsm::Transition {
                source: "paid".into(),
                event: "pack_and_ship".into(),
                target: "shipped".into(),
            }],
            terminal: vec!["shipped".into()],
        },
    );

    if fsm::create_instance("fulfillment_workflow", &order_entry.id).is_err() {
        return Reply::err(500, "fsm_create_failed");
    }

    Reply::json(201, json!({ "id": order_entry.id, "status": "paid", "charge_id": charge_id }))
}

fn list_orders(route: &Route) -> Reply {
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthorized");
    }

    let principal = match authz::introspect(&route.bearer) {
        Ok(p) => p,
        Err(_) => return Reply::err(401, "invalid_token"),
    };

    if !principal.roles.contains(&"customer".to_string())
        && !principal.roles.contains(&"fulfillment".to_string())
    {
        return Reply::err(403, "forbidden");
    }

    let mut items = Vec::new();
    let mut after = "".to_string();

    while let Ok(page) = store::list_records("orders", 100, &after) {
        for record in page.entries {
            if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&record.data) {
                parsed["id"] = json!(record.id);

                let status = match fsm::get_status("fulfillment_workflow", &record.id) {
                    Ok(s) => s.state,
                    Err(_) => "unknown".to_string(),
                };
                parsed["status"] = json!(status);
                items.push(parsed);
            }
        }
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }

    Reply::json(200, json!({ "items": items }))
}

pub fn fulfill_order(method: &Method, route: &Route, id: &str, _body: &str) -> Reply {
    if !matches!(method, Method::Post) {
        return Reply::err(405, "method_not_allowed");
    }

    if route.bearer.is_empty() {
        return Reply::err(401, "unauthorized");
    }

    let principal = match authz::introspect(&route.bearer) {
        Ok(p) => p,
        Err(_) => return Reply::err(401, "invalid_token"),
    };

    if !principal.roles.contains(&"fulfillment".to_string()) {
        return Reply::err(403, "forbidden");
    }

    if fsm::fire("fulfillment_workflow", id, "pack_and_ship").is_err() {
        return Reply::err(500, "fsm_fire_failed");
    }

    let record = match store::get("orders", id) {
        Ok(r) => r,
        Err(_) => return Reply::err(404, "not_found"),
    };

    let mut order_data: Value = match serde_json::from_str(&record.data) {
        Ok(v) => v,
        Err(_) => return Reply::err(500, "bad_data"),
    };

    order_data["status"] = json!("shipped");
    if store::update("orders", id, &serde_json::to_string(&order_data).unwrap(), record.revision)
        .is_err()
    {
        return Reply::err(500, "store_update_failed");
    }

    Reply::json(200, json!({ "id": id, "status": "shipped" }))
}
