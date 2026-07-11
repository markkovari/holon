//! `bench-suite-p3` — the compute rungs of the HTTP overhead ladder on WASI p3.
//!
//! Same first three rungs as `bench-suite` (router floor, serde ser, serde
//! deser), but the handler is a p3 `async fn` returning a `Response` whose
//! body is a native `stream<u8>` — no outparams, no wasi:io, and one instance
//! serves concurrent requests.

mod bindings {
    wit_bindgen::generate!({
        generate_all,
    });
}

use serde::{Deserialize, Serialize};

use bindings::exports::wasi::http::handler::Guest as Handler;
use bindings::wasi::http::types::{ErrorCode, Fields, Method, Request, Response};

struct Component;

#[derive(Serialize, Deserialize)]
struct Pet {
    name: String,
    species: String,
    age: u32,
}

fn respond(status: u16, content_type: &str, body: Vec<u8>) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.append("content-type", content_type.as_bytes());
    let (mut tx, rx) = bindings::wit_stream::new();
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    wit_bindgen::spawn_local(async move {
        if !body.is_empty() {
            tx.write_all(body).await;
        }
        drop(tx);
        let _ = trailers_tx.write(Ok(None)).await;
    });
    let (response, _result) = Response::new(headers, Some(rx), trailers_rx);
    response
        .set_status_code(status)
        .map_err(|()| ErrorCode::InternalError(Some("set status".into())))?;
    Ok(response)
}

/// Consume the request body into memory (`res_tx` resolves Ok on drop).
async fn read_body(request: Request) -> Vec<u8> {
    let (res_tx, res_rx) = bindings::wit_future::new(|| Ok(()));
    let (body, _trailers) = Request::consume_body(request, res_rx);
    let data = body.collect().await;
    drop(res_tx);
    data
}

impl Handler for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_default();
        let route = path.split('?').next().unwrap_or("/");

        match (&method, route) {
            // 1. floor: router + invocation, no work.
            (Method::Get, "/ok") => respond(200, "text/plain", Vec::new()),

            // 2. + serde serialize of a static struct.
            (Method::Get, "/json") => {
                let pet = Pet { name: "Rex".into(), species: "dog".into(), age: 3 };
                respond(200, "application/json", serde_json::to_vec(&pet).unwrap())
            }

            // 3. + serde deserialize (echo).
            (Method::Post, "/echo") => {
                let body = read_body(request).await;
                match serde_json::from_slice::<Pet>(&body) {
                    Ok(pet) => {
                        respond(200, "application/json", serde_json::to_vec(&pet).unwrap())
                    }
                    Err(_) => respond(400, "text/plain", b"bad json".to_vec()),
                }
            }

            _ => respond(404, "text/plain", b"not found".to_vec()),
        }
    }
}

bindings::export!(Component with_types_in bindings);
