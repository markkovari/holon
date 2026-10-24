//! The three `treasury:ledger` gates, ported from `components/treasury-ledger-domain/e2e-*.sh`.
//!
//! Assertions and failure sentences unchanged (ADR-0088). Two record decisions that cost a run each:
//!
//!   * the contention rounds run THREE times: a contention assertion is probabilistic in one direction
//!     (correct work is safe under every interleaving, but a broken implementation only loses money
//!     when requests overlap, and a double-spender once slipped through a single round).
//!   * the losers of a contested transfer may answer 409 or 503; demanding 409 from all fifteen made
//!     the gate fail CORRECT work under load.

mod gatelib;
use gatelib::{field, requires_capability, Gate};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const CRATE: &str = "treasury-ledger-domain";

fn start() -> Option<Gate> {
    Gate::compose_and_start("treasury", CRATE, &[])
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text.trim()).unwrap_or(Value::Null)
}

fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut body = json!({ "subject": subject });
    if let Some(s) = scopes {
        body["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, body).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}

/// The stored balance in minor units, through the scaffold's fixture route.
fn units_of(gate: &Gate, id: &str) -> i64 {
    let raw = gate.stored("account", id);
    parse(&raw)["units"]
        .as_i64()
        .unwrap_or_else(|| panic!("the fixture read answered no units for {id}: {raw}"))
}

/// Two accounts from the fixture, both starting at `start`.
fn pair(gate: &Gate, start: &str) -> (String, String) {
    let seed = gate.post("/test/seed", None, json!({ "start": start })).1;
    let ids: Vec<String> = parse(&seed)["account_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        ids.len() >= 2,
        "the fixture produced no accounts — the scaffold is broken, not the part"
    );
    (ids[0].clone(), ids[1].clone())
}

/// Status codes, counted.
fn tally(codes: &[u16]) -> BTreeMap<u16, usize> {
    let mut m = BTreeMap::new();
    for c in codes {
        *m.entry(*c).or_insert(0) += 1;
    }
    m
}

#[test]
fn accounts_open_credit_and_survive_concurrent_credits() {
    let Some(gate) = start() else { return };
    let w = token(&gate, "treasurer", None);

    let (c, _) = gate.post("/api/accounts", None, json!({"name":"x","currency":"EUR"}));
    assert_eq!(c, 401, "opening an account with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["accounts:read"])));
    let (c, _) = gate.post("/api/accounts", Some(&ro), json!({"name":"x","currency":"EUR"}));
    assert_eq!(c, 403, "a read-only token must be 403 on opening an account");
    for (body, why) in [
        (json!({"name":"","currency":"EUR"}), "an empty name must be 400 invalid_account"),
        (
            json!({"name":"x","currency":"QQQ"}),
            "a currency money:amount does not know must be 400 bad_money",
        ),
        (
            json!({"name":"x","currency":"EUR","start":"1"}),
            "\"1\" is not a EUR amount (parse wants both decimals) — must be 400 bad_money",
        ),
    ] {
        let (c, _) = gate.post("/api/accounts", Some(&w), body);
        assert_eq!(c, 400, "{why}");
    }

    let (_, a) = gate.post(
        "/api/accounts",
        Some(&w),
        json!({"name":"ledger-test","currency":"EUR","start":"10.00"}),
    );
    let id = field(&a, "id");
    assert!(!id.is_empty(), "POST /api/accounts returned no id");
    assert_eq!(
        units_of(&gate, &id),
        1000,
        "an account opened at 10.00 must store 1000 minor units"
    );

    for (amount, code, why) in [
        ("0.00", 400, "a zero credit must be 400 invalid_amount"),
        ("-5.00", 400, "a negative credit must be 400 invalid_amount — that is a transfer with one side missing"),
    ] {
        let (c, _) = gate.post(&format!("/api/accounts/{id}/credit"), Some(&w), json!({"amount": amount}));
        assert_eq!(c, code, "{why}");
    }
    let (c, _) = gate.post("/api/accounts/nope/credit", Some(&w), json!({"amount":"1.00"}));
    assert_eq!(c, 404, "crediting an unknown account must be 404");

    let (_, r) =
        gate.post(&format!("/api/accounts/{id}/credit"), Some(&w), json!({"amount":"2.50"}));
    assert_eq!(parse(&r)["units"], 1250, "10.00 + 2.50 is 1250 minor units: {r}");

    // --- and now all at once ---
    // A fresh account at zero, 24 credits of 1.00 in parallel: every one must be reflected (2400). A
    // part that surfaces a revision conflict instead of re-reading ends short.
    let (_, s) = gate.post(
        "/api/accounts",
        Some(&w),
        json!({"name":"storm","currency":"EUR","start":"0.00"}),
    );
    let storm = field(&s, "id");
    assert!(!storm.is_empty(), "could not open the account for the contention test");

    let credit_storm = |amount: &'static str| -> BTreeMap<u16, usize> {
        let codes: Vec<u16> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..24)
                .map(|_| {
                    sc.spawn(|| {
                        gate.post(
                            &format!("/api/accounts/{storm}/credit"),
                            Some(&w),
                            json!({"amount": amount}),
                        )
                        .0
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("a credit panicked")).collect()
        });
        tally(&codes)
    };

    let codes = credit_storm("1.00");
    let after = units_of(&gate, &storm);
    assert!(
        !codes.contains_key(&409),
        "a credit was answered 409 [{codes:?}]. A revision conflict is the store saying 'read \
         again', not a refusal to show the caller: nobody asked whether the account had changed, \
         they asked to add money to it."
    );
    assert_eq!(
        after,
        2400,
        "twenty-four concurrent credits of 100 minor units left the account at {after}, not 2400 \
         — {} of them vanished. Status codes: [{codes:?}]. Read the conflict and retry from what \
         is there now.",
        (2400 - after) / 100
    );

    // Not a fluke of one run, and not a fluke of an empty account: again, on a balance.
    let codes = credit_storm("0.25");
    let after = units_of(&gate, &storm);
    assert_eq!(
        after, 3000,
        "2400 plus twenty-four credits of 25 is 3000, and the account is at {after}. Codes: [{codes:?}]"
    );
}

#[test]
fn transfers_are_idempotent_and_survive_contention() {
    let Some(gate) = start() else { return };
    let t = token(&gate, "treasurer", None);
    let (l, r) = pair(&gate, "100.00");

    // The key rides on a header, which is why the harness grew `with_headers`.
    let keyed = |key: &str, from: &str, to: &str, amount: &str| -> (u16, String) {
        gate.with_headers(
            "POST",
            "/api/transfers",
            Some(&t),
            &[("idempotency-key", key)],
            Some(json!({"from": from, "to": to, "amount": amount})),
        )
    };

    let (c, _) = gate.with_headers(
        "POST",
        "/api/transfers",
        None,
        &[("idempotency-key", "k")],
        Some(json!({"from": l, "to": r, "amount": "1.00"})),
    );
    assert_eq!(c, 401, "a transfer with no bearer must be 401");

    let (c, _) = gate.post("/api/transfers", Some(&t), json!({"from":l,"to":r,"amount":"1.00"}));
    assert_eq!(c, 400, "a transfer with no Idempotency-Key must be 400 — every client retries");
    let (c, _) = keyed("k-same", &l, &l, "1.00");
    assert_eq!(c, 400, "a transfer to the same account must be 400 same_account");
    let (c, _) = keyed("k-nope", &l, "nope", "1.00");
    assert_eq!(c, 404, "an unknown destination must be 404");

    let (_, ok) = keyed("k-one", &l, &r, "25.00");
    let d = parse(&ok);
    assert!(
        d.get("error").is_none(),
        "a 25.00 transfer between two accounts holding 100.00 was refused: {d}"
    );
    assert_eq!(d["from_units"], 7500, "the source must end at 7500: {d}");
    assert_eq!(d["to_units"], 12500, "the destination must end at 12500: {d}");
    assert!(d.get("transfer").is_some(), "the answer must name the transfer it created: {d}");
    assert_eq!(units_of(&gate, &l), 7500, "the source account's stored balance is wrong");
    assert_eq!(units_of(&gate, &r), 12500, "the destination's stored balance is wrong");

    let tid = field(&ok, "transfer");
    let rec = parse(&gate.get(&format!("/api/transfers/{tid}"), Some(&t)).1);
    assert_eq!(rec["state"], "settled", "a completed transfer is settled: {rec}");
    assert_eq!(rec["units"], 2500, "the recorded amount must be the amount moved: {rec}");

    // Read through the ROUTER's fixture: `/api/journal` belongs to `reconcile`, a stub while this part is
    // judged, which made the check unsatisfiable by any implementation.
    let j = parse(&gate.get("/test/journal", None).1);
    let lines = j["lines"].as_array().cloned().unwrap_or_default();
    assert!(!lines.is_empty(), "the journal is empty after a settled transfer: {j}");
    let mine: Vec<&Value> =
        lines.iter().filter(|x| x["from"] == l.as_str() && x["to"] == r.as_str()).collect();
    assert!(!mine.is_empty(), "no journal line names this pair: {lines:?}");
    assert_eq!(mine.last().unwrap()["units"], 2500, "the journal line must carry the amount");

    let (_, again) = keyed("k-one", &l, &r, "25.00");
    assert_eq!(
        parse(&again), parse(&ok),
        "a retry with the same key must answer exactly what the first call answered.\n  first: {ok}\n  again: {again}"
    );
    assert_eq!(units_of(&gate, &l), 7500, "the retry moved money again");

    for round in 1..=3 {
        let (a, z) = pair(&gate, "60.00");
        let before = units_of(&gate, &a) + units_of(&gate, &z);
        let codes: Vec<u16> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..16)
                .map(|n| {
                    let key = format!("storm-{round}-{n}");
                    let (a, z) = (a.clone(), z.clone());
                    sc.spawn(move || keyed(&key, &a, &z, "60.00").0)
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("a contesting transfer panicked")).collect()
        });
        let counts = tally(&codes);
        let (fa, fz) = (units_of(&gate, &a), units_of(&gate, &z));
        let after = fa + fz;

        assert!(fa >= 0 && fz >= 0, "round {round}: an account went negative: from={fa} to={fz}");
        assert_eq!(
            before, after,
            "round {round}: the two accounts held {before} minor units before sixteen simultaneous \
             transfers and {after} after. Money was created or destroyed. Codes: [{counts:?}]"
        );
        let settled = *counts.get(&201).unwrap_or(&0);
        assert_eq!(
            settled, 1,
            "round {round}: {settled} of sixteen transfers of the ENTIRE balance succeeded. Exactly \
             one can: the comparison and the write have to be one CAS on the same revision, or \
             every request decides against a balance that is already gone. Codes: [{counts:?}]"
        );
        assert!(
            fa == 0 && fz == 12000,
            "round {round}: after the one settlement the source is empty and the destination holds \
             both: from={fa} to={fz}"
        );
        // The losers may answer 409 ("no money") or 503 ("no clean read in the attempts allowed");
        // demanding 409 from all fifteen failed CORRECT work under load.
        let strange: BTreeMap<u16, usize> = counts
            .iter()
            .filter(|(c, _)| ![201u16, 409, 503].contains(c))
            .map(|(c, n)| (*c, *n))
            .collect();
        assert!(
            strange.is_empty(),
            "round {round}: {strange:?} — a loser must be refused for no money (409) or for \
             contention (503), and a caller cannot tell what to do with anything else: [{counts:?}]"
        );
        assert!(
            *counts.get(&409).unwrap_or(&0) >= 1,
            "round {round}: not one request was refused with 409 insufficient_funds, so nothing \
             actually compared a balance: [{counts:?}]"
        );
    }
}

