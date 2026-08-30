//! The three `treasury:ledger` gates, ported from
//! `components/treasury-ledger-domain/e2e-*.sh`.
//!
//! Assertions and failure sentences unchanged — ADR-0088 makes a gate's output the
//! next prompt a repair reads, and these are among the most carefully written in the
//! repository. Two of them record decisions that cost a run each:
//!
//!   * the contention rounds run THREE times, and the script says why: a contention
//!     assertion is probabilistic in one direction. Correct work is safe under every
//!     interleaving, so this can never fail it — but a broken implementation only
//!     loses money when requests actually overlap, and the same double-spending
//!     candidate was caught directly and slipped through a single round in rehearsal.
//!   * the losers of a contested transfer may answer 409 or 503, and demanding 409
//!     from all fifteen made the gate fail CORRECT work under load.
//!
//! The storms were `xargs -P 16` and `seq 24 | xargs -P`; they are scoped threads.

mod gatelib;
use gatelib::{field, Gate};
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

/// Status codes, counted — `sort | uniq -c` in the shell version.
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

    // --- the refusals ---------------------------------------------------------------
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

    // --- one account, one credit ----------------------------------------------------
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

    // --- and now all at once --------------------------------------------------------
    //
    // A fresh account at zero, twenty-four credits of 1.00 fired in parallel. Every one
    // must be reflected: 2400. A part that surfaces a revision conflict instead of
    // re-reading ends short, and by a different amount every run.
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

    // --- the refusals ---------------------------------------------------------------
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

    // --- one transfer, both sides, and a journal line -------------------------------
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

    // Read through the ROUTER's fixture: `/api/journal` belongs to `reconcile`, a stub
    // while this part is judged. Asking it made this check unsatisfiable by any
    // implementation — and it did, for a whole run.
    let j = parse(&gate.get("/test/journal", None).1);
    let lines = j["lines"].as_array().cloned().unwrap_or_default();
    assert!(!lines.is_empty(), "the journal is empty after a settled transfer: {j}");
    let mine: Vec<&Value> =
        lines.iter().filter(|x| x["from"] == l.as_str() && x["to"] == r.as_str()).collect();
    assert!(!mine.is_empty(), "no journal line names this pair: {lines:?}");
    assert_eq!(mine.last().unwrap()["units"], 2500, "the journal line must carry the amount");

    // --- the retry every client makes ----------------------------------------------
    let (_, again) = keyed("k-one", &l, &r, "25.00");
    assert_eq!(
        parse(&again), parse(&ok),
        "a retry with the same key must answer exactly what the first call answered.\n  first: {ok}\n  again: {again}"
    );
    assert_eq!(units_of(&gate, &l), 7500, "the retry moved money again");

    // --- and now, many at once, each for everything, three times over ---------------
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
        // The losers may answer either way: 409 for "there is no money", 503 for "I could
        // not get a clean read in the attempts I allow myself". Demanding 409 from all
        // fifteen made this gate fail CORRECT work under load, which is worse than
        // missing something.
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

    // --- the refusals ---------------------------------------------------------------
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

    // --- an empty journal agrees with untouched balances ----------------------------
    let d = parse(&run("empty"));
    assert_eq!(d["checked"], 2, "two accounts were given and {} were checked: {d}", d["checked"]);
    assert_eq!(d["balanced"], true, "nothing has moved, so the books balance: {d}");
    assert_eq!(d["drift"], json!([]), "drift must be empty when nothing disagrees: {d}");
    assert_eq!(
        d["journal_lines"], 0,
        "the journal is empty and this reports {}",
        d["journal_lines"]
    );

    // --- a journal the balances do not match ----------------------------------------
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

    // --- the same report twice is the same report ----------------------------------
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

    // --- the journal read route ----------------------------------------------------
    let j = parse(&gate.get("/api/journal?limit=2", Some(&t)).1);
    let lines = j["lines"].as_array().cloned().unwrap_or_default();
    assert_eq!(lines.len(), 2, "?limit=2 answered {lines:?}");
    let ats: Vec<&str> = lines.iter().filter_map(|x| x["at"].as_str()).collect();
    let mut sorted = ats.clone();
    sorted.sort();
    assert_eq!(ats, sorted, "oldest first, and these are not: {ats:?}");
}
