//! `books-domain` — a double-entry bookkeeping service (docs/apps/BOOKS.md) as ONE composed
//! wasm HTTP component. Exports `wasi:http`; imports only WIT contracts: the
//! composed auth-guard (`auth:identity`), `records:store`, `ledger:doubleentry`
//! (the debits==credits invariant + trial balance) and `pdf:codec` (statements).
//! No bespoke auth, storage, accounting core, or PDF writer.

#[allow(warnings)]
mod bindings;

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::ledger::doubleentry::ledger;
use bindings::pdf::codec::codec as pdf;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "books";
const ACCOUNTS: &str = "accounts";
const ENTRIES: &str = "entries";
const TYPES: &[&str] = &["asset", "liability", "equity", "income", "expense"];

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "accounts"]) => create_account(&request),
            (Method::Get, ["api", "accounts"]) => list_accounts(&request),
            (Method::Post, ["api", "entries"]) => create_entry(&request),
            (Method::Get, ["api", "entries"]) => list_entries(&request),

            (Method::Get, ["api", "reports", "trial"]) => report_trial(&request),
            (Method::Get, ["api", "reports", "pnl"]) => report_pnl(&request),
            (Method::Get, ["api", "reports", "balance-sheet"]) => report_balance_sheet(&request),
            (Method::Get, ["api", "reports", "statement.pdf"]) => statement_pdf(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    File(u16, String, Option<String>, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "books",
            "about": "double-entry bookkeeping — balanced journal (debits==credits), trial balance / P&L / balance sheet + PDF",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "chart": "POST|GET /api/accounts {code, name, type: asset|liability|equity|income|expense}",
            "journal": "POST /api/entries {date, memo, lines:[{account, amount, side: debit|credit}]}, GET /api/entries",
            "reports": "GET /api/reports/trial | pnl | balance-sheet | statement.pdf"
        })
        .to_string(),
    )
}

// ---- money ------------------------------------------------------------------

/// Format integer minor units as a signed dollar string.
fn money(cents: i64) -> String {
    let neg = cents < 0;
    let a = cents.abs();
    format!("{}${}.{:02}", if neg { "-" } else { "" }, a / 100, a % 100)
}

/// The natural (positive) balance of an account given its type and net
/// (debits - credits): debit-normal types keep the net, credit-normal flip it.
fn natural(acc_type: &str, net: i64) -> i64 {
    match acc_type {
        "asset" | "expense" => net,
        _ => -net,
    }
}

// ---- auth -------------------------------------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get(&"authorization".to_string());
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token = bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    seed_demo(&p.subject);
    Outcome::Json(201, json!({ "subject": p.subject }).to_string())
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

// ---- chart of accounts ------------------------------------------------------