#[test]
fn a_transfer_counts_once_in_the_journal_even_with_a_duplicate_line() {
    let Some(gate) = start() else { return };
    let w = token(&gate, "treasurer", None);
    let (l, r) = pair(&gate, "50.00");
    let (c, sent) = gate.with_headers(
        "POST",
        "/api/transfers",
        Some(&w),
        &[("idempotency-key", "once")],
        Some(json!({"from": l, "to": r, "amount": "10.00"})),
    );
    assert_eq!(c, 201, "{sent}");
    let transfer = field(&sent, "transfer");
    // The line a racing write could have left behind: the same transfer, written down again.
    gate.post(
        "/test/journal",
        None,
        json!({"transfer": transfer, "from": l, "to": r, "units": 1000}),
    );
    let (_, report) = gate.with_headers(
        "POST",
        "/api/reconcile",
        Some(&w),
        &[("idempotency-key", "count-once")],
        Some(json!({"opened": [{"account": l, "units": 5000}, {"account": r, "units": 5000}]})),
    );
    let d = parse(&report);
    assert_eq!(d["journal_lines"], 1, "one transfer is one line: {report}");
    assert_eq!(d["balanced"], true, "a duplicate line moved money on paper: {report}");
}

#[test]
fn reconcile_reads_the_journal_and_is_idempotent() {
    let Some(gate) = start() else { return };
    let t = token(&gate, "auditor", None);
    let (l, r) = pair(&gate, "50.00");

    let line = |from: &str, to: &str, units: i64| {
        gate.post("/test/journal", None, json!({"from": from, "to": to, "units": units}));
    };
    let opened = json!([{"account": l, "units": 5000}, {"account": r, "units": 5000}]);
    let run = |key: &str| -> String {
        gate.with_headers(
            "POST",
            "/api/reconcile",
            Some(&t),
            &[("idempotency-key", key)],
            Some(json!({"opened": opened})),
        )
        .1
    };

    let (c, _) = gate.with_headers(
        "POST",
        "/api/reconcile",
        None,
        &[("idempotency-key", "k")],
        Some(json!({"opened": opened})),
    );
    assert_eq!(c, 401, "reconciling with no bearer must be 401");
    let (c, _) = gate.post("/api/reconcile", Some(&t), json!({"opened": opened}));
    assert_eq!(c, 400, "a reconciliation with no Idempotency-Key must be 400");

    let d = parse(&run("empty"));
    assert_eq!(d["checked"], 2, "two accounts were given and {} were checked: {d}", d["checked"]);
    assert_eq!(d["balanced"], true, "nothing has moved, so the books balance: {d}");
    assert_eq!(d["drift"], json!([]), "drift must be empty when nothing disagrees: {d}");
    assert_eq!(
        d["journal_lines"], 0,
        "the journal is empty and this reports {}",
        d["journal_lines"]
    );

    line(&l, &r, 1000);
    line(&l, &r, 1000);
    let d = parse(&run("drifted"));
    assert_eq!(
        d["journal_lines"], 2,
        "two journal lines were written and this read {}: {d}",
        d["journal_lines"]
    );
    assert_eq!(
        d["balanced"], false,
        "the journal says 20.00 moved and neither balance changed, and this reports the books as \
         balanced. A reconciliation that does not read the journal is worse than none: {d}"
    );
    let drift: BTreeMap<String, Value> = d["drift"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| x["account"].as_str().map(|a| (a.to_string(), x.clone())))
        .collect();
    assert_eq!(
        drift.keys().cloned().collect::<Vec<_>>(),
        {
            let mut v = vec![l.clone(), r.clone()];
            v.sort();
            v
        },
        "both accounts drifted and the report names {:?}: {d}",
        drift.keys().collect::<Vec<_>>()
    );
    // left: opened 5000, journal says -2000, so expected 3000; stored is still 5000.
    assert_eq!(drift[&l]["expected"], 3000, "left expected 5000-2000=3000: {}", drift[&l]);
    assert_eq!(drift[&l]["actual"], 5000, "left's stored balance is 5000: {}", drift[&l]);
    assert_eq!(
        drift[&l]["delta"], 2000,
        "left holds 2000 more than the journal justifies: {}",
        drift[&l]
    );
    assert_eq!(drift[&r]["expected"], 7000, "right expected 5000+2000=7000: {}", drift[&r]);
    assert_eq!(
        drift[&r]["delta"], -2000,
        "right holds 2000 less than the journal justifies: {}",
        drift[&r]
    );

    let (one, two) = (run("twice"), run("twice"));
    assert_eq!(
        parse(&one), parse(&two),
        "a report is a report: running it again under the same key must answer the same thing.\n  {one}\n  {two}"
    );

    // A different key sees the world as it is now — one more line, one more finding.
    line(&l, &r, 500);
    let d = parse(&run("fresh"));
    assert_eq!(
        d["journal_lines"], 3,
        "three lines exist now and this read {}: {d}",
        d["journal_lines"]
    );

    let j = parse(&gate.get("/api/journal?limit=2", Some(&t)).1);
    let lines = j["lines"].as_array().cloned().unwrap_or_default();
    assert_eq!(lines.len(), 2, "?limit=2 answered {lines:?}");
    let ats: Vec<&str> = lines.iter().filter_map(|x| x["at"].as_str()).collect();
    let mut sorted = ats.clone();
    sorted.sort();
    assert_eq!(ats, sorted, "oldest first, and these are not: {ats:?}");
}

