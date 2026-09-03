//! `bench-suite` — the HTTP overhead ladder (see wit/bench.wit).
//!
//! Each endpoint adds exactly one layer over the previous, so diffing two
//! rows of a bench run isolates that layer's cost: router+invocation floor,
//! serde serialize, serde deserialize, one kv round-trip, two kv round-trips,
//! blobstore read, blobstore write+read.

#[allow(warnings)]
mod bindings;

use serde::{Deserialize, Serialize};

use bindings::wasi::blobstore::blobstore;
use bindings::wasi::blobstore::types::OutgoingValue;
use bindings::wasi::keyvalue::store as kv;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

#[derive(Serialize, Deserialize)]
struct Pet {
    name: String,
    species: String,
    age: u32,
}

const CONTAINER: &str = "bench";
const BLOB_KEY: &str = "bench-blob";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/");

        match (&method, route) {
            // 1. floor: router + invocation, no work.
            (Method::Get, "/ok") => respond(response_out, 200, "text/plain", b""),

            // 8. streaming probe: 10 SSE events, one every 200 ms. Headers are
            // sent before the first write, each event is flushed individually —
            // if the client sees them arrive spread over ~2 s the runtime
            // streams; if they all land at once it buffers.
            (Method::Get, "/sse") => {
                let headers = Fields::new();
                let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
                let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
                let response = OutgoingResponse::new(headers);
                let _ = response.set_status_code(200);
                let out = response.body().expect("outgoing body");
                ResponseOutparam::set(response_out, Ok(response));
                {
                    let stream = out.write().expect("write stream");
                    for i in 0..10u32 {
                        let event = format!("data: {{\"tick\":{i}}}\n\n");
                        if !write_all(&stream, event.as_bytes()) {
                            break; // client went away
                        }
                        sleep_ms(200);
                    }
                }
                let _ = OutgoingBody::finish(out, None);
            }

            // 2. + serde serialize of a static struct.
            (Method::Get, "/json") => {
                let pet = Pet { name: "Rex".into(), species: "dog".into(), age: 3 };
                let body = serde_json::to_string(&pet).unwrap();
                respond(response_out, 200, "application/json", body.as_bytes());
            }

            // 3. + serde deserialize (echo).
            (Method::Post, "/echo") => match parse_pet(&request) {
                Some(pet) => {
                    let body = serde_json::to_string(&pet).unwrap();
                    respond(response_out, 200, "application/json", body.as_bytes());
                }
                None => respond(response_out, 400, "text/plain", b"bad json"),
            },

            // 4. + one kv get (key seeded by /kv-rw; missing key still pays the trip).
            (Method::Post, "/kv-read") => match parse_pet(&request) {
                Some(pet) => match kv::open("bench") {
                    Ok(bucket) => {
                        let hit = matches!(bucket.get(&pet.name), Ok(Some(_)));
                        let body = format!("{{\"name\":\"{}\",\"hit\":{hit}}}", pet.name);
                        respond(response_out, 200, "application/json", body.as_bytes());
                    }
                    Err(_) => respond(response_out, 500, "text/plain", b"kv open failed"),
                },
                None => respond(response_out, 400, "text/plain", b"bad json"),
            },

            // 5. + kv set then get (two round-trips).
            (Method::Post, "/kv-rw") => match parse_pet(&request) {
                Some(pet) => match kv::open("bench") {
                    Ok(bucket) => {
                        let val = serde_json::to_vec(&pet).unwrap();
                        if bucket.set(&pet.name, &val).is_err() {
                            return respond(response_out, 500, "text/plain", b"kv set failed");
                        }
                        match bucket.get(&pet.name) {
                            Ok(Some(v)) => respond(response_out, 200, "application/json", &v),
                            _ => respond(response_out, 500, "text/plain", b"kv get failed"),
                        }
                    }
                    Err(_) => respond(response_out, 500, "text/plain", b"kv open failed"),
                },
                None => respond(response_out, 400, "text/plain", b"bad json"),
            },

            // 6. + blobstore read (blob seeded by /blob-rw).
            (Method::Post, "/blob-read") => match parse_pet(&request) {
                Some(_) => match container() {
                    Ok(c) => match c.get_data(BLOB_KEY, 0, u64::MAX) {
                        Ok(incoming) => {
                            let n = read_incoming(incoming);
                            let body = format!("{{\"bytes\":{n}}}");
                            respond(response_out, 200, "application/json", body.as_bytes());
                        }
                        Err(_) => respond(response_out, 404, "text/plain", b"no blob"),
                    },
                    Err(e) => respond(response_out, 500, "text/plain", e.as_bytes()),
                },
                None => respond(response_out, 400, "text/plain", b"bad json"),
            },

            // 7. + blobstore write (1 KiB) then read back.
            (Method::Post, "/blob-rw") => match parse_pet(&request) {
                Some(_) => match container() {
                    Ok(c) => {
                        let payload = vec![0x42u8; 1024];
                        let out = OutgoingValue::new_outgoing_value();
                        // register the sink FIRST, then stream the body into it.
                        if c.write_data(BLOB_KEY, &out).is_err() {
                            return respond(response_out, 500, "text/plain", b"blob write");
                        }
                        let Ok(stream) = out.outgoing_value_write_body() else {
                            return respond(response_out, 500, "text/plain", b"blob stream");
                        };
                        for chunk in payload.chunks(4096) {
                            if let Err(e) = stream.blocking_write_and_flush(chunk) {
                                return respond(
                                    response_out,
                                    500,
                                    "text/plain",
                                    format!("blob body write: {e:?}").as_bytes(),
                                );
                            }
                        }
                        drop(stream);
                        if OutgoingValue::finish(out).is_err() {
                            return respond(response_out, 500, "text/plain", b"blob finish");
                        }
                        match c.get_data(BLOB_KEY, 0, u64::MAX) {
                            Ok(incoming) => {
                                let n = read_incoming(incoming);
                                let body = format!("{{\"bytes\":{n}}}");
                                respond(response_out, 200, "application/json", body.as_bytes());
                            }
                            Err(_) => respond(response_out, 500, "text/plain", b"blob read"),
                        }
                    }
                    Err(e) => respond(response_out, 500, "text/plain", e.as_bytes()),
                },
                None => respond(response_out, 400, "text/plain", b"bad json"),
            },

            _ => respond(response_out, 404, "text/plain", b"not found"),
        }
    }
}

