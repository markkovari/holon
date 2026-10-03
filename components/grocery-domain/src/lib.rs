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
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../barcode-read/wit",
            "../../wit/ui-assets",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "wit",
        ],
        world: "grocery:app/grocery-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

pub mod auth;
pub mod catalog;
pub mod orders;
pub mod scanner;
pub mod store;
pub mod types;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::ui::assets::files as statics;
use serde_json::json;
use types::Outcome;

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_p3_read_body!(MAX_BODY_BYTES);

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        // Support OPTIONS for CORS preflight
        if let Method::Options = method {
            return respond_cors(204, "text/plain", Vec::new());
        }

        let is_get_or_head = matches!(method, Method::Get | Method::Head);

        let outcome = match (&method, seg.as_slice()) {
            // Health / API status
            (Method::Get, ["api", "health"]) => {
                Outcome::Json(200, json!({ "status": "ok", "service": "grocery" }).to_string())
            }

            // Auth Endpoints
            (Method::Post, ["api", "auth", "register"]) => auth::handle_register(request).await,
            (Method::Post, ["api", "auth", "login"]) => auth::handle_login(request).await,
            (Method::Get, ["api", "auth", "me"]) | (Method::Get, ["auth", "me"]) => {
                auth::handle_auth_me(&request)
            }
            (Method::Post, ["api", "auth", "logout"]) => auth::handle_logout(&request),

            // Admin User Management Endpoints (RBAC Admin-Only)
            (Method::Get, ["api", "admin", "users"]) => auth::handle_list_users(&request),
            (Method::Post, ["api", "admin", "users"]) => {
                auth::handle_admin_create_user(request).await
            }
            (Method::Patch, ["api", "admin", "users", id, "role"]) => {
                auth::handle_update_user_role(request, id).await
            }
            (Method::Delete, ["api", "admin", "users", id]) => {
                auth::handle_delete_user(&request, id)
            }

            // Real WASI Barcode Decoding (Allow Shoppers, Admins & In-Store Kiosk)
            (Method::Post, ["api", "scan"]) => scanner::scan_barcode(request).await,

            // Products Catalog
            (Method::Get, ["api", "products"]) => catalog::list_products(),
            (Method::Post, ["api", "products"]) => {
                if let Err(e) = auth::require_role(&request, "admin") {
                    e
                } else {
                    catalog::register_product(request).await
                }
            }
            (Method::Patch, ["api", "products", barcode, "stock"]) => {
                if let Err(e) = auth::require_role(&request, "admin") {
                    e
                } else {
                    catalog::adjust_stock(request, barcode).await
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
            (Method::Post, ["api", "cart", "items"]) => orders::add_cart_item(request).await,
            (Method::Delete, ["api", "cart", "items", barcode]) => {
                orders::remove_cart_item(barcode)
            }
            (Method::Post, ["api", "checkout"]) => orders::checkout(request).await,

            // Serve real fixture images for browser tests
            (Method::Get, ["fixtures", filename]) => match scanner::get_fixture(filename) {
                Some(bytes) => Outcome::File(200, "image/png".into(), bytes.to_vec()),
                None => Outcome::Err(404, "Fixture not found".into()),
            },

            // Non-API GETs and HEADs -> Embedded React SPA via ui:assets/files
            _ if is_get_or_head => serve_static(&route),

            _ => Outcome::Err(404, "Endpoint not found".into()),
        };

        emit(outcome)
    }
}

/// Serve the baked React SPA via ui:assets: exact path, or fall back to /index.html
fn serve_static(route: &str) -> Outcome {
    if route.contains("..") {
        return Outcome::Err(400, "Invalid path: Directory traversal not permitted".into());
    }
    let want = if route == "/" || route.is_empty() { "/index.html" } else { route };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(asset) => Outcome::File(200, asset.content_type, asset.body),
        None => Outcome::Err(404, "Static asset not found".into()),
    }
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    match result {
        Outcome::File(code, ctype, bytes) => respond_cors(code, &ctype, bytes),
        Outcome::Json(code, json_str) => {
            respond_cors(code, "application/json; charset=utf-8", json_str.into_bytes())
        }
        Outcome::Err(code, msg) => {
            let body = json!({ "error": msg }).to_string();
            respond_cors(code, "application/json; charset=utf-8", body.into_bytes())
        }
    }
}

fn respond_cors(status: u16, ctype: &str, body: Vec<u8>) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let _ = headers
        .set("access-control-allow-methods", &[b"GET, POST, PATCH, DELETE, OPTIONS".to_vec()]);
    let _ = headers.set("access-control-allow-headers", &[b"Content-Type, Authorization".to_vec()]);
    respond_with(status, headers, body)
}

bindings::export!(Component with_types_in bindings);
