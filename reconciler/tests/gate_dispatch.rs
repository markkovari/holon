//! The four `dispatch` gates: one per part, and one only the composition can pass.
//!
//! Written as Rust from the start rather than ported from shell, which is the point
//! of #180-#189 having happened first.
//!
//! Every gate judges two different things, because one of them is not enough:
//!
//!   * BEHAVIOUR, because compiling proves nothing. `cargo component check` and
//!     `cargo component build` both succeed on a crate that implements none of its
//!     world — measured twice, while running a real goal. So these start the
//!     component and ask it for things.
//!   * COMPOSITION, because a hand-rolled `@`-finder and a real PII scanner both
//!     answer 201 on a well-behaved body, and a hand-rolled haversine and `geo`
//!     both answer a plausible number of metres. The component's IMPORTS tell them
//!     apart, and `requires_capability` reads them out of the compiled binary.
//!
//! What is deliberately NOT asserted anywhere here: that any particular distance in
//! metres is geometrically correct. `geo` has its own tests and this is not a second
//! copy of them. What these assert is that the number is in the right ORDER OF
//! MAGNITUDE — which is what a degrees-for-radians or a missing-cosine bug breaks —
//! and, in `the_whole_dispatch_api_works`, that `schedule` and `manifest` agree on
//! it. Two parts that each hand-roll the formula can be internally consistent and
//! disagree with each other, and that is the one failure three independent parts can
//! produce and no single part's gate can see.

mod gatelib;
use gatelib::{field, requires_capability, Gate};
use serde_json::{json, Value};

const CRATE: &str = "dispatch-domain";

fn start() -> Option<Gate> {
    Gate::compose_and_start("dispatch", CRATE, &[])
}

/// The three request ids the fixture seeds, in the order it wrote them.
///
/// Read by index rather than by `mapfile`'s shell equivalent, and named here so a
/// gate reads `boiler` instead of `ids[0]` three assertions later.
fn seeded(gate: &Gate) -> (String, String, String) {
    let seed = gate.seed();
    let ids: Vec<String> = seed["request_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        ids.len() >= 3,
        "the fixture did not seed three requests — POST /test/seed answered: {seed}"
    );
    (ids[0].clone(), ids[1].clone(), ids[2].clone())
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The document as STORED, through the router's scaffold route.
///
/// Not `GET /api/requests/{id}`: that belongs to `requests` and is a stub while the
/// other two parts are judged alone, so a gate that read it back that way would
/// blame `schedule` for a route `schedule` does not own.
fn stored(gate: &Gate, id: &str) -> Value {
    let (status, body) = gate.get(&format!("/test/request/{id}"), None);
    assert_eq!(status, 200, "the stored document for {id} was not readable: {body}");
    parse(&body)
}

/// A CSV reader, because a test that needs a parser to state its claim is testing
/// the parser. Handles the one case the gate turns on: a quoted field with a comma.
fn rows(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (mut row, mut cur, mut quoted) = (Vec::new(), String::new(), false);
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    cur.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => row.push(std::mem::take(&mut cur)),
                other => cur.push(other),
            }
        }
        row.push(cur);
        out.push(row);
    }
    out
}

/// `distance_m` off a document or a response, as an integer.
fn distance(v: &Value) -> i64 {
    v["distance_m"].as_i64().unwrap_or(-1)
}

// ---------------------------------------------------------------------------
// part 1 — requests
// ---------------------------------------------------------------------------

