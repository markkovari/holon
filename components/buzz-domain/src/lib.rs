//! `buzz-domain` — a live multiplayer quiz game (docs/apps/BUZZ.md) as ONE composed wasm
//! HTTP component. Exports `wasi:http`; imports only WIT contracts: the composed
//! auth-guard (`auth:identity`) for the host, `records:store` for game state,
//! `wasi:random` for the PIN, and the wall clock for timing + speed-weighted
//! scoring. Players are anonymous, gated by the PIN + the id issued on join.
//! Real-time is client polling — comp-host is request/response.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::random::random::get_random_u64;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "buzz";
const QUIZZES: &str = "quizzes";
const GAMES: &str = "games";
const PLAYERS: &str = "players";
const ANSWERS: &str = "answers";
const BASE_POINTS: f64 = 1000.0;

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

            (Method::Post, ["api", "quizzes"]) => create_quiz(&request),
            (Method::Get, ["api", "quizzes"]) => list_quizzes(&request),
            (Method::Post, ["api", "games"]) => create_game(&request),
            (Method::Get, ["api", "games", pin, "host"]) => host_view(&request, pin),
            (Method::Post, ["api", "games", pin, "start"]) => host_advance(&request, pin, "start"),
            (Method::Post, ["api", "games", pin, "reveal"]) => host_advance(&request, pin, "reveal"),
            (Method::Post, ["api", "games", pin, "next"]) => host_advance(&request, pin, "next"),
            (Method::Post, ["api", "games", pin, "join"]) => join(&request, pin),
            (Method::Get, ["api", "games", pin, "play"]) => play_view(&request, pin, &path),
            (Method::Post, ["api", "games", pin, "answer"]) => answer(&request, pin),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn now_ms() -> u64 {
    let t = wall_clock::now();
    t.seconds * 1000 + (t.nanoseconds / 1_000_000) as u64
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "buzz",
            "about": "a live multiplayer quiz game — host runs a game by PIN, players buzz in, speed-weighted scoring + live leaderboard",
            "host": "POST /api/quizzes, POST /api/games {quiz} -> {pin}, GET /api/games/{pin}/host, POST /api/games/{pin}/start|reveal|next",
            "player": "POST /api/games/{pin}/join {nickname}, GET /api/games/{pin}/play?player=, POST /api/games/{pin}/answer {player, option}"
        })
        .to_string(),
    )
}

// ---- auth (host only) -------------------------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get("authorization");
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

// ---- records helpers --------------------------------------------------------

fn get(coll: &str, id: &str) -> Option<Value> {
    records::get(coll, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(|mut v| {
        v["id"] = json!(id);
        v
    })
}

fn find(coll: &str, field: &str, value: &str) -> Vec<Value> {
    records::find_by(coll, field, &json!(value).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
            v["id"] = json!(e.id);
            v
        }))
        .collect()
}

fn game_by_pin(pin: &str) -> Option<Value> {
    find(GAMES, "pin", pin).into_iter().next()
}

// ---- quizzes ----------------------------------------------------------------

