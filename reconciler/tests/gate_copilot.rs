//! The `invoice:copilot` gate, ported from
//! `components/invoice-copilot-domain/e2e-copilot.sh`.
//!
//! The division is the point and the model is not allowed to do it. `money::allocate`
//! splits 100.00 three ways as 3334 + 3333 + 3333; a model asked to divide loses the
//! cent. So the arithmetic is asserted exactly and the model is judged only on the
//! half it should own — memos that are about the work rather than placeholders or a
//! slice of the prose.
//!
//! Verified against `mlx-community/Qwen3.8-27B-4bit` on csatapaci through
//! `just openai-shim`.

mod gatelib;
use gatelib::{field, Gate, Shim};
use serde_json::{json, Value};

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

const PROSE: &str =
    "Two days of discovery workshops with the billing team, and a written summary of what we agreed.";

#[test]
fn a_suggestion_is_allocated_to_the_cent_and_described_by_the_model() {
    let Some(shim) = Shim::probe("invoice/copilot") else { return };
    let config = shim.config();
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("invoice", "invoice-copilot-domain", &cfg, &[&egress])
    else {
        return;
    };

    let (_, tok) = gate.post("/test/token", None, json!({"subject":"biller"}));
    let t = field(&tok, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );

    let seed = gate.seed();
    let draft = seed["invoice_ids"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        !draft.is_empty(),
        "the fixture produced no invoices — the scaffold is broken, not the part"
    );

    let suggest = |id: &str, body: Value| {
        gate.post(&format!("/api/invoices/{id}/lines/suggest"), Some(&t), body)
    };
    let body = json!({"prose": PROSE, "total":"100.00", "shares":3});

    // --- the refusals, none of which costs a model call ----------------------------
    let (c, _) = gate.post(&format!("/api/invoices/{draft}/lines/suggest"), None, body.clone());
    assert_eq!(c, 401, "suggesting with no bearer must be 401");
    let (c, _) = suggest("nope", body.clone());
    assert_eq!(c, 404, "suggesting on an unknown invoice must be 404");
    for (b, why) in [
        (
            json!({"prose": PROSE, "total":"100.00", "shares":1}),
            "one share is not a split — must be 400 invalid_suggestion",
        ),
        (
            json!({"prose": PROSE, "total":"100.00", "shares":99}),
            "99 shares is out of range — must be 400 invalid_suggestion",
        ),
        (
            json!({"prose": PROSE, "total":"not money", "shares":3}),
            "a total money:amount cannot parse must be 400 bad_money",
        ),
    ] {
        let (c, _) = suggest(&draft, b);
        assert_eq!(c, 400, "{why}");
    }

    // --- the split, to the cent ----------------------------------------------------
    let (_, s) = suggest(&draft, body);
    assert!(
        !s.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&s);
    let lines = d["lines"].as_array().cloned().unwrap_or_default();
    assert_eq!(lines.len(), 3, "three shares means three lines: {d}");
    let units: Vec<i64> = lines
        .iter()
        .map(|l| {
            l["units"]
                .as_i64()
                .unwrap_or_else(|| panic!("every amount is an integer in minor units: {l}"))
        })
        .collect();
    assert_eq!(
        units.iter().sum::<i64>(),
        10000,
        "the lines sum to {} minor units and the total was 10000. This is the cent that a model \
         loses when it is asked to divide, and money::allocate is what does not: 100.00 into 3 is \
         3334 + 3333 + 3333.",
        units.iter().sum::<i64>()
    );
    let mut desc = units.clone();
    desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        desc, [3334, 3333, 3333],
        "allocate distributes the remainder to the earliest shares: expected [3334, 3333, 3333], got {units:?}"
    );
    assert_eq!(d["total_units"], 10000, "the stored total must be the allocated sum: {d}");
    assert_eq!(
        d["total"], "100.00",
        "total is money::format of the parsed amount: {:?}",
        d["total"]
    );

    // The model's half: about the prose, and not a slice of it.
    let memos: Vec<String> =
        lines.iter().map(|l| l["memo"].as_str().unwrap_or_default().trim().to_string()).collect();
    assert!(memos.iter().all(|m| !m.is_empty()), "every line needs a description: {memos:?}");
    assert!(
        !memos.iter().all(|m| m.to_lowercase().starts_with("line ")),
        "the memos are placeholders, so no model wrote them: {memos:?}"
    );
    let joined = memos.join(" ").to_lowercase();
    assert!(
        ["workshop", "discovery", "summary", "billing", "agreed", "day"]
            .iter()
            .any(|w| joined.contains(w)),
        "the descriptions are not about the work described: {memos:?}"
    );
    assert!(
        !memos.iter().any(|m| m.eq_ignore_ascii_case(PROSE)),
        "a memo is the whole prose, verbatim"
    );

    let d = parse(&gate.stored("invoice", &draft));
    assert_eq!(
        d["lines"].as_array().map(|a| a.len()),
        Some(3),
        "the lines must be stored on the invoice: {d}"
    );
    assert_eq!(d["total_units"], 10000, "the stored total must be the allocated sum: {d}");
    assert_eq!(d["state"], "draft", "suggesting does not post an invoice: {d}");

    // A second suggestion replaces the lines: it is a draft, not an error.
    let (_, s2) = suggest(
        &draft,
        json!({"prose":"A single day of pair programming.","total":"50.00","shares":2}),
    );
    let d = parse(&s2);
    let units: Vec<i64> = d["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| l["units"].as_i64())
        .collect();
    assert_eq!(units.len(), 2, "two shares means two lines, not five: {d}");
    assert_eq!(units.iter().sum::<i64>(), 5000, "the new total must be the new amount: {units:?}");
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// The whole invoice API: an invoice, lines the model described and `money` split,
/// and a balanced ledger entry posted exactly once.
///
/// Ported from `components/invoice-copilot-domain/e2e.sh`. It lives beside the
/// copilot gate because it needs the same model on the shim.
///
/// The chain is one number: **10.00 into 3 is 3.34 + 3.33 + 3.33**. A part that
/// divides by hand loses a cent here, and the ledger refuses the entry two steps
/// later — so a rounding error in `copilot` surfaces as a balance failure in
/// `posting`, and neither part's own gate can see the other end of that.
///
/// What else only the composition proves: `copilot` stores lines and `posting` reads
/// them, so a `nothing_to_post` here means they disagree about where lines live, and
/// nothing in either part would notice.
#[test]
fn the_whole_invoice_api_works() {
    let Some(shim) = Shim::probe("invoice/whole") else { return };
    let mut config = shim.config();
    config.push("max-attempts=10".into());
    config.push("lockout-window=60".into());
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) =
        Gate::compose_and_start_with_egress("invoice", "invoice-copilot-domain", &cfg, &[&egress])
    else {
        return;
    };

    for (iface, why) in [
        (
            "ratelimit:guard/limiter",
            "the composed API must still be counting invoices through the limiter",
        ),
        ("ai:inference/inference", "the composed API must still be asking the model for the words"),
        (
            "money:amount/arithmetic",
            "the composed API must still be doing its arithmetic in money:amount",
        ),
        (
            "ledger:doubleentry/ledger",
            "the composed API must still be balancing through the ledger",
        ),
        ("idempotency:guard/store", "the composed API must still post exactly once"),
    ] {
        gatelib::requires_capability("invoice-copilot-domain", iface, why);
    }

    let t = field(&gate.post("/test/token", None, json!({"subject":"biller"})).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the parts"
    );

    // --- an invoice, through the part that owns invoices -------------------
    let (_, created) =
        gate.post("/api/invoices", Some(&t), json!({"customer":"acme-gmbh","currency":"EUR"}));
    let id = field(&created, "id");
    assert!(
        !id.is_empty(),
        "the invoices part did not accept an invoice, so nothing else can be judged: {created}"
    );

    // --- lines, through the part that talks to the model -------------------
    let (_, s) = gate.post(
        &format!("/api/invoices/{id}/lines/suggest"),
        Some(&t),
        json!({
            "prose": "An afternoon reviewing the billing migration, and notes on what to change.",
            "total": "10.00",
            "shares": 3,
        }),
    );
    assert!(
        !s.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&s);
    let units: Vec<i64> = d["lines"]
        .as_array()
        .map(|a| a.iter().map(|l| l["units"].as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default();
    assert_eq!(units.len(), 3, "three shares means three lines: {s}");
    let total: i64 = units.iter().sum();
    assert_eq!(
        total, 1000,
        "10.00 into three shares still totals 1000 minor units, got {total}: {units:?}"
    );
    let mut sorted = units.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sorted, vec![334, 333, 333], "expected [334, 333, 333]: {units:?}");

    // --- posted, through the part that owns posting ------------------------
    let (_, p) = gate.with_headers(
        "POST",
        &format!("/api/invoices/{id}/post"),
        Some(&t),
        &[("idempotency-key", "join-key")],
        None,
    );
    let posted = parse(&p);
    let (_, stored) = gate.get(&format!("/test/invoice/{id}"), None);
    let inv = parse(&stored);
    assert!(
        posted.get("error").is_none(),
        "the composed API refused to post an invoice built through its own routes. If this \
         is nothing_to_post, `copilot` (src/copilot.rs) stored lines somewhere `posting` \
         (src/posting.rs) does not read. Got: {p}"
    );
    assert_eq!(
        posted["total_units"], 1000,
        "the posted total is not the allocated total. `copilot` and `posting` disagree about \
         the invoice's shape: {p} vs {stored}"
    );
    let lines = inv["entry"]["lines"].as_array().cloned().unwrap_or_default();
    let side = |want: &str| -> i64 {
        lines
            .iter()
            .filter(|l| l["side"].as_str() == Some(want))
            .map(|l| l["amount"].as_i64().unwrap_or(0))
            .sum()
    };
    let (debits, credits) = (side("debit"), side("credit"));
    assert!(
        debits == 1000 && credits == 1000,
        "the ledger entry does not balance against the allocated total: debits {debits}, \
         credits {credits}, invoice {}",
        inv["total_units"]
    );

    // And the retry a real client would make.
    let (code, _) = gate.with_headers(
        "POST",
        &format!("/api/invoices/{id}/post"),
        Some(&t),
        &[("idempotency-key", "join-key")],
        None,
    );
    assert_eq!(
        code, 201,
        "the retry of a successful posting must answer as the first did (201), got {code}"
    );
}