#[test]
fn requests_masks_validates_and_deduplicates() {
    let Some(gate) = start() else { return };

    // The notes carry an email, which is the point: the contract says what is STORED
    // is masked, so the raw address must not come back out.
    let body = json!({
        "title": "Radiator cold",
        "lat": 47.4979, "lon": 19.0402,
        "notes": "reach me at ada@example.test any time",
    });
    let (status, resp) = gate.post("/api/requests", None, body.clone());
    assert_eq!(status, 201, "POST /api/requests did not answer 201: {resp}");
    let id = field(&resp, "id");
    assert!(!id.is_empty(), "POST /api/requests returned no id: {resp}");

    let doc = stored(&gate, &id);
    let notes = doc["notes"].as_str().unwrap_or_default();
    assert!(
        !notes.contains("ada@example.test"),
        "the caller's email was stored verbatim — it must be masked: {doc}"
    );
    assert!(
        notes.contains("[EMAIL]"),
        "the notes were not masked with pii:redact's placeholder: {doc}"
    );
    assert_eq!(doc["state"], "new", "a new request starts in `new`: {doc}");
    assert_eq!(doc["engineer"], "", "a new request has no engineer: {doc}");
    assert_eq!(distance(&doc), 0, "a new request has no distance yet: {doc}");
    assert!(
        doc["created"].as_str().unwrap_or_default().ends_with('Z'),
        "`created` is RFC3339 UTC: {doc}"
    );

    // The duplicate rule: same title AND same point, on a request that is not
    // finished.
    let (status, dup) = gate.post("/api/requests", None, body.clone());
    assert_eq!(status, 409, "the same title at the same point is a duplicate: {dup}");
    assert_eq!(field(&dup, "existing"), id, "a 409 names the request that blocks it: {dup}");

    // A blank title is invalid; an out-of-range coordinate is a different error, and
    // the contract distinguishes them because the caller can fix only one of them.
    let (status, body_out) =
        gate.post("/api/requests", None, json!({"title":"", "lat":47.0, "lon":19.0}));
    assert_eq!(status, 400, "a blank title is invalid: {body_out}");
    assert_eq!(field(&body_out, "error"), "invalid", "wrong error code: {body_out}");

    let (status, body_out) =
        gate.post("/api/requests", None, json!({"title":"Off the map", "lat":91.0, "lon":19.0}));
    assert_eq!(status, 400, "latitude 91 is not a coordinate: {body_out}");
    assert_eq!(
        field(&body_out, "error"),
        "bad_coordinate",
        "an out-of-range coordinate is `bad_coordinate`, not `invalid`: {body_out}"
    );

    // The list, and the filter on it.
    let (status, list) = gate.get("/api/requests", None);
    assert_eq!(status, 200, "GET /api/requests did not answer: {list}");
    let all = parse(&list);
    let n = all["requests"].as_array().map(Vec::len).unwrap_or(0);
    assert!(n >= 1, "the list did not contain the request just created: {list}");

    let (_, filtered) = gate.get("/api/requests?state=new", None);
    let only_new = parse(&filtered);
    let states: Vec<&str> = only_new["requests"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["state"].as_str()).collect())
        .unwrap_or_default();
    assert!(!states.is_empty(), "?state=new returned nothing at all: {filtered}");
    assert!(
        states.iter().all(|s| *s == "new"),
        "?state=new returned other states — `query` wants the JSON ENCODING of the value, \
         so `new` is indexed as `\"new\"`, quotes included: {filtered}"
    );

    let (status, missing) = gate.get("/api/requests/nope", None);
    assert_eq!(status, 404, "an unknown id is 404: {missing}");

    requires_capability(
        CRATE,
        "pii:redact/redactor",
        "masking a caller's details is a solved problem in this repository and that \
         capability is in the world for this part to USE, not to reimplement — a \
         hand-written scanner answers 201 on a well-behaved body and the gate cannot \
         tell it apart any other way (see CONTRACT.md)",
    );
}

// ---------------------------------------------------------------------------
// part 2 — schedule
// ---------------------------------------------------------------------------