fn create_quiz(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    let questions = b["questions"].as_array().cloned().unwrap_or_default();
    if title.is_empty() || questions.is_empty() {
        return Outcome::Err(422, "title and at least one question required".into());
    }
    for q in &questions {
        let opts = q["options"].as_array().map(|a| a.len()).unwrap_or(0);
        let ans = q["answer"].as_u64().unwrap_or(u64::MAX);
        if q["prompt"].as_str().unwrap_or("").is_empty() || opts < 2 || ans as usize >= opts {
            return Outcome::Err(422, "each question needs a prompt, >=2 options, and a valid answer index".into());
        }
    }
    let d = json!({ "host": p.subject, "title": title, "questions": questions, "created": now_ms() });
    match records::create(QUIZZES, &d.to_string(), &["host".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn list_quizzes(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let items: Vec<Value> = find(QUIZZES, "host", &p.subject)
        .into_iter()
        .map(|mut q| {
            q["question_count"] = json!(q["questions"].as_array().map(|a| a.len()).unwrap_or(0));
            q
        })
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

// ---- games (host) -----------------------------------------------------------

fn mint_pin() -> String {
    for _ in 0..20 {
        let pin = format!("{:06}", get_random_u64() % 1_000_000);
        if game_by_pin(&pin).is_none() {
            return pin;
        }
    }
    format!("{:06}", now_ms() % 1_000_000)
}

fn create_game(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let quiz_id = b["quiz"].as_str().unwrap_or("");
    let quiz = match get(QUIZZES, quiz_id) {
        Some(q) if q["host"].as_str() == Some(&p.subject) => q,
        Some(_) => return Outcome::Err(403, "not your quiz".into()),
        None => return Outcome::Err(404, "no such quiz".into()),
    };
    let pin = mint_pin();
    let d = json!({ "pin": pin, "quiz": quiz_id, "quiz_title": quiz["title"], "host": p.subject, "phase": "lobby", "current": -1, "q_started_ms": 0, "created": now_ms() });
    match records::create(GAMES, &d.to_string(), &["pin".to_string()]) {
        Ok(_) => Outcome::Json(201, json!({ "pin": pin }).to_string()),
        Err(e) => store_err(e),
    }
}

/// Load the game (by pin) for its host, or an error.
fn host_game(p: &Principal, pin: &str) -> Result<Value, Outcome> {
    let g = game_by_pin(pin).ok_or(Outcome::Err(404, "no such game".into()))?;
    if g["host"].as_str() != Some(&p.subject) {
        return Err(Outcome::Err(403, "not your game".into()));
    }
    Ok(g)
}

fn quiz_questions(quiz_id: &str) -> Vec<Value> {
    get(QUIZZES, quiz_id).and_then(|q| q["questions"].as_array().cloned()).unwrap_or_default()
}

fn players_ranked(pin: &str) -> Vec<Value> {
    let mut ps = find(PLAYERS, "game", pin);
    ps.sort_by(|a, b| b["score"].as_i64().unwrap_or(0).cmp(&a["score"].as_i64().unwrap_or(0)).then(a["joined"].as_u64().cmp(&b["joined"].as_u64())));
    ps
}

/// Answers for the game's current question index.
fn answers_for(pin: &str, q: i64) -> Vec<Value> {
    find(ANSWERS, "game", pin).into_iter().filter(|a| a["q"].as_i64() == Some(q)).collect()
}

fn host_advance(request: &IncomingRequest, pin: &str, action: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut g = match host_game(&p, pin) {
        Ok(g) => g,
        Err(o) => return o,
    };
    let id = g["id"].as_str().unwrap_or("").to_string();
    let questions = quiz_questions(g["quiz"].as_str().unwrap_or(""));
    let total = questions.len() as i64;
    let phase = g["phase"].as_str().unwrap_or("lobby").to_string();

    match action {
        "start" if phase == "lobby" => {
            g["current"] = json!(0);
            g["phase"] = json!("question");
            g["q_started_ms"] = json!(now_ms());
        }
        "reveal" if phase == "question" => {
            grade(pin, &g, &questions);
            g["phase"] = json!("reveal");
        }
        "next" if phase == "reveal" => {
            let cur = g["current"].as_i64().unwrap_or(0);
            if cur + 1 < total {
                g["current"] = json!(cur + 1);
                g["phase"] = json!("question");
                g["q_started_ms"] = json!(now_ms());
            } else {
                g["phase"] = json!("final");
            }
        }
        _ => return Outcome::Err(409, format!("can't {action} from phase {phase}")),
    }
    g.as_object_mut().map(|m| m.remove("id"));
    let _ = records::update(GAMES, &id, &g.to_string(), 0);
    Outcome::Json(200, json!({ "ok": true, "phase": g["phase"] }).to_string())
}

/// Grade the current question's answers (speed-weighted) and bump player scores.
fn grade(pin: &str, g: &Value, questions: &[Value]) {
    let cur = g["current"].as_i64().unwrap_or(0);
    let q = match questions.get(cur as usize) {
        Some(q) => q,
        None => return,
    };
    let answer_idx = q["answer"].as_u64().unwrap_or(0);
    let limit_ms = (q["time_limit"].as_u64().unwrap_or(20) * 1000).max(1) as f64;
    let started = g["q_started_ms"].as_u64().unwrap_or(0);

    for a in answers_for(pin, cur) {
        let correct = a["option"].as_u64() == Some(answer_idx);
        let points = if correct {
            let elapsed = a["at_ms"].as_u64().unwrap_or(started).saturating_sub(started) as f64;
            let frac = (elapsed / limit_ms).min(1.0);
            // full points when instant, half at the buzzer.
            (BASE_POINTS * (1.0 - frac * 0.5)).round() as i64
        } else {
            0
        };
        // record the grade on the answer.
        let aid = a["id"].as_str().unwrap_or("").to_string();
        let mut av = a.clone();
        av["correct"] = json!(correct);
        av["points"] = json!(points);
        av.as_object_mut().map(|m| m.remove("id"));
        let _ = records::update(ANSWERS, &aid, &av.to_string(), 0);
        // bump the player's score.
        if points > 0 {
            if let Some(player) = a["player"].as_str().and_then(|pid| get(PLAYERS, pid)) {
                let pid = player["id"].as_str().unwrap_or("").to_string();
                let mut pv = player;
                pv["score"] = json!(pv["score"].as_i64().unwrap_or(0) + points);
                pv.as_object_mut().map(|m| m.remove("id"));
                let _ = records::update(PLAYERS, &pid, &pv.to_string(), 0);
            }
        }
    }
}

fn host_view(request: &IncomingRequest, pin: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let g = match host_game(&p, pin) {
        Ok(g) => g,
        Err(o) => return o,
    };
    let phase = g["phase"].as_str().unwrap_or("lobby");
    let cur = g["current"].as_i64().unwrap_or(-1);
    let questions = quiz_questions(g["quiz"].as_str().unwrap_or(""));
    let total = questions.len();
    let ranked = players_ranked(pin);

    let mut out = json!({
        "pin": pin, "phase": phase, "quiz_title": g["quiz_title"], "current": cur, "total": total,
        "players": ranked.iter().map(|p| json!({ "nickname": p["nickname"], "score": p["score"] })).collect::<Vec<_>>(),
        "leaderboard": ranked.iter().take(8).map(|p| json!({ "nickname": p["nickname"], "score": p["score"] })).collect::<Vec<_>>(),
    });
    if (phase == "question" || phase == "reveal") && cur >= 0 {
        if let Some(q) = questions.get(cur as usize) {
            let ans = answers_for(pin, cur);
            let mut counts = vec![0u32; q["options"].as_array().map(|a| a.len()).unwrap_or(0)];
            for a in &ans {
                if let Some(o) = a["option"].as_u64() {
                    if (o as usize) < counts.len() {
                        counts[o as usize] += 1;
                    }
                }
            }
            out["question"] = json!({ "index": cur, "prompt": q["prompt"], "options": q["options"], "answer": q["answer"], "time_limit": q["time_limit"] });
            out["answered"] = json!(ans.len());
            out["counts"] = json!(counts);
        }
    }
    Outcome::Json(200, out.to_string())
}

// ---- players (anonymous) ----------------------------------------------------

fn join(request: &IncomingRequest, pin: &str) -> Outcome {
    let g = match game_by_pin(pin) {
        Some(g) => g,
        None => return Outcome::Err(404, "no game with that PIN".into()),
    };
    if g["phase"].as_str() != Some("lobby") {
        return Outcome::Err(409, "the game already started".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let nickname = b["nickname"].as_str().unwrap_or("").trim().to_string();
    if nickname.is_empty() {
        return Outcome::Err(422, "nickname required".into());
    }
    let d = json!({ "game": pin, "nickname": nickname, "score": 0, "joined": now_ms() });
    match records::create(PLAYERS, &d.to_string(), &["game".to_string()]) {
        Ok(rec) => Outcome::Json(201, json!({ "player": rec.id, "nickname": nickname }).to_string()),
        Err(e) => store_err(e),
    }
}

fn player_of(pin: &str, id: &str) -> Option<Value> {
    get(PLAYERS, id).filter(|p| p["game"].as_str() == Some(pin))
}

fn answer(request: &IncomingRequest, pin: &str) -> Outcome {
    let g = match game_by_pin(pin) {
        Some(g) => g,
        None => return Outcome::Err(404, "no such game".into()),
    };
    if g["phase"].as_str() != Some("question") {
        return Outcome::Err(409, "not accepting answers right now".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let player = b["player"].as_str().unwrap_or("");
    if player_of(pin, player).is_none() {
        return Outcome::Err(403, "unknown player".into());
    }
    let cur = g["current"].as_i64().unwrap_or(0);
    // one answer per question.
    if answers_for(pin, cur).iter().any(|a| a["player"].as_str() == Some(player)) {
        return Outcome::Json(200, json!({ "ok": true, "already": true }).to_string());
    }
    let option = b["option"].as_u64().unwrap_or(u64::MAX);
    let d = json!({ "game": pin, "q": cur, "player": player, "option": option, "at_ms": now_ms(), "correct": Value::Null, "points": 0 });
    let _ = records::create(ANSWERS, &d.to_string(), &["game".to_string()]);
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

fn rank_of(ranked: &[Value], player_id: &str) -> usize {
    ranked.iter().position(|p| p["id"].as_str() == Some(player_id)).map(|i| i + 1).unwrap_or(0)
}

fn play_view(request: &IncomingRequest, pin: &str, path: &str) -> Outcome {
    let _ = request;
    let g = match game_by_pin(pin) {
        Some(g) => g,
        None => return Outcome::Err(404, "no such game".into()),
    };
    let pid = query_str(path, "player").unwrap_or_default();
    let player = match player_of(pin, &pid) {
        Some(p) => p,
        None => return Outcome::Err(403, "unknown player".into()),
    };
    let phase = g["phase"].as_str().unwrap_or("lobby");
    let cur = g["current"].as_i64().unwrap_or(-1);
    let questions = quiz_questions(g["quiz"].as_str().unwrap_or(""));
    let ranked = players_ranked(pin);
    let my_rank = rank_of(&ranked, &pid);

    let mut out = json!({ "phase": phase, "nickname": player["nickname"], "players_count": ranked.len(), "my_score": player["score"], "my_rank": my_rank });
    match phase {
        "question" if cur >= 0 => {
            if let Some(q) = questions.get(cur as usize) {
                let started = g["q_started_ms"].as_u64().unwrap_or(0);
                let limit_ms = q["time_limit"].as_u64().unwrap_or(20) * 1000;
                let left = (started + limit_ms).saturating_sub(now_ms());
                let answered = answers_for(pin, cur).iter().any(|a| a["player"].as_str() == Some(&pid));
                out["question"] = json!({ "index": cur, "total": questions.len(), "prompt": q["prompt"], "options": q["options"], "time_limit": q["time_limit"], "time_left_ms": left, "answered": answered });
            }
        }
        "reveal" if cur >= 0 => {
            if let Some(q) = questions.get(cur as usize) {
                let mine = answers_for(pin, cur).into_iter().find(|a| a["player"].as_str() == Some(&pid));
                out["reveal"] = json!({
                    "correct_option": q["answer"],
                    "my_option": mine.as_ref().and_then(|a| a["option"].as_u64()),
                    "my_correct": mine.as_ref().map(|a| a["correct"].as_bool().unwrap_or(false)).unwrap_or(false),
                    "my_points": mine.as_ref().map(|a| a["points"].as_i64().unwrap_or(0)).unwrap_or(0),
                });
            }
        }
        "final" => {
            out["podium"] = json!(ranked.iter().take(3).map(|p| json!({ "nickname": p["nickname"], "score": p["score"] })).collect::<Vec<_>>());
        }
        _ => {}
    }
    Outcome::Json(200, out.to_string())
}

// ---- demo seed --------------------------------------------------------------

fn seed_demo(subject: &str) {
    let d = json!({
        "host": subject, "title": "WIT Warm-up", "created": now_ms(),
        "questions": [
            { "prompt": "A WIT world describes a component's…", "options": ["colours", "imports & exports", "database", "keyboard shortcuts"], "answer": 1, "time_limit": 20 },
            { "prompt": "How do you wire two components together?", "options": ["wac plug", "docker compose", "a REST call", "copy-paste"], "answer": 0, "time_limit": 20 },
            { "prompt": "Where is a component's storage backend chosen?", "options": ["hard-coded", "at link/deploy time", "in the browser", "never"], "answer": 1, "time_limit": 20 }
        ]
    });
    let _ = records::create(QUIZZES, &d.to_string(), &["host".to_string()]);
}

// ---- http plumbing ----------------------------------------------------------

fn hydrate(id: &str, data: &str) -> String {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    v["id"] = json!(id);
    v.to_string()
}

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        if it.next()? == key {
            Some(it.next().unwrap_or("").to_string())
        } else {
            None
        }
    })
}

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
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
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

fn emit(response_out: ResponseOutparam, result: Outcome) {
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
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in bytes.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
