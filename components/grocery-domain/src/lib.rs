//! `grocery-domain` — Grocery shop app as ONE composed wasm HTTP component.
//!
//! Exports `wasi:http/incoming-handler@0.2.0`;
//! Imports:
//!   - `barcode:read/reader@0.1.0`: real linear barcode decoding from PNG bytes (pure compute WASI component)
//!   - `ui:assets/files@0.1.0`: embedded React SPA bundle
//!   - `wasi:keyvalue/store@0.2.0-draft`: persistence across requests
//!   - `wasi:clocks/wall-clock@0.2.0`: timestamps
//!
//! ZERO MOCKING: Image bytes posted to `/api/scan` are read directly by the Rust
//! scanline decoder in `components/barcode-read`. Real RBAC identity & session management.

#[allow(warnings)]
mod bindings;

pub mod auth;
pub mod catalog;
pub mod orders;
pub mod scanner;
pub mod store;
pub mod types;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::ui::assets::files as statics;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;
use types::Outcome;

guestio::guest_bearer!();
guestio::guest_write_all!();

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_read_body!(MAX_BODY_BYTES);

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        // Support OPTIONS for CORS preflight
        if let Method::Options = method {
            respond(response_out, 204, "text/plain", &[]);
            return;
        }

        let is_get_or_head = matches!(method, Method::Get | Method::Head);

        let outcome = match (&method, seg.as_slice()) {
            // Health / API status
            (Method::Get, ["api", "health"]) => {
                Outcome::Json(200, json!({ "status": "ok", "service": "grocery" }).to_string())
            }

            // Auth Endpoints
            (Method::Post, ["api", "auth", "register"]) => auth::handle_register(&request),
            (Method::Post, ["api", "auth", "login"]) => auth::handle_login(&request),
            (Method::Get, ["api", "auth", "me"]) | (Method::Get, ["auth", "me"]) => {
                auth::handle_auth_me(&request)
            }
            (Method::Post, ["api", "auth", "logout"]) => auth::handle_logout(&request),

            // Admin User Management Endpoints (RBAC Admin-Only)
            (Method::Get, ["api", "admin", "users"]) => auth::handle_list_users(&request),
            (Method::Post, ["api", "admin", "users"]) => auth::handle_admin_create_user(&request),
            (Method::Patch, ["api", "admin", "users", id, "role"]) => {
                auth::handle_update_user_role(&request, id)
            }
            (Method::Delete, ["api", "admin", "users", id]) => {
                auth::handle_delete_user(&request, id)
            }

            // Real WASI Barcode Decoding (Allow Shoppers, Admins & In-Store Kiosk)
            (Method::Post, ["api", "scan"]) => scanner::scan_barcode(&request),

            // Products Catalog
            (Method::Get, ["api", "products"]) => catalog::list_products(),
            (Method::Post, ["api", "products"]) => {
                if let Err(e) = auth::require_role(&request, "admin") {
                    e
                } else {
                    catalog::register_product(&request)
                }
            }
            (Method::Patch, ["api", "products", barcode, "stock"]) => {
                if let Err(e) = auth::require_role(&request, "admin") {
                    e
                } else {
                    catalog::adjust_stock(&request, barcode)
                }
            }

            // Low Stock Alerts (Admin-Only RBAC)
            (Method::Get, ["api", "alerts"]) => {
                if let Err(e) = auth::require_role(&request, "admin") {
                    e
                } else {
                    catalog::list_alerts()
                }
            }

            // Cart and Checkout
            (Method::Get, ["api", "cart"]) => orders::get_cart(),
            (Method::Post, ["api", "cart", "items"]) => orders::add_cart_item(&request),
            (Method::Delete, ["api", "cart", "items", barcode]) => {
                orders::remove_cart_item(barcode)
            }
            (Method::Post, ["api", "checkout"]) => orders::checkout(&request),

            // Serve real fixture images for browser tests
            (Method::Get, ["fixtures", filename]) => match scanner::get_fixture(filename) {
                Some(bytes) => Outcome::File(200, "image/png".into(), bytes.to_vec()),
                None => Outcome::Err(404, "Fixture not found".into()),
            },

            // Non-API GETs and HEADs -> Embedded React SPA via ui:assets/files
            _ if is_get_or_head => serve_static(&route),

            _ => Outcome::Err(404, "Endpoint not found".into()),
        };

        emit(response_out, outcome);
    }
}

/// Serve the baked React SPA via ui:assets: exact path, or fall back to /index.html
fn serve_static(route: &str) -> Outcome {
    if route.contains("..") {
        return Outcome::Err(400, "Invalid path: Directory traversal not permitted".into());
    }
    let want = if route == "/" || route.is_empty() {
        "/index.html"
    } else {
        route
    };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(asset) => Outcome::File(200, asset.content_type, asset.body),
        None => Outcome::Err(404, "Static asset not found".into()),
    }
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::File(code, ctype, bytes) => {
            respond(response_out, code, &ctype, &bytes);
        }
        Outcome::Json(code, json_str) => {
            respond(
                response_out,
                code,
                "application/json; charset=utf-8",
                json_str.as_bytes(),
            );
        }
        Outcome::Err(code, msg) => {
            let body = json!({ "error": msg }).to_string();
            respond(
                response_out,
                code,
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let _ = headers.set(
        "access-control-allow-methods",
        &[b"GET, POST, PATCH, DELETE, OPTIONS".to_vec()],
    );
    let _ = headers.set(
        "access-control-allow-headers",
        &[b"Content-Type, Authorization".to_vec()],
    );
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
