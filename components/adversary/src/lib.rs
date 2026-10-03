//! `adversary` — the component ADR-0023 asks for.
//!
//! It runs two sweeps and counts what got through. Both are designed so that the
//! WRONG answer is loud: a store it should not have reached, or a connection it
//! should not have opened.
//!
//!   GET /sweep?neighbour=<tenant>/<app>   the whole thing, as JSON
//!   GET /work                             a trivial handler, so the same binary
//!                                         can generate the load the throughput
//!                                         half of the measurement needs
//!
//! `/work` exists because ADR-0023 insists the isolation number and the throughput
//! number come from ONE run. Measuring them separately is how you end up with a
//! density claim taken on an idle box and a safety claim taken on a quiet one.

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
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
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

/// Bucket names a leak would have used.
///
/// `default` is first because it is what every catalogue component hardcodes — it
/// MUST succeed, and it must resolve to this tenant's own store. Everything after
/// it must fail. A run where `default` fails is a broken host, not a secure one,
/// so the report distinguishes the two.
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

/// Targets, and whether reaching one is a finding.
///
/// The `control` entry is not padding. A host that refused everything would pass a
/// lateral-movement test while being useless, and the two are indistinguishable
/// unless something legitimate is expected to get through. So the sweep also dials
/// one ordinary public host: if it is allow-listed and still refused, the policy is
/// broken in the other direction, and that is worth knowing in the same run.
const TARGETS: &[(&str, bool)] = &[
    // (target, lateral) — `lateral` means reaching it is an escape.
    ("127.0.0.1:4222", true),     // the NATS bus this node is joined to
    ("localhost:4222", true),     // the same thing, by a name that resolves to it
    ("127.0.0.1:3401", true),     // this host's own listener — calling back in
    ("169.254.169.254:80", true), // cloud metadata: credentials
    ("100.64.0.1:3401", true),    // Tailscale CGNAT — the rest of the lattice
    ("100.127.255.254:80", true),
    ("10.0.0.1:80", true),
    ("192.168.1.1:80", true),
    ("[::1]:4222", true),
    ("example.com:80", false), // the control
];

struct Reached {
    /// Stores opened that are not this tenant's own.
    foreign_opens: u32,
    /// Bytes of a neighbour's data actually read.
    foreign_keys: u32,
    /// Connections that were established rather than refused.
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
                // An open is not yet a breach — `default` is supposed to work. What
                // matters is whether it reached SOMEONE ELSE'S data, so every handle
                // is used, not just counted.
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

/// Try to open a connection. Anything but a refusal is a finding.
///
/// Two refusal points, and they are deliberately distinguished: `http-request-denied`
/// is the NAME check (the authority is not on this app's allow-list), and any
/// other send error is the ADDRESS check (an allow-listed name that
/// resolves somewhere no tenant may reach). A design that only had the first would
/// pass a DNS entry pointed at the metadata endpoint.
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
                // Only a LATERAL connection is an escape. Reaching an allow-listed
                // public host is the policy working, not failing.
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

    // p3 has one `send` where p2 had `handle` + a future, so the NAME check shows
    // up as its own error code (`http-request-denied`) rather than a sync `Err`.
    match client::send(req).await {
        Ok(_resp) => Dial::Connected,
        Err(ErrorCode::HttpRequestDenied) => Dial::DeniedByName,
        // The host refused after resolving, or the connection failed on its own.
        // `wasi:http` gives one error channel for both, so the host's log is what
        // separates "prohibited" from "nobody listening" — the count that matters
        // (`connections`) is unaffected either way.
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
                // Deliberately trivial and deliberately touching the store, so the
                // load half of the run exercises the same capability path the
                // isolation half attacks.
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

/// `wasi:keyvalue` has no counter on `bucket`; the host's `atomics` does, but this
/// component deliberately does not import it — the fewer capabilities it holds, the
/// more the sweep says about the ones it does. A read-modify-write is fine here:
/// nothing about this measurement depends on the count being exact.
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
