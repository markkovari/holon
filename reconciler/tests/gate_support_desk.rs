//! The `support:desk` tickets gate, ported from
//! `components/support-desk-domain/e2e-tickets.sh`.
//!
//! `e2e-reply.sh` wants a model on :8787 and `e2e-courier.sh` wants a webhook sink it
//! starts itself; both stay shell gates for now.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const CRATE: &str = "support-desk-domain";

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut b = json!({ "subject": subject });
    if let Some(s) = scopes {
        b["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, b).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}

#[test]
fn tickets_open_validate_their_address_and_list_oldest_first() {
    let Some(gate) = Gate::compose_and_start("support", CRATE, &[]) else { return };
    let w = token(&gate, "agent", None);
    let ticket = json!({
        "subject":"Cannot export my data",
        "body":"The export button spins forever.",
        "customer":"webhook:https://acme.test/hooks/ada"});

    let (c, _) = gate.post("/api/tickets", None, ticket.clone());
    assert_eq!(c, 401, "opening a ticket with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["tickets:read"])));
    let (c, _) = gate.post("/api/tickets", Some(&ro), ticket.clone());
    assert_eq!(c, 403, "a token with only tickets:read must be 403 on a write");
    let (c, _) = gate.post(
        "/api/tickets",
        Some(&w),
        json!({"subject":"","body":"x","customer":"webhook:https://acme.test/h"}),
    );
    assert_eq!(c, 400, "an empty subject must be 400 invalid_ticket");

    // The address check. `mailto:` is a real scheme and still not something this desk
    // delivers.
    for bad in ["ada@example.test", "mailto:ada@example.test", "https://acme.test/hooks/ada", ""] {
        let (c, _) =
            gate.post("/api/tickets", Some(&w), json!({"subject":"s","body":"b","customer":bad}));
        assert_eq!(
            c, 400,
            "customer '{bad}' cannot be delivered to and must be 400 invalid_ticket"
        );
    }

    let (_, created) = gate.post("/api/tickets", Some(&w), ticket);
    let id = field(&created, "id");
    assert!(!id.is_empty(), "POST /api/tickets returned no id");

    let raw = gate.stored("ticket", &id);
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    assert_eq!(d["state"], "open", "a new ticket is open: {d}");
    assert_eq!(d["customer"], "webhook:https://acme.test/hooks/ada", "{d}");
    assert!(
        d.get("reply").is_none(),
        "tickets must not invent a reply — that is the reply part's job"
    );
    assert!(
        d["opened_at"].as_str().unwrap_or_default().ends_with('Z'),
        "opened_at must be RFC3339 UTC: {d}"
    );

    let (_, read) = gate.get(&format!("/api/tickets/{id}"), Some(&w));
    assert!(
        !read.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&read);
    assert_eq!(d["subject"], "Cannot export my data", "{d}");
    assert_eq!(d["id"], id.as_str(), "a ticket must carry its id: {d}");
    let (c, _) = gate.get("/api/tickets/nope", Some(&w));
    assert_eq!(c, 404, "an unknown ticket id must be 404");

    // The list, which is an index lookup and oldest first.
    let listed = parse(&gate.get("/api/tickets", Some(&w)).1);
    let items = listed["tickets"].as_array().cloned().unwrap_or_default();
    assert!(!items.is_empty(), "the open list is empty right after a ticket was opened: {listed}");
    let ids: Vec<&str> = items.iter().filter_map(|t| t["id"].as_str()).collect();
    assert!(ids.contains(&id.as_str()), "the new ticket is missing: {items:?}");
    for t in &items {
        assert_eq!(t["state"], "open", "the default list is the open one: {t}");
    }
    let stamps: Vec<&str> = items.iter().filter_map(|t| t["opened_at"].as_str()).collect();
    let mut sorted = stamps.clone();
    sorted.sort();
    assert_eq!(stamps, sorted, "oldest first, and these are not: {stamps:?}");

    let answered = parse(&gate.get("/api/tickets?state=answered", Some(&w)).1);
    assert_eq!(
        answered["tickets"],
        json!([]),
        "nothing is answered yet and the list said otherwise"
    );
}
