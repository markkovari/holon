//! The four `clinic` gates, ported from `components/clinic-domain/e2e-*.sh`.
//!
//! One file rather than four: they share a fixture-free start and differ only in what
//! they drive, and four integration binaries would each pay for their own host.
//! Assertions and failure sentences are unchanged — ADR-0088 makes a gate's output the
//! next prompt a repair reads.
//!
//! The CSV and ranking checks were `python3` heredocs; `csv` is not a dependency here,
//! so the reader is fifteen lines and handles exactly what the contract describes — a
//! quoted field containing a comma. It is written out rather than pulled in because a
//! test that needs a CSV parser to state its claim is testing the parser.

mod gatelib;
use gatelib::{field, requires_capability, Gate};
use serde_json::{json, Value};

const CRATE: &str = "clinic-domain";

fn start() -> Option<Gate> {
    Gate::compose_and_start("clinic", CRATE, &[])
}

#[test]
fn owners_and_pets() {
    let Some(gate) = start() else { return };

    let (_, o) = gate.post("/api/owners", None, json!({"name":"Ada","email":"ada@example.test"}));
    let owner = field(&o, "id");
    assert!(!owner.is_empty(), "POST /api/owners returned no id");
    let (c, _) = gate.get(&format!("/api/owners/{owner}"), None);
    assert_eq!(c, 200, "the owner does not read back");
    let (c, _) = gate.post("/api/owners", None, json!({"name":"","email":"x@y"}));
    assert_eq!(c, 400, "an empty name is a 400");
    let (c, _) = gate.post("/api/owners", None, json!({"name":"Bo","email":"nope"}));
    assert_eq!(c, 400, "an email without @ is a 400");
    let (c, _) = gate.get("/api/owners/nosuch", None);
    assert_eq!(c, 404, "an unknown owner is a 404");

    let (_, p) = gate.post(&format!("/api/owners/{owner}/pets"), None,
        json!({"name":"Rex","species":"dog","born":"2020-01-01"}));
    let pet = field(&p, "id");
    assert!(!pet.is_empty(), "POST pets returned no id");
    let (c, _) = gate.post(&format!("/api/owners/{owner}/pets"), None,
        json!({"name":"X","species":"dragon","born":"2020-01-01"}));
    assert_eq!(c, 400, "an unknown species is a 400");
    let (c, _) = gate.post("/api/owners/nosuch/pets", None,
        json!({"name":"X","species":"cat","born":"2020-01-01"}));
    assert_eq!(c, 404, "a pet for an unknown owner is a 404");

    let (_, hits) = gate.get("/api/owners?q=ada", None);
    assert!(hits.contains(&owner), "search by name does not find the owner");
    let (_, pets) = gate.get(&format!("/api/owners/{owner}/pets"), None);
    assert!(pets.contains(&pet), "the owner's pets do not list");

    let (c, _) = gate.get("/api/nope", None);
    assert_eq!(c, 404, "an unknown route is a 404");
}

#[test]
fn visits_and_the_rule_a_compiler_cannot_check() {
    let Some(gate) = start() else { return };

    // `visits` cannot create a pet: pets belong to the other half, and this gate has
    // to pass while `src/owners.rs` is still a stub. `POST /test/seed` is scaffold.
    let seed = gate.seed();
    let pet = seed["pet_id"].as_str().unwrap_or_default().to_string();
    assert!(!pet.is_empty(), "the seed fixture gave no pet: {seed}");

    let visit = |vet: &str, start: &str, minutes: u32| {
        json!({"pet_id": pet, "vet": vet, "start": start, "minutes": minutes})
    };
    let (_, v) = gate.post("/api/visits", None, visit("vet-a", "2026-09-01T09:00:00Z", 30));
    let v1 = field(&v, "id");
    assert!(!v1.is_empty(), "POST /api/visits returned no id");

    let (c, _) = gate.post("/api/visits", None, visit("vet-a", "2026-09-01T09:15:00Z", 30));
    assert_eq!(c, 409, "an overlapping visit for the same vet must be a 409");
    let (c, _) = gate.post("/api/visits", None, visit("vet-a", "2026-09-01T09:30:00Z", 30));
    assert_eq!(c, 201, "touching at the boundary is not an overlap");
    let (c, _) = gate.post("/api/visits", None, visit("vet-b", "2026-09-01T09:15:00Z", 30));
    assert_eq!(c, 201, "a different vet at the same time is fine");
    let (c, _) = gate.post("/api/visits", None, visit("vet-a", "2026-09-01T11:00:00Z", 45));
    assert_eq!(c, 400, "45 minutes is not one of 15/30/60");
    let (c, _) = gate.post("/api/visits", None,
        json!({"pet_id":"nosuch","vet":"vet-a","start":"2026-09-01T14:00:00Z","minutes":30}));
    assert_eq!(c, 404, "a visit for an unknown pet is a 404");

    let (_, day) = gate.get("/api/visits?vet=vet-a&day=2026-09-01", None);
    assert!(day.contains(&v1), "the day's visits do not list");

    let (del, _) = gate.delete(&format!("/api/visits/{v1}"), None);
    assert_eq!(del, 204, "DELETE of a visit is a 204");
    let (c, _) = gate.post("/api/visits", None, visit("vet-a", "2026-09-01T09:00:00Z", 30));
    assert_eq!(c, 201, "a deleted visit must free its slot");

    let (c, _) = gate.get("/api/nope", None);
    assert_eq!(c, 404, "an unknown route is a 404");
}

