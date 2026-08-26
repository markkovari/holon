//! arena:app — multiplayer Connect Four over composed contracts.
//!
//! One `record:store` row per game holds the board (a 42-char string, index
//! `row*7 + col`, row 0 = bottom), the two seats + their secret tokens, whose
//! turn it is, and the outcome. Every move is validated server-side — the game
//! is live, the caller's token owns the seat whose turn it is, the column has
//! room — then applied and win/draw-checked, all under the store's optimistic
//! revision check so two racing moves resolve to exactly one. `GET /events`
//! streams the public board (tokens redacted) to both players and any spectators
//! (the same SSE loop as pulse). Rules + interactive authoritative state; the
//! only bespoke logic is Connect Four itself.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::id::generate::generator as ids;
use bindings::records::store::store as records;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

const GAMES: &str = "games";
const COLS: usize = 7;
const ROWS: usize = 6;
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 900;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            (Method::Get, ["api", "games", id, "events"]) => stream_events(response_out, id, &path),
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Get, ["api", "games"]) => lobby(),
                    (Method::Post, ["api", "games"]) => create(&request),
                    (Method::Get, ["api", "games", id]) => get_game(id),
                    (Method::Post, ["api", "games", id, "join"]) => join(&request, id),
                    (Method::Post, ["api", "games", id, "move"]) => make_move(&request, id),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(response_out, outcome);
            }
        }
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "arena",
            "about": "multiplayer Connect Four — server-validated moves, win detection, live board over SSE",
            "create": "POST /api/games {name} -> {game, seat:'R', token}",
            "join": "POST /api/games/{id}/join {name} -> {game, seat:'Y', token}",
            "move": "POST /api/games/{id}/move {token, col}",
            "state": "GET /api/games/{id}",
            "stream": "GET /api/games/{id}/events?rev=n  (text/event-stream)",
            "lobby": "GET /api/games"
        })
        .to_string(),
    )
}

// ---- Connect Four rules -----------------------------------------------------

fn empty_board() -> String {
    ".".repeat(COLS * ROWS)
}

fn idx(r: usize, c: usize) -> usize {
    r * COLS + c
}

/// The lowest empty row in `col`, or None if the column is full.
fn drop_row(board: &[u8], col: usize) -> Option<usize> {
    (0..ROWS).find(|&r| board[idx(r, col)] == b'.')
}

/// If placing `color` at (r,c) makes four in a row, the winning cell indices.
fn winning_line(board: &[u8], r: usize, c: usize, color: u8) -> Option<Vec<usize>> {
    // (dr, dc) for the four axes: horizontal, vertical, both diagonals.
    for (dr, dc) in [(0i32, 1i32), (1, 0), (1, 1), (1, -1)] {
        let mut line = vec![idx(r, c)];
        // extend both directions along the axis
        for sign in [1i32, -1] {
            let (mut rr, mut cc) = (r as i32 + dr * sign, c as i32 + dc * sign);
            while rr >= 0
                && rr < ROWS as i32
                && cc >= 0
                && cc < COLS as i32
                && board[idx(rr as usize, cc as usize)] == color
            {
                line.push(idx(rr as usize, cc as usize));
                rr += dr * sign;
                cc += dc * sign;
            }
        }
        if line.len() >= 4 {
            line.sort_unstable();
            return Some(line);
        }
    }
    None
}

fn board_full(board: &[u8]) -> bool {
    !board.contains(&b'.')
}

// ---- game records -----------------------------------------------------------

fn load(id: &str) -> Option<(Value, u64)> {
    let e = records::get(GAMES, id).ok()?;
    let d = serde_json::from_str::<Value>(&e.data).ok()?;
    Some((d, e.revision))
}

/// The public view — board, players, turn, outcome — with tokens redacted.
fn public(d: &Value, rev: u64) -> Value {
    let seat = |k: &str| {
        let s = &d[k];
        if s["name"].is_string() {
            json!({ "name": s["name"], "joined": true })
        } else {
            json!({ "joined": false })
        }
    };
    json!({
        "id": d["id"],
        "status": d["status"],
        "board": d["board"],
        "cols": COLS,
        "rows": ROWS,
        "turn": d["turn"],
        "red": seat("red"),
        "yellow": seat("yellow"),
        "winner": d["winner"],
        "line": d["line"],
        "rev": rev,
        "updated": d["updated"],
    })
}

fn get_game(id: &str) -> Outcome {
    match load(id) {
        Some((d, rev)) => Outcome::Json(200, public(&d, rev).to_string()),
        None => Outcome::Err(404, "no such game".into()),
    }
}

fn lobby() -> Outcome {
    let entries = records::list_records(GAMES, 100, "").map(|p| p.entries).unwrap_or_default();
    let mut games: Vec<Value> = entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| public(&d, e.revision)))
        .collect();
    games.sort_by(|a, b| {
        b["updated"].as_u64().unwrap_or(0).cmp(&a["updated"].as_u64().unwrap_or(0))
    });
    Outcome::Json(200, json!({ "games": games }).to_string())
}

