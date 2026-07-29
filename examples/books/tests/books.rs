//! E2E for the books bookkeeping app (BOOKS.md) as ONE composed wasm HTTP
//! component (books-domain + auth-guard + records + ledger + pdf) on the native
//! Rust host. Proves the double-entry invariant end to end: a fresh account is
//! seeded a demo chart + entries; a BALANCED entry posts and an UNBALANCED one
//! is rejected; the trial balance's debits equal its credits; the P&L nets
//! income minus expenses; the balance sheet balances (assets = liabilities +
//! equity + net income); and a statements PDF renders.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3045";

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

fn signup(email: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/books_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-books`)");
    assert!(component.exists(), "composed wasm missing (just compose-books)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "books")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("books host did not start");
}

fn dr(acct: &str, amt: i64) -> Value {
    json!({ "account": acct, "amount": amt, "side": "debit" })
}
fn cr(acct: &str, amt: i64) -> Value {
    json!({ "account": acct, "amount": amt, "side": "credit" })
}

#[test]
fn double_entry_invariant_and_statements() {
    let _host = start_host();
    let tok = signup("acc@acme.io");

    // ===== a fresh account is seeded a demo chart + entries ================
    let (_, ac) = req("GET", "/api/accounts", Some(&tok), None);
    assert_eq!(ac["items"].as_array().unwrap().len(), 7, "seeded chart of accounts");
    let (_, en) = req("GET", "/api/entries", Some(&tok), None);
    assert_eq!(en["items"].as_array().unwrap().len(), 4, "seeded journal");

    // ===== a BALANCED entry posts; an UNBALANCED one is rejected ==========
    let (s, _) = req("POST", "/api/entries", Some(&tok),
        Some(json!({ "date": "2026-07-20", "memo": "credit sale", "lines": [dr("1100", 25000), cr("4000", 25000)] })));
    assert_eq!(s, 201, "balanced entry posts");

    let (s, r) = req("POST", "/api/entries", Some(&tok),
        Some(json!({ "date": "2026-07-20", "memo": "oops", "lines": [dr("1000", 100), cr("4000", 90)] })));
    assert_eq!(s, 422, "unbalanced rejected");
    assert!(r["error"].as_str().unwrap().contains("unbalanced"), "{r}");

    // a lopsided entry with the wrong side never balances either.
    let (s, _) = req("POST", "/api/entries", Some(&tok),
        Some(json!({ "date": "2026-07-20", "memo": "both debit", "lines": [dr("1000", 100), dr("4000", 100)] })));
    assert_eq!(s, 422, "two debits don't balance");

    // an unknown account is rejected.
    let (s, r) = req("POST", "/api/entries", Some(&tok),
        Some(json!({ "date": "2026-07-20", "memo": "x", "lines": [dr("9999", 10), cr("4000", 10)] })));
    assert_eq!(s, 422);
    assert!(r["error"].as_str().unwrap().contains("unknown account"), "{r}");

    // ===== the trial balance's debits equal its credits ===================
    let (_, t) = req("GET", "/api/reports/trial", Some(&tok), None);
    assert_eq!(t["balanced"], true);
    assert_eq!(t["total_debits"], t["total_credits"], "trial balance balances: {t}");

    // ===== P&L nets income minus expenses =================================
    // seed: Sales 1200 (+ this test's 250) = 1450 income; Rent 800 + Supplies 300 = 1100 expenses.
    let (_, p) = req("GET", "/api/reports/pnl", Some(&tok), None);
    assert_eq!(p["total_income"], 145000);
    assert_eq!(p["total_expenses"], 110000);
    assert_eq!(p["net_income"], 35000, "net income = income - expenses");

    // ===== the balance sheet balances (A = L + E + net income) ============
    let (_, b) = req("GET", "/api/reports/balance-sheet", Some(&tok), None);
    assert_eq!(b["balanced"], true, "{b}");
    let a = b["total_assets"].as_i64().unwrap();
    let l = b["total_liabilities"].as_i64().unwrap();
    let e = b["total_equity"].as_i64().unwrap();
    let ni = b["net_income"].as_i64().unwrap();
    assert_eq!(a, l + e + ni, "assets = liabilities + equity + net income");

    // ===== the statements export to a valid PDF (pdf:codec) ===============
    let resp = ureq::get(&format!("{}/api/reports/statement.pdf", base()))
        .set("authorization", &format!("Bearer {tok}"))
        .call()
        .expect("statement.pdf");
    assert_eq!(resp.status(), 200);
    assert!(resp.header("content-type").unwrap_or("").starts_with("application/pdf"));
    let mut pdf = Vec::new();
    resp.into_reader().read_to_end(&mut pdf).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4") && pdf.trim_ascii_end().ends_with(b"%%EOF"), "valid PDF");
}