#[test]
fn staff_access_and_pet_search() {
    let Some(gate) = start() else { return };

    requires_capability(CRATE, "auth:identity/accounts",
        "that capability is in the world for this part to USE, and reimplementing it is the one \
         thing this part must not do (see CONTRACT.md)");
    requires_capability(CRATE, "auth:identity/session",
        "sessions are auth-guard's job; do not invent a token format (see CONTRACT.md)");
    requires_capability(CRATE, "search:index/index",
        "ranked search already exists in this repository; a substring scan is not it (see CONTRACT.md)");

    gate.seed();

    let (c, _) = gate.post("/api/staff", None, json!({"email":"vet@clinic.test","password":"short"}));
    assert_eq!(c, 400, "a password under 8 characters is a 400");
    let (_, s) = gate.post("/api/staff", None, json!({"email":"vet@clinic.test","password":"correct-horse"}));
    assert!(!field(&s, "id").is_empty(), "POST /api/staff returned no id");
    let (c, _) = gate.post("/api/staff", None, json!({"email":"vet@clinic.test","password":"correct-horse"}));
    assert_eq!(c, 409, "registering an email twice is a 409");

    let (c, _) = gate.post("/api/staff/login", None, json!({"email":"vet@clinic.test","password":"wrong"}));
    assert_eq!(c, 401, "a wrong password is a 401");
    let (c, _) = gate.post("/api/staff/login", None, json!({"email":"nobody@clinic.test","password":"correct-horse"}));
    assert_eq!(c, 401, "an unknown email is a 401, the same answer as a wrong password");
    let (_, l) = gate.post("/api/staff/login", None, json!({"email":"vet@clinic.test","password":"correct-horse"}));
    let token = field(&l, "token");
    assert!(!token.is_empty(), "a correct login returned no token");

    let (c, _) = gate.get("/api/pets/search?q=cat", None);
    assert_eq!(c, 401, "search without a token is a 401");
    let (c, _) = gate.get("/api/pets/search?q=cat", Some("not-a-real-token"));
    assert_eq!(c, 401, "search with a made-up token is a 401");
    let (c, _) = gate.get("/api/pets/search?q=", Some(&token));
    assert_eq!(c, 400, "an empty q is a 400");

    let (_, hits) = gate.get("/api/pets/search?q=Marbles", Some(&token));
    assert!(hits.contains("Marbles"), "searching a pet's name does not find it: {hits}");
    assert!(!hits.contains("Biscuit"), "searching 'Marbles' returned an unrelated pet: {hits}");
    let (_, dogs) = gate.get("/api/pets/search?q=dog", Some(&token));
    assert!(dogs.contains("Biscuit"), "searching by species does not find the dog");

    // Ranked, best first: the pet whose NAME is the query outranks one that merely
    // shares a species with it.
    let (_, order) = gate.get("/api/pets/search?q=Marbles%20cat", Some(&token));
    let first = serde_json::from_str::<Value>(&order)
        .ok()
        .and_then(|v| v["pets"].as_array().and_then(|a| a.first().cloned()))
        .and_then(|p| p["name"].as_str().map(str::to_string))
        .unwrap_or_default();
    assert_eq!(first, "Marbles", "results are not ranked best-first: {order}");
}