fn create(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = body["name"].as_str().unwrap_or("Red").trim().to_string();
    let name = if name.is_empty() { "Red".into() } else { name };
    let token = ids::nanoid(24);
    let data = json!({
        "id": Value::Null, // filled after create (record id is the game id)
        "status": "waiting",
        "board": empty_board(),
        "turn": "R",
        "red": { "name": name, "token": token },
        "yellow": Value::Null,
        "winner": "",
        "line": [],
        "created": now(),
        "updated": now(),
    });
    let entry = match records::create(GAMES, &data.to_string(), &["status".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    // write the game id back into the record so views carry it.
    let mut d = data;
    d["id"] = json!(entry.id);
    let _ = records::update(GAMES, &entry.id, &d.to_string(), entry.revision);
    Outcome::Json(
        201,
        json!({ "game": public(&d, entry.revision + 1), "seat": "R", "token": token_of(&d, "red") })
            .to_string(),
    )
}

fn token_of(d: &Value, seat: &str) -> String {
    d[seat]["token"].as_str().unwrap_or("").to_string()
}

fn join(request: &IncomingRequest, id: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = body["name"].as_str().unwrap_or("Yellow").trim().to_string();
    let name = if name.is_empty() { "Yellow".into() } else { name };
    let (mut d, rev) = match load(id) {
        Some(x) => x,
        None => return Outcome::Err(404, "no such game".into()),
    };
    if d["status"] != "waiting" {
        return Outcome::Err(409, "game is not open to join".into());
    }
    let token = ids::nanoid(24);
    d["yellow"] = json!({ "name": name, "token": token });
    d["status"] = json!("active");
    d["updated"] = json!(now());
    match records::update(GAMES, id, &d.to_string(), rev) {
        Ok(e) => Outcome::Json(
            200,
            json!({ "game": public(&d, e.revision), "seat": "Y", "token": token_of(&d, "yellow") })
                .to_string(),
        ),
        Err(records::StoreError::RevisionConflict(_)) => Outcome::Err(409, "already taken".into()),
        Err(e) => store_err(e),
    }
}

fn make_move(request: &IncomingRequest, id: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let token = body["token"].as_str().unwrap_or("").to_string();
    let col = match body["col"].as_u64() {
        Some(c) if (c as usize) < COLS => c as usize,
        _ => return Outcome::Err(422, "col must be 0..6".into()),
    };
    let (mut d, rev) = match load(id) {
        Some(x) => x,
        None => return Outcome::Err(404, "no such game".into()),
    };
    if d["status"] != "active" {
        return Outcome::Err(409, "game is not active".into());
    }
    // token -> seat
    let seat = if token == token_of(&d, "red") {
        "R"
    } else if token == token_of(&d, "yellow") {
        "Y"
    } else {
        return Outcome::Err(403, "not a player in this game".into());
    };
    if d["turn"] != seat {
        return Outcome::Err(403, "not your turn".into());
    }
    let color = if seat == "R" { b'R' } else { b'Y' };
    let mut board: Vec<u8> = d["board"].as_str().unwrap_or("").bytes().collect();
    if board.len() != COLS * ROWS {
        board = empty_board().into_bytes();
    }
    let row = match drop_row(&board, col) {
        Some(r) => r,
        None => return Outcome::Err(409, "column is full".into()),
    };
    board[idx(row, col)] = color;

    // outcome
    if let Some(line) = winning_line(&board, row, col, color) {
        d["status"] = json!("finished");
        d["winner"] = json!(seat);
        d["line"] = json!(line);
    } else if board_full(&board) {
        d["status"] = json!("finished");
        d["winner"] = json!("draw");
    } else {
        d["turn"] = json!(if seat == "R" { "Y" } else { "R" });
    }
    d["board"] = json!(String::from_utf8_lossy(&board));
    d["updated"] = json!(now());

    match records::update(GAMES, id, &d.to_string(), rev) {
        Ok(e) => Outcome::Json(200, public(&d, e.revision).to_string()),
        // a concurrent move bumped the revision — exactly one lands.
        Err(records::StoreError::RevisionConflict(_)) => {
            Outcome::Err(409, "the board changed — reload and move again".into())
        }
        Err(e) => store_err(e),
    }
}

// ---- SSE --------------------------------------------------------------------

fn stream_events(response_out: ResponseOutparam, id: &str, path: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    let mut cursor = query_i64(path, "rev").unwrap_or(-1);
    {
        let stream = body.write().expect("write stream");
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            let frame = match load(id) {
                Some((d, rev)) if (rev as i64) != cursor => {
                    cursor = rev as i64;
                    format!("data: {}\n\n", public(&d, rev))
                }
                _ => ": ping\n\n".to_string(),
            };
            if !write_all(&stream, frame.as_bytes()) {
                break;
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

// ---- http plumbing -----------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn query_i64(path: &str, key: &str) -> Option<i64> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").parse().ok())?
    })
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => {
            respond(response_out, code, "application/json", body.as_bytes())
        }
        Outcome::Err(code, msg) => respond(
            response_out,
            code,
            "application/json",
            json!({ "error": msg }).to_string().as_bytes(),
        ),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = write_all(&stream, chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
