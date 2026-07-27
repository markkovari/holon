//! A deliberately unreliable HTTP upstream — the thing `mesh` protects callers
//! from. std only, no dependencies, ~100 lines.
//!
//! The CALLER decides how it misbehaves, per request, so both the demo and the
//! e2e are deterministic (no random failure percentages to flake on):
//!
//!   GET /hit                     -> 200
//!   GET /hit?fail=1              -> 500, always
//!   GET /hit?fail_n=2&id=x       -> 500 for the first 2 requests tagged `x`,
//!                                   200 after that (a blip a retry rides out)
//!   GET /hit?delay=400           -> 200, 400ms late (trips an SLO)
//!   GET /count?id=x              -> how many /hit requests were tagged `x`
//!                                   (does not count itself)
//!   GET /                        -> health, plus the total hit count
//!
//! "Upstream down" needs no flag: kill this process (or point mesh at a port
//! nothing listens on) and the host's outgoing handler gives a real
//! connect-refused.
//!
//! Run: `cargo run --release --bin flaky -- 127.0.0.1:3051` (or `just mesh-upstream`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

/// Per-`id` request counts — what makes `fail_n` (and so "the retry worked")
/// deterministic across a run.
fn hits() -> &'static Mutex<HashMap<String, u32>> {
    static H: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:3051".to_string());
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    eprintln!("flaky upstream on http://{addr} — /hit, /hit?fail=1, /hit?fail_n=2&id=x, /hit?delay=400, /count?id=x");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                thread::spawn(move || serve(s));
            }
            Err(e) => eprintln!("accept: {e}"),
        }
    }
}

fn serve(mut stream: TcpStream) {
    // Requests here have no body, so one read is enough to see the request line.
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target.as_str(), ""),
    };
    let q = |key: &str| -> Option<String> {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    };
    let num = |key: &str| q(key).and_then(|v| v.parse::<u64>().ok());
    let id = q("id").unwrap_or_else(|| "-".to_string());

    let (code, body) = match path {
        "/" => {
            let total: u32 = hits().lock().unwrap().values().sum();
            (200, format!(r#"{{"upstream":"flaky","hits":{total}}}"#))
        }
        "/count" => {
            let n = hits().lock().unwrap().get(&id).copied().unwrap_or(0);
            (200, format!(r#"{{"id":"{id}","hits":{n}}}"#))
        }
        "/hit" => {
            let hit = {
                let mut h = hits().lock().unwrap();
                let c = h.entry(id.clone()).or_insert(0);
                *c += 1;
                *c
            };
            if let Some(ms) = num("delay") {
                thread::sleep(Duration::from_millis(ms.min(10_000)));
            }
            let fail = q("fail").as_deref() == Some("1")
                || num("fail_n").is_some_and(|n| hit as u64 <= n);
            if fail {
                (500, format!(r#"{{"error":"flaky upstream","hit":{hit}}}"#))
            } else {
                (200, format!(r#"{{"ok":true,"hit":{hit}}}"#))
            }
        }
        _ => (404, r#"{"error":"not_found"}"#.to_string()),
    };

    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}