/// A CSV reader that handles exactly what the contract describes: quoted fields, and a
/// comma inside one. Written out rather than depending on a parser, because a test
/// that needs a CSV library to state its claim is testing the library.
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

#[test]
fn reports_csv_and_summary() {
    let Some(gate) = start() else { return };

    requires_capability(CRATE, "csv:codec/codec",
        "CSV quoting is a solved problem in this repository and that capability is in the world \
         for this part to USE, not to reimplement (see CONTRACT.md)");

    // Built through the OTHER halves' routes: a report over a fixture nobody booked
    // would not be a report. The name is the point — a comma inside a field is what
    // separates a CSV encoder from `join(",")`.
    let (_, o) = gate.post("/api/owners", None, json!({"name":"Dana Vance","email":"dana@example.test"}));
    let owner = field(&o, "id");
    assert!(!owner.is_empty(), "could not create an owner to report on");
    let (_, p) = gate.post(&format!("/api/owners/{owner}/pets"), None,
        json!({"name":"Rex, Jr.","species":"dog","born":"2020-05-05"}));
    let pet = field(&p, "id");
    assert!(!pet.is_empty(), "could not create a pet to report on");
    let (_, c2) = gate.post(&format!("/api/owners/{owner}/pets"), None,
        json!({"name":"Zoe","species":"cat","born":"2021-06-06"}));
    let cat = field(&c2, "id");
    assert!(!cat.is_empty(), "could not create a second pet to report on");

    const DAY: &str = "2026-09-02";
    for (who, vet, at, mins) in [
        (&pet, "vet-a", "T09:00:00Z", 30),
        (&cat, "vet-b", "T08:00:00Z", 60),
        (&pet, "vet-a", "T10:00:00Z", 15),
    ] {
        gate.post("/api/visits", None,
            json!({"pet_id": who, "vet": vet, "start": format!("{DAY}{at}"), "minutes": mins}));
    }

    let (c, _) = gate.get("/api/reports/visits.csv", None);
    assert_eq!(c, 400, "a missing day is a 400");

    let (_, csv) = gate.get(&format!("/api/reports/visits.csv?day={DAY}"), None);
    let r = rows(&csv);
    assert!(!r.is_empty(), "no rows at all: {csv}");
    assert_eq!(
        r[0], ["id", "pet_id", "pet_name", "vet", "start", "minutes"],
        "the CSV is not what CONTRACT.md describes, header: {:?}", r[0]
    );
    assert_eq!(r.len(), 4, "three visits and a header make 4 rows, got {}", r.len());
    for row in &r[1..] {
        assert_eq!(row.len(), 6, "row has {} columns, not 6 — a comma broke it: {row:?}", row.len());
    }
    assert!(r[1..].iter().any(|row| row[2] == "Rex, Jr."), "the comma in the name did not survive: {r:?}");
    let starts: Vec<&String> = r[1..].iter().map(|row| &row[4]).collect();
    let mut sorted = starts.clone();
    sorted.sort();
    assert_eq!(starts, sorted, "not sorted by start: {starts:?}");

    // A day nobody booked is the header alone — not a 404, not an empty body.
    let (_, empty) = gate.get("/api/reports/visits.csv?day=2026-09-29", None);
    assert!(
        empty.lines().next().unwrap_or_default().starts_with("id,pet_id,pet_name,vet,start,minutes"),
        "an empty day still has its header: {empty}"
    );
    assert_eq!(
        empty.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "an empty day has no rows: {empty}"
    );

    let (_, sum) = gate.get(&format!("/api/reports/summary?day={DAY}"), None);
    let s: Value = serde_json::from_str(&sum).unwrap_or(Value::Null);
    assert_eq!(s["visits"], 3, "the summary is not what CONTRACT.md describes: {sum}");
    assert_eq!(s["minutes"], 105, "the summary is not what CONTRACT.md describes: {sum}");
    assert_eq!(s["by_vet"], json!({"vet-a": 2, "vet-b": 1}), "by_vet: {sum}");
    assert_eq!(s["by_species"], json!({"dog": 2, "cat": 1}), "by_species: {sum}");
}
