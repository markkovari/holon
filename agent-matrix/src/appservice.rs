//! What Synapse asks of an appservice, answered for whatever serves it: the component, or the
//! native tests' server. Synapse waits for one transaction's answer before it sends the next, so
//! events are handled in the order they were sent; a run they start is a job the pump delivers.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::bridge::Bridge;

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

fn authorised(authorization: Option<&str>, query: &str, hs_token: &str) -> bool {
    authorization == Some(format!("Bearer {hs_token}").as_str())
        || query.split('&').any(|p| p == format!("access_token={hs_token}"))
}

/// `url` is the path and query as requested. Answers a status and a JSON body.
pub fn handle(
    bridge: &Arc<Bridge>,
    method: &str,
    url: &str,
    authorization: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    // Synapse uses /_matrix/app/v1/...; older versions used the bare paths.
    let path = path
        .strip_prefix("/_matrix/app/v1")
        .or_else(|| path.strip_prefix("/_matrix/app/unstable"))
        .unwrap_or(path);
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();

    if path == "/health" {
        return (200, json!({"ok": true}));
    }
    if !authorised(authorization, query, &bridge.cfg.hs_token) {
        return (401, json!({"errcode": "M_UNKNOWN_TOKEN", "error": "bad hs_token"}));
    }
    match (method, segs.as_slice()) {
        ("PUT", ["transactions", _txn]) => {
            let events = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|v| v["events"].as_array().cloned())
                .unwrap_or_default();
            bridge.handle_events(&events);
            (200, json!({}))
        }
        // Synapse's user query for a never-registered id, e.g. the owner inviting `@agent-coach` first.
        ("GET", ["users", user]) => {
            let user = percent_decode(user);
            let known = bridge.cfg.agent_of(&user).is_some_and(|a| {
                bridge.rt.agents().is_ok_and(|all| all.iter().any(|x| x.name == a))
            });
            let localpart = user.trim_start_matches('@').split(':').next().unwrap_or_default();
            if known && bridge.mx.register(localpart).is_ok() {
                (200, json!({}))
            } else {
                (404, json!({"errcode": "M_NOT_FOUND", "error": "no such agent"}))
            }
        }
        ("GET", ["rooms", _alias]) => (404, json!({"errcode": "M_NOT_FOUND"})),
        // The rooms the board links to, in the shape the native bridge's state.json had.
        ("GET", ["internal", "rooms"]) => bridge.state.read(|d| {
            (200, json!({"control_room": d.control_room, "dms": d.dms, "projects": d.projects}))
        }),
        // Once, moving from the native bridge: its state file, taken only into an empty store.
        ("PUT", ["internal", "state"]) => match bridge.state.import(body) {
            Ok(()) => (200, json!({"imported": true})),
            Err(e) => (409, json!({"errcode": "M_UNKNOWN", "error": e})),
        },
        _ => (404, json!({"errcode": "M_UNRECOGNIZED"})),
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
