//! The two portable `invoice:copilot` gates, ported from
//! `components/invoice-copilot-domain/e2e-*.sh`. `e2e-copilot.sh` wants a model on
//! :8787 and stays a shell gate.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const CRATE: &str = "invoice-copilot-domain";

fn start(config: &[&str]) -> Option<Gate> {
    Gate::compose_and_start("invoice", CRATE, config)
}
fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut b = json!({ "subject": subject });
    if let Some(s) = scopes {
        b["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, b).1, "token");
    assert!(!t.is_empty(), "POST /test/token returned no token — the scaffold is broken, not the part");
    t
}

#[test]
fn invoices_open_as_drafts_and_the_limit_is_per_subject() {
    let Some(gate) = start(&["max-attempts=3", "lockout-window=60"]) else { return };
    let w = token(&gate, "biller", None);
    let inv = json!({"customer":"acme-gmbh","currency":"EUR"});

    let (c, _) = gate.post("/api/invoices", None, inv.clone());
    assert_eq!(c, 401, "opening an invoice with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["invoices:read"])));
    let (c, _) = gate.post("/api/invoices", Some(&ro), inv.clone());
    assert_eq!(c, 403, "a token with only invoices:read must be 403 on a write");
    let (c, _) = gate.post("/api/invoices", Some(&w), json!({"customer":"","currency":"EUR"}));
    assert_eq!(c, 400, "an empty customer must be 400 invalid_invoice");

    // A currency the arithmetic cannot do. Refused here rather than at posting time.
    let (c, _) = gate.post("/api/invoices", Some(&w), json!({"customer":"acme-gmbh","currency":"QQQ"}));
    assert_eq!(
        c, 400,
        "a currency money:amount does not know must be 400 bad_money — an invoice that cannot be \
         totalled is not a draft, it is a trap"
    );

    let (_, created) = gate.post("/api/invoices", Some(&w), inv.clone());
    let id = field(&created, "id");
    assert!(!id.is_empty(), "POST /api/invoices returned no id");

    let raw = gate.stored("invoice", &id);
    assert!(!raw.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    let d = parse(&raw);
    assert_eq!(d["state"], "draft", "a new invoice is a draft: {d}");
    assert_eq!(d["lines"], json!([]), "a new invoice has no lines: {d}");
    assert_eq!(d["total_units"], 0, "a new invoice totals zero, as an integer: {d}");
    assert!(d.get("entry").is_none(), "invoices must not invent a posted entry — that is the posting part's job");
    assert!(d["created_at"].as_str().unwrap_or_default().ends_with('Z'), "created_at must be RFC3339 UTC: {d}");

    let (_, read) = gate.get(&format!("/api/invoices/{id}"), Some(&w));
    let d = parse(&read);
    assert_eq!(d["id"], id.as_str(), "an invoice must carry its id: {d}");
    assert_eq!(d["currency"], "EUR", "{d}");
    let (c, _) = gate.get("/api/invoices/nope", Some(&w));
    assert_eq!(c, 404, "an unknown invoice id must be 404");

    // The limit, counting what was accepted, keyed on the subject.
    let burst = token(&gate, "burst", None);
    for i in 1..=3 {
        let (c, _) = gate.post("/api/invoices", Some(&burst), inv.clone());
        assert_eq!(c, 201, "invoice {i} of 3 within the limit must be accepted");
    }
    let (_, locked) = gate.post("/api/invoices", Some(&burst), inv.clone());
    let d = parse(&locked);
    assert_eq!(d["error"], "rate_limited", "past the limit the part must refuse and say how long to wait: {d}");
    assert!(d["retry_after"].as_i64().unwrap_or(0) > 0, "retry_after must be the limiter's seconds: {d}");
    let (c, _) = gate.post("/api/invoices", Some(&w), inv);
    assert_eq!(c, 201, "locking out one subject must not lock out another");
}

#[test]
fn posting_writes_one_balanced_entry_and_a_retry_is_not_a_second_charge() {
    let Some(gate) = start(&[]) else { return };
    let t = token(&gate, "biller", None);

    let seed = gate.seed();
    let ids: Vec<String> = seed["invoice_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(ids.len() >= 2, "the fixture produced no invoices — the scaffold is broken, not the part");
    let (empty, filled) = (ids[0].clone(), ids[1].clone());

    let post_it = |id: &str, key: &str| -> (u16, String) {
        gate.with_headers("POST", &format!("/api/invoices/{id}/post"), Some(&t), &[("idempotency-key", key)], None)
    };

    // --- the refusals ---------------------------------------------------------------
    let (c, _) = gate.with_headers("POST", &format!("/api/invoices/{filled}/post"), None, &[("idempotency-key", "k")], None);
    assert_eq!(c, 401, "posting with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["invoices:read"])));
    let (c, _) = gate.with_headers("POST", &format!("/api/invoices/{filled}/post"), Some(&ro), &[("idempotency-key", "k")], None);
    assert_eq!(c, 403, "posting needs invoices:post — a read-only token must be 403");
    let (c, _) = gate.json("POST", &format!("/api/invoices/{filled}/post"), Some(&t), None);
    assert_eq!(c, 400, "posting with no Idempotency-Key must be 400 — a retry would charge twice");
    let (c, _) = post_it("nope", "k1");
    assert_eq!(c, 404, "posting an unknown invoice must be 404");
    let (c, _) = post_it(&empty, "k2");
    assert_eq!(c, 409, "posting an invoice with no lines must be 409 nothing_to_post");
    let (c, _) = gate.get(&format!("/api/invoices/{filled}/entry"), Some(&t));
    assert_eq!(c, 404, "an unposted invoice has no entry: must be 404 not_posted");

    // --- posted once ----------------------------------------------------------------
    let (_, first) = post_it(&filled, "key-abc");
    assert!(!first.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    let d = parse(&first);
    assert_eq!(d["total_units"], 10000, "the posted total must be the invoice's: {d}");
    assert!(d["posted_at"].as_str().unwrap_or_default().ends_with('Z'), "posted_at must be RFC3339 UTC: {d}");

    let d = parse(&gate.stored("invoice", &filled));
    assert_eq!(d["state"], "posted", "a posted invoice is not a draft any more: {d}");
    let e = &d["entry"];
    assert!(e.is_object(), "the invoice has no entry: {d}");
    let lines = e["lines"].as_array().cloned().unwrap_or_default();
    assert!(lines.len() >= 2, "double entry needs two sides: {e}");
    let side = |s: &str| -> i64 {
        lines.iter().filter(|l| l["side"] == s).filter_map(|l| l["amount"].as_i64()).sum()
    };
    let (debits, credits) = (side("debit"), side("credit"));
    assert!(
        debits == credits && debits == 10000,
        "the two sides must be equal and must be the invoice total: debits {debits}, credits {credits}"
    );

    let entry = parse(&gate.get(&format!("/api/invoices/{filled}/entry"), Some(&t)).1);
    assert!(
        entry.get("lines").is_some() || entry.get("entry").is_some(),
        "the entry route answered nothing usable: {entry}"
    );

    // --- and the retry gets the same answer ----------------------------------------
    let (_, again) = post_it(&filled, "key-abc");
    let (code, _) = post_it(&filled, "key-abc");
    assert_eq!(
        parse(&again), parse(&first),
        "a retry with the same Idempotency-Key must return the response the first call got, \
         verbatim. First: {first}\nAgain: {again}"
    );
    assert_eq!(
        code, 201,
        "the retry answered {code}. A 409 tells a caller its request never happened when it did; \
         the point of the key is that a retry is indistinguishable from the original."
    );

    // A DIFFERENT key on an already-posted invoice is the one that must refuse: this is
    // not a retry, it is a second posting, and it must not add a second entry.
    let before = gate.stored("invoice", &filled);
    let (c, _) = post_it(&filled, "key-xyz");
    assert_eq!(
        c, 409,
        "a new key on an already-posted invoice must be 409 already_posted — that is a second \
         charge, not a retry"
    );
    let after = gate.stored("invoice", &filled);
    assert_eq!(before, after, "the refused second posting changed the invoice — the entry must be written exactly once");
}