#[test]
fn schedule_assigns_the_nearest_and_refuses_illegal_moves() {
    let Some(gate) = start() else { return };
    let (boiler, lift, _meter) = seeded(&gate);

    // `boiler` sits about a kilometre due north of `cili` and several kilometres from
    // the other two, so the nearest engineer is not a close call — which is
    // deliberate: this asserts the CHOICE, not the arithmetic.
    let (status, assigned) = gate.post(&format!("/api/requests/{boiler}/assign"), None, json!({}));
    assert_eq!(status, 200, "assigning a `new` request did not answer 200: {assigned}");
    let doc = parse(&assigned);
    assert_eq!(
        doc["engineer"], "cili",
        "`cili` is about 1 km from that point and the others are 2.5 km and 5.9 km — \
         the nearest engineer by great-circle distance is `cili`: {assigned}"
    );
    let d = distance(&doc);
    assert!(
        (900..=1100).contains(&d),
        "the distance to `cili` is about 1000 m; {d} m is the wrong ORDER of magnitude, \
         which is what degrees-for-radians or a dropped cosine looks like. This gate \
         does not assert the exact metre — `geo` has its own tests — only that this is \
         a distance and not a coordinate difference: {assigned}"
    );

    // The document, not just the response: the contract says the document carries
    // `state`, so a transition has to move the fsm instance AND the document.
    let stored_doc = stored(&gate, &boiler);
    assert_eq!(stored_doc["state"], "assigned", "the document's state did not move: {stored_doc}");
    assert_eq!(stored_doc["engineer"], "cili", "the engineer was not written down: {stored_doc}");
    assert_eq!(
        distance(&stored_doc),
        d,
        "the response and the stored document disagree about the distance: {stored_doc}"
    );

    // Assigning twice is not legal, and the 409 says what state it is in NOW —
    // which `fsm:workflow`'s `IllegalTransition` already carries.
    let (status, again) = gate.post(&format!("/api/requests/{boiler}/assign"), None, json!({}));
    assert_eq!(status, 409, "assigning an already-assigned request is illegal: {again}");
    assert_eq!(field(&again, "error"), "illegal_transition", "wrong error code: {again}");
    assert_eq!(
        field(&again, "state"),
        "assigned",
        "the 409 must report the state the request is in now: {again}"
    );

    // The lifecycle, forwards.
    let (status, departed) =
        gate.post(&format!("/api/requests/{boiler}/transition"), None, json!({"event":"depart"}));
    assert_eq!(status, 200, "`depart` from `assigned` is legal: {departed}");
    assert_eq!(parse(&departed)["state"], "enroute", "`depart` goes to `enroute`: {departed}");

    let (status, twice) =
        gate.post(&format!("/api/requests/{boiler}/transition"), None, json!({"event":"depart"}));
    assert_eq!(status, 409, "`depart` from `enroute` is not legal: {twice}");
    assert_eq!(field(&twice, "state"), "enroute", "the 409 must report the current state: {twice}");

    let (status, bogus) =
        gate.post(&format!("/api/requests/{boiler}/transition"), None, json!({"event":"teleport"}));
    assert_eq!(status, 400, "an event that is not in the contract is invalid: {bogus}");
    assert_eq!(field(&bogus, "error"), "invalid", "wrong error code: {bogus}");

    // `cancel` is legal from `new`, and terminal.
    let (status, cancelled) =
        gate.post(&format!("/api/requests/{lift}/transition"), None, json!({"event":"cancel"}));
    assert_eq!(status, 200, "`cancel` from `new` is legal: {cancelled}");
    assert_eq!(parse(&cancelled)["state"], "cancelled", "`cancel` goes to `cancelled`");

    let (status, after) =
        gate.post(&format!("/api/requests/{lift}/transition"), None, json!({"event":"depart"}));
    assert_eq!(status, 409, "`cancelled` is terminal — nothing leaves it: {after}");

    // The queue: open work only, ordered by distance, unassigned first.
    let (status, queue) = gate.get("/api/queue", None);
    assert_eq!(status, 200, "GET /api/queue did not answer: {queue}");
    let q = parse(&queue);
    let entries = q["queue"].as_array().cloned().unwrap_or_default();
    assert!(!entries.is_empty(), "the queue was empty with open requests in the store: {queue}");
    assert!(
        entries.iter().all(|e| e["state"] != "cancelled" && e["state"] != "done"),
        "the queue must not carry finished work — `{lift}` was cancelled: {queue}"
    );
    let ds: Vec<i64> = entries.iter().map(distance).collect();
    assert!(
        ds.windows(2).all(|w| w[0] <= w[1]),
        "the queue is ordered by distance_m ascending; got {ds:?}: {queue}"
    );

    let (status, missing) = gate.post("/api/requests/nope/assign", None, json!({}));
    assert_eq!(status, 404, "assigning an unknown request is 404: {missing}");

    requires_capability(
        CRATE,
        "fsm:workflow/engine",
        "the lifecycle is a DEFINITION, not a ladder of string comparisons — \
         `fsm-workflow` is in this part's world and its `IllegalTransition` already \
         carries the current state the 409 has to report (see CONTRACT.md)",
    );
    requires_capability(
        CRATE,
        "geo:resolve/coords",
        "the distance is not this part's to compute — `geo` is in the world, and \
         `manifest` imports the SAME component to answer `within_m`. A hand-rolled \
         haversine here is internally consistent, passes this gate, and disagrees with \
         a sibling that used the component (see CONTRACT.md)",
    );
}

// ---------------------------------------------------------------------------
// part 3 — manifest
// ---------------------------------------------------------------------------

