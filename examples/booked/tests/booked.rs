//! E2E for the booked scheduling app (BOOKED.md) as ONE composed wasm HTTP
//! component (booked-domain + auth-guard + records + lock-mutex + email-render +
//! ical + rrule) on the native Rust host. Proves the capability model: an owner
//! creates a resource + weekly availability; a member books a free slot and
//! CANNOT double-book (a second/overlapping/out-of-availability booking is
//! rejected); CONCURRENT attempts on one slot leave exactly one booking (the
//! lock:mutex no-double-book guarantee); a weekly recurrence expands to N
//! instances (rrule:recur); and a booking exports to a valid `.ics` (ical:codec).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3041";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn base() -> String {
    format!("http://{ADDR}")
}

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let mut r = ureq::request(method, &url);
    if let Some(t) = token {
        r = r.set("authorization", &format!("Bearer {t}"));
    }
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

/// GET a text body (e.g. an .ics) with its content-type.
fn get_text(path: &str, token: &str) -> (u16, String, String) {
    let r = ureq::get(&format!("{}{}", base(), path)).set("authorization", &format!("Bearer {token}")).call();
    match r {
        Ok(resp) => {
            let ct = resp.header("content-type").unwrap_or("").to_string();
            (200, ct, resp.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(s, resp)) => (s, String::new(), resp.into_string().unwrap_or_default()),
        Err(e) => panic!("GET {path}: {e}"),
    }
}

fn signup(email: &str, role: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678", "role": role })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/booked_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-booked`)");
    assert!(component.exists(), "composed wasm missing (just compose-booked)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "booked")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("booked host did not start");
}

fn book(tok: &str, res: &str, day: &str, start: i64, end: i64) -> (u16, Value) {
    req("POST", "/api/bookings", Some(tok), Some(json!({ "resource": res, "day": day, "start": start, "end": end })))
}

#[test]
fn booking_capability_and_no_double_book() {
    let _host = start_host();
    let owner = signup("owner@acme.io", "owner");
    let member = signup("mem@acme.io", "member");

    // ===== owner creates a resource; a non-owner cannot ====================
    let (s, r) = req("POST", "/api/resources", Some(&owner), Some(json!({ "key": "room-a", "name": "Room A", "slot": 30 })));
    assert_eq!(s, 201, "{r}");
    let rid = r["id"].as_str().unwrap().to_string();
    let (s, _) = req("POST", "/api/resources", Some(&member), Some(json!({ "key": "x", "name": "X" })));
    assert_eq!(s, 403, "member cannot create resources");

    // weekly availability: Tuesday (1) 09:00–12:00.
    let (s, _) = req("POST", &format!("/api/resources/{rid}/availability"), Some(&owner),
        Some(json!({ "windows": [{ "weekday": 1, "start": 540, "end": 720 }] })));
    assert_eq!(s, 200);

    // ===== free slots on a Tuesday: six 30-min slots in 09:00–12:00 ========
    let (s, r) = req("GET", &format!("/api/resources/{rid}/slots?day=2026-07-21"), Some(&member), None);
    assert_eq!(s, 200);
    assert_eq!(r["slots"].as_array().unwrap().len(), 6, "09:00–12:00 / 30 = 6 slots");

    // ===== book one; then NO double-book ===================================
    let (s, r) = book(&member, &rid, "2026-07-21", 540, 570);
    assert_eq!(s, 201, "{r}");
    assert_eq!(r["booked"].as_array().unwrap().len(), 1);
    assert!(r["confirmation"]["subject"].as_str().unwrap().contains("Room A"), "email-render confirmation");
    let bid = r["booked"][0]["id"].as_str().unwrap().to_string();

    assert_eq!(book(&owner, &rid, "2026-07-21", 540, 570).0, 409, "exact double-book rejected");
    assert_eq!(book(&owner, &rid, "2026-07-21", 555, 585).0, 409, "overlapping booking rejected");
    assert_eq!(book(&member, &rid, "2026-07-22", 540, 570).0, 409, "Wednesday is outside availability");

    // one slot gone from the free list.
    let (_, r) = req("GET", &format!("/api/resources/{rid}/slots?day=2026-07-21"), Some(&member), None);
    assert_eq!(r["slots"].as_array().unwrap().len(), 5, "one slot now taken");

    // ===== a weekly recurrence expands to N instances (rrule:recur) ========
    let (s, r) = req("POST", "/api/bookings", Some(&member),
        Some(json!({ "resource": rid, "day": "2026-07-21", "start": 600, "end": 630, "repeat": { "freq": "weekly", "count": 4 } })));
    assert_eq!(s, 201, "{r}");
    let days: Vec<&str> = r["booked"].as_array().unwrap().iter().map(|b| b["day"].as_str().unwrap()).collect();
    assert_eq!(days, ["2026-07-21", "2026-07-28", "2026-08-04", "2026-08-11"], "four consecutive Tuesdays");

    // ===== a booking exports to a valid .ics (ical:codec) ==================
    let (s, ct, ics) = get_text(&format!("/api/bookings/{bid}.ics"), &member);
    assert_eq!(s, 200);
    assert!(ct.starts_with("text/calendar"), "content-type: {ct}");
    assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"), "CRLF VCALENDAR header");
    assert!(ics.contains("DTSTART:20260721T090000Z"), "UTC DTSTART in .ics:\n{ics}");
    assert!(ics.contains("BEGIN:VALARM"), "a reminder alarm");
    assert!(ics.trim_end().ends_with("END:VCALENDAR"));
    // the resource feed carries every event (1 single + 4 recurring = 5).
    let (_, _, feed) = get_text(&format!("/api/resources/{rid}/calendar.ics"), &owner);
    assert_eq!(feed.matches("BEGIN:VEVENT").count(), 5, "feed has all 5 events");

    // ===== CONCURRENCY: many racers, one slot, exactly one wins ===========
    // Thursday is outside availability, so use a fresh Tuesday slot (11:30).
    let day = "2026-07-28";
    let (start, end) = (690, 720);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let tok = member.clone();
            let res = rid.clone();
            std::thread::spawn(move || book(&tok, &res, day, start, end).0)
        })
        .collect();
    let codes: Vec<u16> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let wins = codes.iter().filter(|&&c| c == 201).count();
    let losses = codes.iter().filter(|&&c| c == 409).count();
    assert_eq!(wins, 1, "exactly one concurrent booking succeeds: {codes:?}");
    assert_eq!(wins + losses, 8, "the rest are conflicts: {codes:?}");
    // ground truth: exactly one booking exists in the store for that slot.
    let (_, r) = req("GET", "/api/bookings?from=2026-07-28&to=2026-07-28", Some(&owner), None);
    let at_slot = r["items"].as_array().unwrap().iter()
        .filter(|b| b["resource"].as_str() == Some(&rid) && b["start"].as_i64() == Some(start))
        .count();
    assert_eq!(at_slot, 1, "the store holds exactly one booking for the contested slot");
}
