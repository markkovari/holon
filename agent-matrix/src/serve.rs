//! The appservice HTTP side: Synapse pushes events here. Loopback or tailnet
//! only (the registration file says where); every request must carry the `hs_token`.

use std::net::SocketAddr;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::bridge::Bridge;

fn reply(req: Request, status: u16, v: Value) {
    let h = Header::from_bytes("content-type", "application/json").unwrap();
    let _ =
        req.respond(Response::from_string(v.to_string()).with_status_code(status).with_header(h));
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && (b[i + 1] as char).is_ascii_hexdigit()
            && (b[i + 2] as char).is_ascii_hexdigit()
        {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn authorised(req: &Request, query: &str, hs_token: &str) -> bool {
    let bearer = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("authorization"))
        .map(|h| h.value.as_str().to_string());
    bearer.as_deref() == Some(&format!("Bearer {hs_token}"))
        || query.split('&').any(|p| p == format!("access_token={hs_token}"))
}

/// Starts serving; returns the bound address. Events are handed to one worker
/// thread so they are processed in the order Synapse sent them.
pub fn serve(bridge: Arc<Bridge>) -> Result<SocketAddr, String> {
    let server =
        Server::http(&bridge.cfg.listen).map_err(|e| format!("bind {}: {e}", bridge.cfg.listen))?;
    let addr = server.server_addr().to_ip().ok_or("not an IP listener")?;
    let (tx, rx) = channel::<Vec<Value>>();
    {
        let b = bridge.clone();
        std::thread::spawn(move || {
            for events in rx {
                b.handle_events(&events);
            }
        });
    }
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let (b, tx) = (bridge.clone(), tx.clone());
            std::thread::spawn(move || handle(&b, &tx, req));
        }
    });
    Ok(addr)
}

fn handle(bridge: &Arc<Bridge>, tx: &Sender<Vec<Value>>, mut req: Request) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    // Synapse uses /_matrix/app/v1/...; older versions used the bare paths.
    let path = path
        .strip_prefix("/_matrix/app/v1")
        .or_else(|| path.strip_prefix("/_matrix/app/unstable"))
        .unwrap_or(path);
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let method = req.method().clone();

    if path == "/health" {
        return reply(req, 200, json!({"ok": true}));
    }
    if !authorised(&req, query, &bridge.cfg.hs_token) {
        return reply(req, 401, json!({"errcode": "M_UNKNOWN_TOKEN", "error": "bad hs_token"}));
    }
    match (&method, segs.as_slice()) {
        (Method::Put, ["transactions", _txn]) => {
            let mut body = String::new();
            let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
            let events = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["events"].as_array().cloned())
                .unwrap_or_default();
            if !events.is_empty() {
                let _ = tx.send(events);
            }
            reply(req, 200, json!({}))
        }
        // "Does this user exist?" — asked when someone refers to a user in our namespace
        // that was never registered, e.g. the owner inviting `@agent-coach` first.
        (Method::Get, ["users", user]) => {
            let user = percent_decode(user);
            let known = bridge.cfg.agent_of(&user).is_some_and(|a| {
                bridge.rt.agents().is_ok_and(|all| all.iter().any(|x| x.name == a))
            });
            if known
                && bridge
                    .mx
                    .register(user.trim_start_matches('@').split(':').next().unwrap_or_default())
                    .is_ok()
            {
                reply(req, 200, json!({}))
            } else {
                reply(req, 404, json!({"errcode": "M_NOT_FOUND", "error": "no such agent"}))
            }
        }
        (Method::Get, ["rooms", _alias]) => reply(req, 404, json!({"errcode": "M_NOT_FOUND"})),
        _ => reply(req, 404, json!({"errcode": "M_UNRECOGNIZED"})),
    }
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_user_ids_from_the_path() {
        assert_eq!(percent_decode("%40agent-x%3As.net"), "@agent-x:s.net");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