#[test]
fn manifest_counts_quotes_and_filters_by_radius() {
    let Some(gate) = start() else { return };
    let (_boiler, _lift, _meter) = seeded(&gate);

    let (status, body) = gate.get("/api/manifest", None);
    assert_eq!(status, 200, "GET /api/manifest did not answer: {body}");
    let m = parse(&body);
    assert_eq!(m["total"], 3, "the fixture seeds three requests: {body}");
    assert_eq!(m["by_state"]["new"], 2, "two of the three are `new`: {body}");
    assert_eq!(m["by_state"]["assigned"], 1, "one of the three is `assigned`: {body}");
    assert!(
        m["by_state"].get("done").is_none() && m["by_state"].get("enroute").is_none(),
        "`by_state` carries only the states that OCCUR — no zero-filling: {body}"
    );
    assert_eq!(m["by_engineer"]["cili"], 1, "one request is assigned, to `cili`: {body}");
    assert!(
        m["by_engineer"].as_object().map(|o| o.len()) == Some(1),
        "an unassigned request contributes to no engineer — `\"\"` is not a key: {body}"
    );
    assert_eq!(
        m["total_distance_m"], 557,
        "`total_distance_m` is the sum of every `distance_m`, and the fixture's one \
         assigned request carries 557 while the two `new` ones carry 0: {body}"
    );

    // Nothing here is a 404. A day with no work is a manifest of zero, and a caller
    // that has to tell "no work" from "route is broken" cannot do it from a 404.
    let (status, csv) = gate.get("/api/manifest.csv", None);
    assert_eq!(status, 200, "GET /api/manifest.csv did not answer: {csv}");
    let (_, ct, _) = gate.bytes("/api/manifest.csv", None);
    assert!(
        ct.contains("text/csv"),
        "the CSV must be served as `text/csv`, not as JSON — look at what `Reply` in \
         src/lib.rs can answer with. Got: {ct}"
    );

    let parsed = rows(&csv);
    assert_eq!(parsed.len(), 4, "a header row and one row per request: {csv}");
    assert_eq!(
        parsed[0],
        vec!["id", "title", "state", "engineer", "distance_m"],
        "the header is the contract's, in that order: {csv}"
    );
    for (i, r) in parsed.iter().enumerate() {
        assert_eq!(
            r.len(),
            5,
            "row {i} has {} columns rather than 5 — a title containing a comma has to \
             come back QUOTED, which is what `csv:codec`'s `format` is for: {csv}",
            r.len()
        );
    }
    let titles: Vec<&str> = parsed[1..].iter().map(|r| r[1].as_str()).collect();
    assert!(
        titles.contains(&"Boiler leaking, badly"),
        "the title with a comma in it did not survive the round trip intact — the \
         parser read it as {titles:?} from: {csv}"
    );

    // The radius filter. `cili` is the centre; the seeded `assigned` request is about
    // 557 m away and `boiler` about 1 km, while `lift` is roughly 7 km north. 2000 m
    // is nowhere near any of those boundaries on purpose — a gate that turns on a
    // point being 1000 m from a 1000 m radius is testing floating point.
    let (status, near) =
        gate.get("/api/manifest?near_lat=47.4700&near_lon=19.0600&within_m=2000", None);
    assert_eq!(status, 200, "the radius filter did not answer: {near}");
    let n = parse(&near);
    assert_eq!(
        n["total"], 2,
        "two of the three seeded requests are within 2 km of `cili` (about 557 m and \
         about 1 km); the third is roughly 7 km away. A bounding box alone is a cheap \
         PRE-filter and not the answer — its corners are further from the centre than \
         the radius, so `contains` then `distance_meters` is the pair: {near}"
    );

    // All three or none: a half-specified filter is not a filter.
    let (_, half) = gate.get("/api/manifest?near_lat=47.4700&within_m=2000", None);
    assert_eq!(
        parse(&half)["total"],
        3,
        "`near_lon` is missing, so there is no filter and every request counts: {half}"
    );

    let (status, bad) =
        gate.get("/api/manifest?near_lat=47.4700&near_lon=19.0600&within_m=soon", None);
    assert_eq!(status, 400, "a radius that is not a number is invalid: {bad}");
    assert_eq!(field(&bad, "error"), "invalid", "wrong error code: {bad}");

    requires_capability(
        CRATE,
        "csv:codec/codec",
        "the counting is this part's and the CSV is not — `csv` is in the world, and \
         the gate reads the compiled component's imports, so formatting it by hand \
         fails even when the quoting happens to come out right (see CONTRACT.md)",
    );
    requires_capability(
        CRATE,
        "geo:resolve/coords",
        "the radius is not this part's to compute — `geo` is in the world, and \
         `schedule` imports the SAME component to pick an engineer. Two hand-rolled \
         haversines agree with nothing, including each other (see CONTRACT.md)",
    );
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// One request driven the whole way through all three parts.
///
/// `requests` takes it in and masks it, `schedule` assigns it and moves it, and
/// `manifest` reports it — so a part that stored its own shape passes its own gate
/// and fails here. The last two assertions are the ones that need all three: the
/// distance `schedule` computed has to be the distance `manifest` prints, and the
/// request `schedule` placed has to fall inside a radius `manifest` measures with the
/// same component. Two independently hand-rolled formulas can each be self-consistent
/// and still fail both.
#[test]
fn the_whole_dispatch_api_works() {
    let Some(gate) = start() else { return };
    let (_boiler, _lift, _meter) = seeded(&gate);

    // Near `bela`, and about 200 m from it — far from the other two.
    let (status, created) = gate.post(
        "/api/requests",
        None,
        json!({
            "title": "Alarm panel dead",
            "lat": 47.5300, "lon": 19.0440,
            "notes": "site contact is bela@example.test",
        }),
    );
    assert_eq!(status, 201, "the request was not accepted: {created}");
    let id = field(&created, "id");
    assert!(!id.is_empty(), "no id came back: {created}");

    // `requests` did its half: readable through its own route, and masked.
    let (status, read_back) = gate.get(&format!("/api/requests/{id}"), None);
    assert_eq!(status, 200, "GET /api/requests/{{id}} did not answer: {read_back}");
    assert!(
        !read_back.contains("bela@example.test"),
        "the site contact's email survived into the store: {read_back}"
    );

    // `schedule` did its half.
    let (status, assigned) = gate.post(&format!("/api/requests/{id}/assign"), None, json!({}));
    assert_eq!(status, 200, "assign did not answer 200: {assigned}");
    let doc = parse(&assigned);
    assert_eq!(
        doc["engineer"], "bela",
        "that point is about 200 m from `bela` and kilometres from the others: {assigned}"
    );
    let d = distance(&doc);
    assert!(
        (100..=400).contains(&d),
        "the distance to `bela` is about 200 m; {d} m is the wrong order of magnitude: {assigned}"
    );

    let (status, departed) =
        gate.post(&format!("/api/requests/{id}/transition"), None, json!({"event":"depart"}));
    assert_eq!(status, 200, "`depart` was refused: {departed}");

    // `manifest` did its half, over what the other two wrote.
    let (_, body) = gate.get("/api/manifest", None);
    let m = parse(&body);
    assert_eq!(m["total"], 4, "three seeded plus the one just created: {body}");
    assert_eq!(m["by_state"]["enroute"], 1, "the new request is `enroute`: {body}");
    assert_eq!(m["by_engineer"]["bela"], 1, "it is assigned to `bela`: {body}");
    assert_eq!(
        m["total_distance_m"].as_i64().unwrap_or(-1),
        557 + d,
        "`total_distance_m` is the fixture's 557 plus the {d} m this run assigned — a \
         manifest that recomputes the distance itself, or a `schedule` that wrote a \
         number it did not store, lands here: {body}"
    );

    // THE JOIN. `manifest`'s CSV has to carry the distance `schedule` computed. Two
    // parts, one number, and neither part's own gate can compare them.
    let (_, csv) = gate.get("/api/manifest.csv", None);
    let parsed = rows(&csv);
    let row = parsed
        .iter()
        .find(|r| r.first().map(String::as_str) == Some(id.as_str()))
        .unwrap_or_else(|| panic!("the request just driven through is not in the CSV: {csv}"));
    assert_eq!(row.len(), 5, "the row lost a column: {csv}");
    assert_eq!(row[2], "enroute", "the CSV disagrees with the manifest about the state: {csv}");
    assert_eq!(row[3], "bela", "the CSV disagrees about the engineer: {csv}");
    assert_eq!(
        row[4],
        d.to_string(),
        "the CSV says the distance is {} and `schedule` said it was {d} — the two parts \
         are not using the same `geo`: {csv}",
        row[4]
    );

    // AND the radius, measured by `manifest`, has to contain the request `schedule`
    // placed. Within 2 km of `bela` there is this request and the seeded `lift`
    // (about 200 m away, still `new` and therefore carrying 0), and nothing else.
    let (_, near) = gate.get("/api/manifest?near_lat=47.5316&near_lon=19.0430&within_m=2000", None);
    let n = parse(&near);
    assert_eq!(
        n["total"], 2,
        "this request and the seeded `lift` are both within 2 km of `bela`; the other \
         two seeded requests are 6 km and further: {near}"
    );
    assert_eq!(
        n["total_distance_m"].as_i64().unwrap_or(-1),
        d,
        "inside that radius only this request has been assigned, so the filtered \
         distance total is exactly what `schedule` computed. This is the assertion that \
         two hand-rolled haversines cannot both satisfy: {near}"
    );
}