/// `?limit=` is "the oldest `limit` lines", not "the first `limit` the store lists, sorted". The two
/// agree whenever `at` matches write order, so the fixture takes an explicit `at` to tell them apart.
#[test]
fn journal_limit_answers_the_oldest_lines() {
    let Some(gate) = start() else { return };
    let t = token(&gate, "treasurer", None);
    let (l, r) = pair(&gate, "100.00");
    // Written in this order; listed in this order; NOT oldest-first.
    for at in ["2030-01-01T00:00:00Z", "2020-01-01T00:00:00Z", "2025-01-01T00:00:00Z"] {
        let (c, b) =
            gate.post("/test/journal", None, json!({"from": l, "to": r, "units": 1, "at": at}));
        assert_eq!(c, 201, "the journal fixture refused a line: {b}");
    }
    let j = parse(&gate.get("/api/journal?limit=2", Some(&t)).1);
    let ats: Vec<&str> = j["lines"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["at"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        ats,
        vec!["2020-01-01T00:00:00Z", "2025-01-01T00:00:00Z"],
        "?limit=2 must answer the two OLDEST lines, oldest first. Taking the first two the \
         store lists and sorting those answers 2020 and 2030 — ordered, and the wrong page: {j}"
    );
}

// the composition: the gate no single part can pass

/// The whole treasury, through the real routes, with the auditor checking the mover.
///
/// Ported from `components/treasury-ledger-domain/e2e.sh`. Only the composition can prove that
/// `reconcile` recomputes both balances from the journal ALONE: it was written against the same
/// contract by an agent that never saw `transfers`, so a disagreement about the journal's shape (a
/// field name, a direction, a unit) shows only here.
///
/// The transfer storm MIRRORS `transfers_are_idempotent_and_survive_contention`: there ten transfers
/// contend for a balance only one can have ("exactly one lands"); here all ten fit ("all land").
#[test]
fn the_whole_treasury_api_works() {
    let Some(gate) = start() else { return };

    requires_capability(
        CRATE,
        "auth:identity/authorizer",
        "resolving a bearer token is a solved problem in this repository and `authorize` does \
         the verification and the permission check in one call — parsing a token by hand is how \
         this part fails",
    );
    for (iface, why) in [
        (
            "records:store/store",
            "the composed API must still be serialising its debits on the store's revision",
        ),
        (
            "money:amount/arithmetic",
            "the composed API must still do its arithmetic in money:amount",
        ),
        (
            "ledger:doubleentry/ledger",
            "the composed API must still balance its journal through the ledger",
        ),
        ("idempotency:guard/store", "the composed API must still be exactly-once"),
        ("fsm:workflow/engine", "the composed API must still drive the transfer lifecycle"),
    ] {
        requires_capability(CRATE, iface, why);
    }

    let w = token(&gate, "treasurer", None);
    let open = |name: &str| {
        let (_, a) = gate.post(
            "/api/accounts",
            Some(&w),
            json!({"name": name, "currency": "EUR", "start": "0.00"}),
        );
        field(&a, "id")
    };
    let (a, z) = (open("join-a"), open("join-b"));
    assert!(
        !a.is_empty() && !z.is_empty(),
        "the accounts part did not open an account, so nothing else can be judged"
    );

    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..20)
            .map(|_| {
                sc.spawn(|| {
                    gate.post(
                        &format!("/api/accounts/{a}/credit"),
                        Some(&w),
                        json!({"amount": "5.00"}),
                    )
                    .0
                })
            })
            .collect();
        for h in hs {
            h.join().expect("a credit panicked");
        }
    });
    let funded = units_of(&gate, &a);
    assert_eq!(
        funded, 10000,
        "twenty concurrent credits of 5.00 should fund the account with 10000 minor units, \
         it holds {funded}"
    );

    // --- move it across in ten concurrent transfers ---
    // The same borrow-capturing closure the transfers gate uses: it holds only references, so it is
    // `Copy` and a `move` thread body copies it rather than taking the `Gate`.
    let keyed = |key: &str| -> u16 {
        gate.with_headers(
            "POST",
            "/api/transfers",
            Some(&w),
            &[("idempotency-key", key)],
            Some(json!({"from": a, "to": z, "amount": "10.00"})),
        )
        .0
    };
    let codes: Vec<u16> = std::thread::scope(|sc| {
        let hs: Vec<_> = (1..=10)
            .map(|i| {
                let key = format!("join-{i}");
                sc.spawn(move || keyed(&key))
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("a transfer panicked")).collect()
    });
    let counted = tally(&codes);
    let (from, to) = (units_of(&gate, &a), units_of(&gate, &z));
    assert_eq!(
        from + to,
        10000,
        "the pair held 10000 and now holds {}. Codes: {counted:?}",
        from + to
    );
    assert!(
        from == 0 && to == 10000,
        "ten transfers of 10.00 out of 100.00 all fit, so the source should be empty and the \
         destination full: from={from} to={to}. Codes: {counted:?}"
    );

    let (_, report) = gate.with_headers(
        "POST",
        "/api/reconcile",
        Some(&w),
        &[("idempotency-key", "join-audit")],
        Some(json!({"opened": [
            {"account": a, "units": 10000},
            {"account": z, "units": 0},
        ]})),
    );
    assert!(!report.trim().is_empty(), "the reconcile route answered an empty body");
    let d = parse(&report);
    assert!(
        d.get("error").is_none(),
        "the reconcile part refused a report over accounts moved through the real routes. If \
         this is a store or shape error, the two parts disagree about the `journal` \
         collection: {report}"
    );
    // Seen once in CI (13 lines, balances right) and never in ~200 local runs: say what the lines were.
    let lines = d["journal_lines"].as_i64().unwrap_or(-1);
    if lines != 10 {
        let journal = parse(&gate.get("/api/journal?limit=500", Some(&w)).1);
        let mut by_transfer: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for l in journal["lines"].as_array().into_iter().flatten() {
            by_transfer.entry(l["transfer"].as_str().unwrap_or("?").to_string()).or_default().push(
                format!(
                    "{} at {}",
                    l["id"].as_str().unwrap_or("?"),
                    l["at"].as_str().unwrap_or("?")
                ),
            );
        }
        let twice: Vec<_> = by_transfer.iter().filter(|(_, ids)| ids.len() > 1).collect();
        panic!(
            "ten transfers settled and the journal has {lines} lines, naming {} distinct transfers. \
             Journalled more than once: {twice:#?}. Transfer codes: {counted:?}. Report: {report}",
            by_transfer.len()
        );
    }
    assert_eq!(
        lines, 10,
        "ten transfers settled and the journal has {} lines. `transfers` writes them and \
         `reconcile` reads them, and nothing else in the app would notice if they disagreed \
         about the collection or the field names.",
        d["journal_lines"]
    );
    assert_eq!(
        d["balanced"],
        json!(true),
        "the recomputation from the journal disagrees with the stored balances. Every unit \
         that moved was journalled, or it was not: {report}"
    );
    assert_eq!(d["drift"], json!([]), "drift must be empty when the books agree: {report}");
}

/// The token fixture is a gate's, not a deployment's: without `test-tokens=on` nobody can mint a
/// token with whatever roles they ask for.
#[test]
fn the_token_fixture_is_closed_unless_a_gate_opens_it() {
    let Some(gate) = Gate::compose_and_start("treasury", CRATE, &["test-tokens=off"]) else {
        return;
    };
    let (code, body) = gate.post("/test/token", None, json!({ "subject": "x", "roles": ["admin"] }));
    assert_eq!(code, 404, "an app that does not ask for the fixture must not mint: {body}");
}
