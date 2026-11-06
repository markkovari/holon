//! `image-optimizer-domain` — shrink an uploaded picture and hand back the smaller one

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../image-optimizer/wit",
            "wit",
        ],
        world: "local:image-optimizer/domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
use bindings::media::image::optimizer;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::wasi::keyvalue::store;
use serde_json::json;

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Html(200, PAGE.to_string()),
            (Method::Post, ["api", "optimize"]) => match read_body(request).await {
                Err(()) => Outcome::Err(413, format!("an upload is at most {MAX_UPLOAD} bytes")),
                Ok(img) => match optimizer::optimize(&img) {
                    Ok(small) => Outcome::Image(small),
                    Err(optimizer::ImageError::NotAnImage(d)) => Outcome::Err(415, d),
                    Err(optimizer::ImageError::TooLarge(d)) => Outcome::Err(413, d),
                },
            },
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

/// The page posts the chosen file's bytes and shows what comes back.
const PAGE: &str = r#"<!DOCTYPE html><html><head><title>Image Optimizer</title></head><body><h1>Image Optimizer</h1><input type="file" id="f" accept="image/*"><div id="r"></div><script>document.getElementById("f").onchange=async e=>{const f=e.target.files[0];const r=await fetch("/api/optimize",{method:"POST",body:f});const o=document.getElementById("r");if(!r.ok){o.innerText=(await r.json()).error;return}const b=await r.blob();o.innerHTML="";o.append(f.size+" bytes to "+b.size,document.createElement("br"));const i=document.createElement("img");i.src=URL.createObjectURL(b);o.append(i)}</script></body></html>"#;

/// The component decodes at most 8 megapixels; a larger file would not get that far.
const MAX_UPLOAD: usize = 16 * 1024 * 1024;

enum Outcome {
    Html(u16, String),
    Image(Vec<u8>),
    Err(u16, String),
}

/// The optimizer answers in the format it was given.
fn image_type(b: &[u8]) -> &'static [u8] {
    match b {
        [0xff, 0xd8, ..] => b"image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => b"image/png",
        [b'G', b'I', b'F', ..] => b"image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => b"image/webp",
        _ => b"application/octet-stream",
    }
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b.into_bytes(), b"text/html".to_vec()),
        Outcome::Image(b) => {
            let t = image_type(&b).to_vec();
            (200, b, t)
        }
        Outcome::Err(c, m) => {
            (c, json!({ "error": m }).to_string().into_bytes(), b"application/json".to_vec())
        }
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type]);
    respond_with(code, headers, body)
}
bindings::export!(Component with_types_in bindings);

guestio::guest_p3_respond!();
guestio::guest_p3_read_body!(MAX_UPLOAD);