/// Total bytes in an incoming blob value, via the async (streamed) consumer —
/// the host's `consume-sync` reads into a zero-capacity buffer and always
/// returns empty (upstream wash-runtime NatsBlobstore bug).
fn read_incoming(incoming: bindings::wasi::blobstore::types::IncomingValue) -> usize {
    let Ok(stream) =
        bindings::wasi::blobstore::types::IncomingValue::incoming_value_consume_async(incoming)
    else {
        return 0;
    };
    let mut n = 0usize;
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => n += chunk.len(),
            Err(_) => break,
        }
    }
    n
}

fn container() -> Result<bindings::wasi::blobstore::container::Container, String> {
    blobstore::get_container(CONTAINER)
        .or_else(|_| blobstore::create_container(CONTAINER))
        .map_err(|e| format!("blobstore: {e}"))
}

fn parse_pet(request: &IncomingRequest) -> Option<Pet> {
    let body = read_body(request).ok()?;
    serde_json::from_slice(&body).ok()
}

fn sleep_ms(ms: u64) {
    bindings::wasi::clocks::monotonic_clock::subscribe_duration(ms * 1_000_000).block();
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: when this was measured, 148 of the tree's 150
/// components accumulated whatever arrived until the guest hit wasmtime's 64 MiB
/// per-store memory cap and TRAPPED, which reaches the caller as a closed
/// connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
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