fn owned_accounts(subject: &str) -> Vec<Value> {
    records::find_by(ACCOUNTS, "owner", &json!(subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

/// code -> (name, type) for the owner's chart.
fn account_meta(subject: &str) -> BTreeMap<String, (String, String)> {
    owned_accounts(subject)
        .into_iter()
        .filter_map(|a| {
            Some((
                a["code"].as_str()?.to_string(),
                (a["name"].as_str().unwrap_or("").to_string(), a["type"].as_str().unwrap_or("asset").to_string()),
            ))
        })
        .collect()
}

fn create_account(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let code = b["code"].as_str().unwrap_or("").trim().to_string();
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    let acc_type = b["type"].as_str().unwrap_or("").to_string();
    if code.is_empty() || name.is_empty() {
        return Outcome::Err(422, "code and name required".into());
    }
    if !TYPES.contains(&acc_type.as_str()) {
        return Outcome::Err(422, "type must be asset|liability|equity|income|expense".into());
    }
    if account_meta(&p.subject).contains_key(&code) {
        return Outcome::Err(409, "account code already exists".into());
    }
    let d = json!({ "code": code, "name": name, "type": acc_type, "owner": p.subject, "created": now() });
    match records::create(ACCOUNTS, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn list_accounts(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut items = owned_accounts(&p.subject);
    items.sort_by(|a, b| a["code"].as_str().cmp(&b["code"].as_str()));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

// ---- journal ----------------------------------------------------------------

fn parse_side(s: &str) -> Option<ledger::Side> {
    match s {
        "debit" => Some(ledger::Side::Debit),
        "credit" => Some(ledger::Side::Credit),
        _ => None,
    }
}

/// Build a `ledger::Entry` from a stored/posted entry's JSON.
fn to_ledger(id: &str, v: &Value) -> Option<ledger::Entry> {
    let mut lines = Vec::new();
    for l in v["lines"].as_array()? {
        lines.push(ledger::Line {
            account: l["account"].as_str()?.to_string(),
            amount: l["amount"].as_i64()?,
            side: parse_side(l["side"].as_str()?)?,
        });
    }
    Some(ledger::Entry { id: id.to_string(), memo: v["memo"].as_str().unwrap_or("").to_string(), lines })
}

fn ledger_err(e: ledger::LedgerError) -> Outcome {
    let msg = match e {
        ledger::LedgerError::Unbalanced((d, c)) => format!("unbalanced: debits {} != credits {}", money(d), money(c)),
        ledger::LedgerError::TooFewLines => "an entry needs at least two lines".into(),
        ledger::LedgerError::Nonpositive(a) => format!("line for {a} must have a positive amount"),
    };
    Outcome::Err(422, msg)
}

fn create_entry(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let date = b["date"].as_str().unwrap_or("").to_string();
    if date.is_empty() {
        return Outcome::Err(422, "date required (YYYY-MM-DD)".into());
    }
    // build + validate the double-entry (the invariant lives in ledger:doubleentry).
    let le = match to_ledger("", &b) {
        Some(e) => e,
        None => return Outcome::Err(422, "each line needs {account, amount>0, side: debit|credit}".into()),
    };
    // every account must exist in the owner's chart.
    let meta = account_meta(&p.subject);
    for l in &le.lines {
        if !meta.contains_key(&l.account) {
            return Outcome::Err(422, format!("unknown account: {}", l.account));
        }
    }
    if let Err(e) = ledger::validate(&le) {
        return ledger_err(e);
    }
    let d = json!({
        "date": date, "memo": b["memo"].as_str().unwrap_or(""),
        "lines": b["lines"], "owner": p.subject, "created": now()
    });
    match records::create(ENTRIES, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn owned_entries(subject: &str) -> Vec<(String, Value)> {
    records::find_by(ENTRIES, "owner", &json!(subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id.clone(), v)))
        .collect()
}

fn list_entries(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut items: Vec<Value> = owned_entries(&p.subject)
        .into_iter()
        .map(|(id, mut v)| {
            v["id"] = json!(id);
            v
        })
        .collect();
    items.sort_by(|a, b| (a["date"].as_str(), a["created"].as_u64()).cmp(&(b["date"].as_str(), b["created"].as_u64())));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

// ---- reports (derived from the trial balance) -------------------------------

/// An enriched trial-balance line.
struct Row {
    code: String,
    name: String,
    kind: String,
    debits: i64,
    credits: i64,
    net: i64,
    balance: i64, // natural (positive) balance
}

/// Compute the owner's trial balance via `ledger:doubleentry`, enriched with
/// account name/type. `None` on an internal inconsistency (shouldn't happen —
/// every stored entry was validated on create).
fn snapshot(subject: &str) -> Option<(Vec<Row>, i64, i64, bool)> {
    let meta = account_meta(subject);
    let les: Vec<ledger::Entry> = owned_entries(subject).iter().filter_map(|(id, v)| to_ledger(id, v)).collect();
    let trial = ledger::trial_balance(&les).ok()?;
    let rows = trial
        .accounts
        .into_iter()
        .map(|a| {
            let (name, kind) = meta.get(&a.account).cloned().unwrap_or_default();
            let balance = natural(&kind, a.net);
            Row { code: a.account, name, kind, debits: a.debits, credits: a.credits, net: a.net, balance }
        })
        .collect();
    Some((rows, trial.total_debits, trial.total_credits, trial.balanced))
}

fn report_trial(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (rows, td, tc, balanced) = match snapshot(&p.subject) {
        Some(s) => s,
        None => return Outcome::Err(500, "ledger inconsistency".into()),
    };
    let accounts: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "code": r.code, "name": r.name, "type": r.kind, "debits": r.debits, "credits": r.credits, "net": r.net }))
        .collect();
    Outcome::Json(
        200,
        json!({ "accounts": accounts, "total_debits": td, "total_credits": tc, "balanced": balanced }).to_string(),
    )
}

/// (income rows, expense rows, total_income, total_expenses, net_income).
fn pnl(rows: &[Row]) -> (Vec<Value>, Vec<Value>, i64, i64, i64) {
    let mut income = Vec::new();
    let mut expenses = Vec::new();
    let (mut ti, mut te) = (0i64, 0i64);
    for r in rows {
        match r.kind.as_str() {
            "income" => {
                ti += r.balance;
                income.push(json!({ "code": r.code, "name": r.name, "amount": r.balance }));
            }
            "expense" => {
                te += r.balance;
                expenses.push(json!({ "code": r.code, "name": r.name, "amount": r.balance }));
            }
            _ => {}
        }
    }
    (income, expenses, ti, te, ti - te)
}

fn report_pnl(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (rows, _, _, _) = match snapshot(&p.subject) {
        Some(s) => s,
        None => return Outcome::Err(500, "ledger inconsistency".into()),
    };
    let (income, expenses, ti, te, net) = pnl(&rows);
    Outcome::Json(
        200,
        json!({ "income": income, "expenses": expenses, "total_income": ti, "total_expenses": te, "net_income": net }).to_string(),
    )
}

fn report_balance_sheet(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (rows, _, _, _) = match snapshot(&p.subject) {
        Some(s) => s,
        None => return Outcome::Err(500, "ledger inconsistency".into()),
    };
    let (_, _, _, _, net_income) = pnl(&rows);
    let mut assets = Vec::new();
    let mut liabilities = Vec::new();
    let mut equity = Vec::new();
    let (mut ta, mut tl, mut teq) = (0i64, 0i64, 0i64);
    for r in &rows {
        let entry = json!({ "code": r.code, "name": r.name, "amount": r.balance });
        match r.kind.as_str() {
            "asset" => {
                ta += r.balance;
                assets.push(entry);
            }
            "liability" => {
                tl += r.balance;
                liabilities.push(entry);
            }
            "equity" => {
                teq += r.balance;
                equity.push(entry);
            }
            _ => {}
        }
    }
    // the accounting identity: assets = liabilities + equity + current earnings.
    let balanced = ta == tl + teq + net_income;
    Outcome::Json(
        200,
        json!({
            "assets": assets, "liabilities": liabilities, "equity": equity,
            "total_assets": ta, "total_liabilities": tl, "total_equity": teq,
            "net_income": net_income, "balanced": balanced
        })
        .to_string(),
    )
}

fn statement_pdf(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (rows, td, tc, balanced) = match snapshot(&p.subject) {
        Some(s) => s,
        None => return Outcome::Err(500, "ledger inconsistency".into()),
    };
    let (income, expenses, ti, te, net) = pnl(&rows);
    let line = |text: String, size: u32, bold: bool, gap: u32| pdf::Block { text, size, bold, gap_before: gap };
    let col = |left: &str, right: String| format!("{:<34}{}", trunc(left, 34), right);

    let mut blocks = vec![line("Trial balance".into(), 13, true, 4)];
    for r in &rows {
        blocks.push(line(
            format!("{:<8}{:<26}Dr {:<12} Cr {}", r.code, trunc(&r.name, 24), money(r.debits), money(r.credits)),
            10,
            false,
            0,
        ));
    }
    blocks.push(line(col("Totals", format!("Dr {}  Cr {}   {}", money(td), money(tc), if balanced { "BALANCED" } else { "OFF" })), 11, true, 2));

    blocks.push(line("Profit & Loss".into(), 13, true, 14));
    for r in &income {
        blocks.push(line(col(&format!("  {}", r["name"].as_str().unwrap_or("")), money(r["amount"].as_i64().unwrap_or(0))), 10, false, 0));
    }
    blocks.push(line(col("Total income", money(ti)), 11, true, 0));
    for r in &expenses {
        blocks.push(line(col(&format!("  {}", r["name"].as_str().unwrap_or("")), money(r["amount"].as_i64().unwrap_or(0))), 10, false, 0));
    }
    blocks.push(line(col("Total expenses", money(te)), 11, true, 0));
    blocks.push(line(col("Net income", money(net)), 12, true, 2));

    blocks.push(line("Balance sheet".into(), 13, true, 14));
    let (mut ta, mut tl, mut teq) = (0i64, 0i64, 0i64);
    for r in &rows {
        let amt = r.balance;
        let (head, total) = match r.kind.as_str() {
            "asset" => ("A", &mut ta),
            "liability" => ("L", &mut tl),
            "equity" => ("E", &mut teq),
            _ => continue,
        };
        *total += amt;
        blocks.push(line(col(&format!("  [{}] {}", head, r.name), money(amt)), 10, false, 0));
    }
    blocks.push(line(col("Total assets", money(ta)), 11, true, 2));
    blocks.push(line(col("Liabilities + equity + net income", money(tl + teq + net)), 11, true, 0));
    blocks.push(line(
        if ta == tl + teq + net { "Balance sheet BALANCES".into() } else { "Balance sheet does NOT balance".into() },
        11,
        true,
        2,
    ));

    let doc = pdf::Document { title: "books — financial statements".to_string(), blocks };
    Outcome::File(200, "application/pdf".into(), Some("books-statements.pdf".into()), pdf::render(&doc))
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

// ---- demo seed --------------------------------------------------------------

fn seed_demo(subject: &str) {
    let chart = [
        ("1000", "Cash", "asset"),
        ("1100", "Accounts Receivable", "asset"),
        ("2000", "Accounts Payable", "liability"),
        ("3000", "Owner's Capital", "equity"),
        ("4000", "Sales", "income"),
        ("5000", "Rent Expense", "expense"),
        ("5100", "Supplies Expense", "expense"),
    ];
    for (code, name, kind) in chart {
        let d = json!({ "code": code, "name": name, "type": kind, "owner": subject, "created": now() });
        let _ = records::create(ACCOUNTS, &d.to_string(), &["owner".to_string()]);
    }
    let dr = |acct: &str, amt: i64| json!({ "account": acct, "amount": amt, "side": "debit" });
    let cr = |acct: &str, amt: i64| json!({ "account": acct, "amount": amt, "side": "credit" });
    let entries = [
        ("2026-07-01", "Owner investment", vec![dr("1000", 500000), cr("3000", 500000)]),
        ("2026-07-05", "Cash sale", vec![dr("1000", 120000), cr("4000", 120000)]),
        ("2026-07-08", "July rent", vec![dr("5000", 80000), cr("1000", 80000)]),
        ("2026-07-12", "Office supplies", vec![dr("5100", 30000), cr("1000", 30000)]),
    ];
    for (date, memo, lines) in entries {
        let d = json!({ "date": date, "memo": memo, "lines": lines, "owner": subject, "created": now() });
        let _ = records::create(ENTRIES, &d.to_string(), &["owner".to_string()]);
    }
}

// ---- http plumbing ----------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    if let Outcome::File(code, ctype, name, bytes) = result {
        let disp = name.map(|n| format!("attachment; filename=\"{}\"", n));
        return respond(response_out, code, &ctype, disp.as_deref(), &bytes);
    }
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
        Outcome::File(..) => unreachable!(),
    };
    respond(response_out, code, "application/json", None, body.as_bytes());
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, disposition: Option<&str>, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set(&"content-disposition".to_string(), &[d.as_bytes().to_vec()]);
    }
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
