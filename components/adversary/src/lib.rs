//! `adversary` — the component ADR-0023 asks for: two sweeps (store reach, egress) that

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "wit",
        ],
        world: "probe:adversary/adversary",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::p3::http::client;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response, Scheme};
use bindings::wasi::keyvalue::store as kv;

guestio::guest_p3_respond!();

struct Component;

/// `default` must open this tenant's own store: failing it means a broken host, not a secure one.
fn dictionary(neighbour: &str) -> Vec<String> {
    let (tenant, app) = neighbour.split_once('/').unwrap_or(("eve", "shop"));
    vec![
        "default".to_string(),
        // The exact string the host derives for the neighbour (`manifest::env_for`).
        format!("b-app-{tenant}-{app}"),
        format!("app-{tenant}-{app}"),
        format!("{tenant}-{app}"),
        tenant.to_string(),
        app.to_string(),
        neighbour.to_string(),
        // Shapes that a naive prefix scheme would let through.
        format!("b-app-{tenant}-{app}/"),
        format!("../b-app-{tenant}-{app}"),
        format!("default/../b-app-{tenant}-{app}"),
        "DEFAULT".to_string(),
        "default ".to_string(),
        " default".to_string(),
        "".to_string(),
        "*".to_string(),
        "%2e%2e".to_string(),
    ]
}

/// The `control` entry catches a host that refuses everything and so passes while useless.
const TARGETS: &[(&str, bool)] = &[
    // (target, lateral) — `lateral` means reaching it is an escape.
    ("127.0.0.1:4222", true), // the NATS bus this node is joined to
    ("localhost:4222", true),
    ("127.0.0.1:3401", true), // this host's own listener — calling back in
    ("169.254.169.254:80", true), // cloud metadata: credentials
    ("100.64.0.1:3401", true), // Tailscale CGNAT — the rest of the lattice
    ("100.127.255.254:80", true),
    ("10.0.0.1:80", true),
    ("192.168.1.1:80", true),
    ("[::1]:4222", true),
    ("example.com:80", false),
];

struct Reached {
    foreign_opens: u32,
    foreign_keys: u32,
    connections: u32,
}

fn sweep_stores(neighbour: &str, out: &mut String, r: &mut Reached) {
    out.push_str("\"stores\":[");
    let mut first = true;
    for name in dictionary(neighbour) {
        if !first {
            out.push(',');
        }
        first = false;
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        match kv::open(&name) {
            Ok(bucket) => {
                // An open is not a breach (`default` must work), so each handle is read to see whose data it reaches.
                let keys = bucket.list_keys(None).map(|k| k.keys.len()).unwrap_or(0);
                let own = name == "default";
                if !own {
                    r.foreign_opens += 1;
                    r.foreign_keys += keys as u32;
                }
                out.push_str(&format!(
                    "{{\"name\":\"{escaped}\",\"open\":\"ok\",\"keys\":{keys},\"expected\":{}}}",
                    if own { "true" } else { "false" }
                ));
            }
            Err(_) => out.push_str(&format!("{{\"name\":\"{escaped}\",\"open\":\"refused\"}}")),
        }
    }
    out.push(']');
}

/// `http-request-denied` is the name check; any other send error is the address check
/// (an allow-listed name resolving somewhere no tenant may reach).
async fn probe_egress(out: &mut String, r: &mut Reached) {
    out.push_str(",\"egress\":[");
    let mut first = true;
    for (target, lateral) in TARGETS {
        if !first {
            out.push(',');
        }
        first = false;
        let verdict = match dial(target).await {
            Dial::DeniedByName => "refused:name",
            Dial::DeniedByAddress => "refused:address",
            Dial::Connected => {
                // Only a lateral connection is an escape; reaching an allow-listed public host is the policy working.
                if *lateral {
                    r.connections += 1;
                }
                "connected"
            }
        };
        out.push_str(&format!(
            "{{\"target\":\"{target}\",\"lateral\":{lateral},\"result\":\"{verdict}\"}}"
        ));
    }
    out.push(']');
}

enum Dial {
    DeniedByName,
    DeniedByAddress,
    Connected,
}

async fn dial(authority: &str) -> Dial {
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (req, _sent) = Request::new(Fields::new(), None, trailers_rx, None);
    let _ = req.set_method(&Method::Get);
    let _ = req.set_scheme(Some(&Scheme::Http));
    let _ = req.set_authority(Some(authority));
    let _ = req.set_path_with_query(Some("/"));

    // In p3 the name check surfaces as its own error code, not a sync `Err`.
    match client::send(req).await {
        Ok(_resp) => Dial::Connected,
        Err(ErrorCode::HttpRequestDenied) => Dial::DeniedByName,
        // wasi:http has one error channel for address refusal and connect failure; the host log separates them.
        Err(_) => Dial::DeniedByAddress,
    }
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };

        let body = match route.as_str() {
            "/work" => {
                // Touches the store so the load half exercises the capability path the isolation half attacks.
                let n = kv::open("default").and_then(|b| b.increment_or_zero("hits")).unwrap_or(0);
                format!("{{\"ok\":true,\"hits\":{n}}}")
            }
            "/sweep" => {
                let neighbour = query
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .find(|(k, _)| *k == "neighbour")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_else(|| "eve/shop".to_string());
                let mut r = Reached { foreign_opens: 0, foreign_keys: 0, connections: 0 };
                let mut out = String::from("{");
                sweep_stores(&neighbour, &mut out, &mut r);
                probe_egress(&mut out, &mut r).await;
                out.push_str(&format!(
                    ",\"foreign_opens\":{},\"foreign_keys\":{},\"connections\":{},\"verdict\":\"{}\"}}",
                    r.foreign_opens,
                    r.foreign_keys,
                    r.connections,
                    if r.foreign_opens == 0 && r.foreign_keys == 0 && r.connections == 0 {
                        "contained"
                    } else {
                        "ESCAPED"
                    }
                ));
                out
            }
            _ => "{\"usage\":[\"/sweep?neighbour=<tenant>/<app>\",\"/work\"]}".to_string(),
        };

        respond(200, "application/json", body)
    }
}

/// No `atomics` import on purpose (fewer capabilities); an inexact read-modify-write count is fine.
trait CounterExt {
    fn increment_or_zero(&self, key: &str) -> Result<u64, kv::Error>;
}

impl CounterExt for kv::Bucket {
    fn increment_or_zero(&self, key: &str) -> Result<u64, kv::Error> {
        let cur = self
            .get(key)?
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let next = cur + 1;
        self.set(key, next.to_string().as_bytes())?;
        Ok(next)
    }
}

use bindings::p3::handler::Guest;

bindings::export!(Component with_types_in bindings);
