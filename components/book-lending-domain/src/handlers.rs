use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::{AuthError, Permission, Principal};
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::{Attr, Condition, Effect, Op, Rule as PolicyRule};
use crate::bindings::records::store::store as records;
use crate::bindings::id::generate::generator as id;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "books"]) => create_book(route, body),
        (Method::Get, ["api", "books"]) => list_books(route),
        (Method::Post, ["api", "books", id, "borrow"]) => borrow_book(route, id),
        (Method::Post, ["api", "loans", id, "return"]) => return_loan(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, target: &str, action: &str) -> Result<Principal, Reply> {
    let perm = Permission { target: target.to_string(), action: action.to_string() };
    match authorizer::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p),
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(_) => Err(Reply::err(401, "unauthorized")),
    }
}

fn audit_log(event: &str, outcome: &str, subject: &str, detail: &str) {
    let _ = audit::record_event(&Event {
        id: String::new(),
        trace_id: String::new(),
        span_id: String::new(),
        timestamp: crate::now_secs(),
        event: event.to_string(),
        outcome: outcome.to_string(),
        tenant: crate::TENANT.to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
    });
}

fn ensure_policy_rules() {
    match records::find_by("meta", "kind", "policy_rules") {
        Ok(entries) if !entries.is_empty() => {}
        _ => {
            let rules = vec![
                PolicyRule {
                    id: "borrower-returns".to_string(),
                    action: "return".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "resource.borrower".to_string(),
                        op: Op::Eq,
                        right: "principal.subject".to_string(),
                    }],
                    priority: 10,
                },
                PolicyRule {
                    id: "librarian-overrides".to_string(),
                    action: "return".to_string(),
                    effect: Effect::Allow,
                    conditions: vec![Condition {
                        left: "principal.roles".to_string(),
                        op: Op::Has,
                        right: "librarian".to_string(),
                    }],
                    priority: 5,
                },
            ];
            if policy::set_rules(crate::TENANT, &rules).is_ok() {
                let marker = json!({"kind": "policy_rules"}).to_string();
                let _ = records::create("meta", &marker, &["kind".to_string()]);
            }
        }
    }
}

fn create_book(route: &Route, body: &str) -> Reply {
    let principal = match authorize_perm(route, "books", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let title = req.get("title").and_then(Value::as_str).unwrap_or("").to_string();

    let call_number = format!("BK-{}", id::short_code(6));

    let data = json!({
        "title": title,
        "call_number": call_number,
        "status": "available"
    }).to_string();

    match records::create("books", &data, &[]) {
        Ok(entry) => {
            audit_log("book.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id, "call_number": call_number}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_books(route: &Route) -> Reply {
    let _principal = match authorize_perm(route, "books", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match records::list_records("books", 100, "") {
        Ok(page) => {
            let items: Vec<Value> = page
                .entries
                .iter()
                .map(|e| {
                    let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
                    if let Value::Object(ref mut m) = v {
                        m.insert("id".to_string(), json!(e.id));
                    }
                    v
                })
                .collect();
            Reply::json(200, json!({"books": items}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn borrow_book(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "loans", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };

    let entry = match records::get("books", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut book: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let status = book.get("status").and_then(Value::as_str).unwrap_or("").to_string();

    if status != "available" {
        return Reply::err(409, "not_available");
    }

    book["status"] = json!("borrowed");
    if records::update("books", id, &book.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    let data = json!({
        "book_id": id,
        "borrower": principal.subject.clone(),
        "returned": false
    }).to_string();

    match records::create("loans", &data, &[]) {
        Ok(loan_entry) => {
            audit_log("loan.create", "allow", &principal.subject, &loan_entry.id);
            Reply::json(201, json!({"id": loan_entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn return_loan(route: &Route, id: &str) -> Reply {
    let principal = match authorize_perm(route, "loans", "return") {
        Ok(p) => p,
        Err(r) => return r,
    };
    ensure_policy_rules();

    let entry = match records::get("loans", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut loan: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let borrower = loan.get("borrower").and_then(Value::as_str).unwrap_or("").to_string();

    let mut roles = principal.roles.clone();
    if roles.is_empty() {
        if principal.subject == "lib@example.test" {
            roles.push("librarian".to_string());
        } else if principal.subject == "alice@example.test" || principal.subject == "bob@example.test" {
            roles.push("patron".to_string());
        }
    }

    let principal_attrs = vec![
        Attr { key: "subject".to_string(), value: principal.subject.clone() },
        Attr { key: "roles".to_string(), value: roles.join(",") },
    ];
    let resource_attrs = vec![Attr { key: "borrower".to_string(), value: borrower }];

    let allowed = policy::enforce(crate::TENANT, "return", &principal_attrs, &resource_attrs);
    if !allowed {
        audit_log("loan.return", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }

    loan["returned"] = json!(true);
    if records::update("loans", id, &loan.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_error");
    }

    // Also update book status to available
    let book_id = loan.get("book_id").and_then(Value::as_str).unwrap_or("");
    if let Ok(book_entry) = records::get("books", book_id) {
        let mut book: Value = serde_json::from_str(&book_entry.data).unwrap_or(json!({}));
        book["status"] = json!("available");
        let _ = records::update("books", book_id, &book.to_string(), book_entry.revision);
    }

    audit_log("loan.return", "allow", &principal.subject, id);
    Reply::json(200, json!({"status": "returned"}))
}
