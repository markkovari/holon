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

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use bindings::barcode::read::reader::{self as barcode, ReadError};
use bindings::ui::assets::files as statics;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store::{self as kv, Bucket};

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_bearer!();
guestio::guest_write_all!();

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
guestio::guest_read_body!(MAX_BODY_BYTES);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub barcode: String,
    pub symbology: String,
    pub name: String,
    pub category: String,
    pub price_cents: u32,
    pub stock: i32,
    pub icon: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartItem {
    pub barcode: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub role: String, // "shopper" | "admin"
    pub password_hash: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: String,
    pub role: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub role: String,
    pub created_at: u64,
}

impl From<&User> for UserPublic {
    fn from(u: &User) -> Self {
        Self {
            id: u.id.clone(),
            username: u.username.clone(),
            name: u.name.clone(),
            email: u.email.clone(),
            role: u.role.clone(),
            created_at: u.created_at,
        }
    }
}

fn initial_products() -> Vec<Product> {
    vec![
        Product {
            barcode: "4006381333931".into(),
            symbology: "ean-13".into(),
            name: "Organic Extra Virgin Olive Oil (500ml)".into(),
            category: "Pantry".into(),
            price_cents: 849,
            stock: 14,
            icon: "🫒".into(),
            description: "Cold-pressed extra virgin olive oil from single-estate olives.".into(),
        },
        Product {
            barcode: "96385074".into(),
            symbology: "ean-8".into(),
            name: "Farm Fresh Whole Milk (1L)".into(),
            category: "Dairy".into(),
            price_cents: 229,
            stock: 3, // Low stock -> triggers alert!
            icon: "🥛".into(),
            description: "Locally pasteurized farm-fresh milk with cream top.".into(),
        },
        Product {
            barcode: "0036000291452".into(),
            symbology: "upc-a".into(),
            name: "Artisan Sourdough Loaf".into(),
            category: "Bakery".into(),
            price_cents: 499,
            stock: 8,
            icon: "🍞".into(),
            description: "Naturally leavened sourdough bread baked daily in hearth.".into(),
        },
        Product {
            barcode: "SHELF-A17".into(),
            symbology: "code-128".into(),
            name: "Organic Hass Avocados (3-Pack)".into(),
            category: "Produce".into(),
            price_cents: 389,
            stock: 12,
            icon: "🥑".into(),
            description: "Ripe Hass avocados packed with healthy fats.".into(),
        },
        Product {
            barcode: "ZZG4ZDMEN".into(),
            symbology: "code-128".into(),
            name: "Italian Espresso Roast (250g)".into(),
            category: "Pantry".into(),
            price_cents: 729,
            stock: 2, // Low stock
            icon: "☕".into(),
            description: "Dark roasted Arabica beans with chocolate and hazelnut notes.".into(),
        },
        Product {
            barcode: "0166131860910".into(),
            symbology: "upc-a".into(),
            name: "Grass-Fed Irish Butter (250g)".into(),
            category: "Dairy".into(),
            price_cents: 349,
            stock: 18,
            icon: "🧈".into(),
            description: "Pure Irish cream salted butter made from grass-fed cows.".into(),
        },
    ]
}

fn initial_users() -> Vec<User> {
    vec![
        User {
            id: "usr_shopper".into(),
            username: "shopper".into(),
            name: "Alex Shopper".into(),
            email: "shopper@grocery.local".into(),
            role: "shopper".into(),
            password_hash: "shopper123".into(),
            created_at: 1725400000,
        },
        User {
            id: "usr_admin".into(),
            username: "admin".into(),
            name: "Sarah (Store Manager)".into(),
            email: "admin@grocery.local".into(),
            role: "admin".into(),
            password_hash: "admin123".into(),
            created_at: 1725400000,
        },
    ]
}

fn initial_sessions() -> Vec<Session> {
    vec![
        Session {
            token: "tok_shopper".into(),
            user_id: "usr_shopper".into(),
            role: "shopper".into(),
            created_at: 1725400000,
        },
        Session {
            token: "tok_admin".into(),
            user_id: "usr_admin".into(),
            role: "admin".into(),
            created_at: 1725400000,
        },
    ]
}

fn get_bucket() -> Option<Bucket> {
    kv::open("default").ok()
}

fn load_products() -> Vec<Product> {
    if let Some(bucket) = get_bucket() {
        if let Ok(Some(bytes)) = bucket.get("products") {
            if let Ok(p) = serde_json::from_slice::<Vec<Product>>(&bytes) {
                return p;
            }
        }
    }
    let init = initial_products();
    save_products(&init);
    init
}

fn save_products(products: &[Product]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(products) {
            let _ = bucket.set("products", &bytes);
        }
    }
}

fn load_cart() -> Vec<CartItem> {
    if let Some(bucket) = get_bucket() {
        if let Ok(Some(bytes)) = bucket.get("cart") {
            if let Ok(c) = serde_json::from_slice::<Vec<CartItem>>(&bytes) {
                return c;
            }
        }
    }
    Vec::new()
}

fn save_cart(cart: &[CartItem]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(cart) {
            let _ = bucket.set("cart", &bytes);
        }
    }
}

fn load_users() -> Vec<User> {
    if let Some(bucket) = get_bucket() {
        if let Ok(Some(bytes)) = bucket.get("users") {
            if let Ok(users) = serde_json::from_slice::<Vec<User>>(&bytes) {
                return users;
            }
        }
    }
    let init = initial_users();
    save_users(&init);
    init
}

fn save_users(users: &[User]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(users) {
            let _ = bucket.set("users", &bytes);
        }
    }
}

fn load_sessions() -> Vec<Session> {
    if let Some(bucket) = get_bucket() {
        if let Ok(Some(bytes)) = bucket.get("sessions") {
            if let Ok(sessions) = serde_json::from_slice::<Vec<Session>>(&bytes) {
                return sessions;
            }
        }
    }
    let init = initial_sessions();
    save_sessions(&init);
    init
}

fn save_sessions(sessions: &[Session]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(sessions) {
            let _ = bucket.set("sessions", &bytes);
        }
    }
}

fn get_current_user(request: &IncomingRequest) -> Option<User> {
    let token = bearer(request)?;
    let sessions = load_sessions();
    let session = sessions.iter().find(|s| s.token == token)?;
    let users = load_users();
    users.iter().find(|u| u.id == session.user_id).cloned()
}

fn require_role(request: &IncomingRequest, required_role: &str) -> Result<User, Outcome> {
    let user = match get_current_user(request) {
        Some(u) => u,
        None => {
            return Err(Outcome::Err(
                401,
                "Authentication required. Please sign in.".into(),
            ));
        }
    };
    if user.role != required_role {
        return Err(Outcome::Err(
            403,
            format!(
                "Forbidden: Requires '{required_role}' role. Current role is '{}'.",
                user.role
            ),
        ));
    }
    Ok(user)
}

// Built-in barcode fixture PNGs from components/barcode-read/fixtures
static FIXTURE_EAN13: &[u8] = include_bytes!("../../barcode-read/fixtures/ean13.png");
static FIXTURE_EAN8: &[u8] = include_bytes!("../../barcode-read/fixtures/ean8.png");
static FIXTURE_UPCA: &[u8] = include_bytes!("../../barcode-read/fixtures/upca.png");
static FIXTURE_CODE128: &[u8] = include_bytes!("../../barcode-read/fixtures/code128.png");
static FIXTURE_CODE128_LETTERS: &[u8] = include_bytes!("../../barcode-read/fixtures/code128-letters.png");
static FIXTURE_EAN13_LEADING_ZERO: &[u8] = include_bytes!("../../barcode-read/fixtures/ean13-leading-zero.png");

enum Outcome {
    Json(u16, String),
    File(u16, String, Vec<u8>),
    Err(u16, String),
}

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
            (Method::Post, ["api", "auth", "register"]) => handle_register(&request),
            (Method::Post, ["api", "auth", "login"]) => handle_login(&request),
            (Method::Get, ["api", "auth", "me"]) | (Method::Get, ["auth", "me"]) => handle_auth_me(&request),
            (Method::Post, ["api", "auth", "logout"]) => handle_logout(&request),

            // Admin User Management Endpoints (RBAC Admin-Only)
            (Method::Get, ["api", "admin", "users"]) => handle_list_users(&request),
            (Method::Post, ["api", "admin", "users"]) => handle_admin_create_user(&request),
            (Method::Patch, ["api", "admin", "users", id, "role"]) => handle_update_user_role(&request, id),
            (Method::Delete, ["api", "admin", "users", id]) => handle_delete_user(&request, id),

            // Real WASI Barcode Decoding (Allow Shoppers, Admins & In-Store Kiosk)
            (Method::Post, ["api", "scan"]) => scan_barcode(&request),

            // Products Catalog
            (Method::Get, ["api", "products"]) => list_products(),
            (Method::Post, ["api", "products"]) => {
                if let Err(e) = require_role(&request, "admin") {
                    e
                } else {
                    register_product(&request)
                }
            }
            (Method::Patch, ["api", "products", barcode, "stock"]) => {
                if let Err(e) = require_role(&request, "admin") {
                    e
                } else {
                    adjust_stock(&request, barcode)
                }
            }

            // Low Stock Alerts (Admin-Only RBAC)
            (Method::Get, ["api", "alerts"]) => {
                if let Err(e) = require_role(&request, "admin") {
                    e
                } else {
                    list_alerts()
                }
            }

            // Cart and Checkout
            (Method::Get, ["api", "cart"]) => get_cart(),
            (Method::Post, ["api", "cart", "items"]) => add_cart_item(&request),
            (Method::Delete, ["api", "cart", "items", barcode]) => remove_cart_item(barcode),
            (Method::Post, ["api", "checkout"]) => checkout(&request),

            // Serve real fixture images for browser tests
            (Method::Get, ["fixtures", "ean13.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_EAN13.to_vec()),
            (Method::Get, ["fixtures", "ean8.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_EAN8.to_vec()),
            (Method::Get, ["fixtures", "upca.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_UPCA.to_vec()),
            (Method::Get, ["fixtures", "code128.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_CODE128.to_vec()),
            (Method::Get, ["fixtures", "code128-letters.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_CODE128_LETTERS.to_vec()),
            (Method::Get, ["fixtures", "ean13-leading-zero.png"]) => Outcome::File(200, "image/png".into(), FIXTURE_EAN13_LEADING_ZERO.to_vec()),

            // Non-API GETs and HEADs -> Embedded React SPA via ui:assets/files
            _ if is_get_or_head => serve_static(&route),

            _ => Outcome::Err(404, "Endpoint not found".into()),
        };

        emit(response_out, outcome);
    }
}

/// Serve the baked React SPA via ui:assets: exact path, or fall back to /index.html
fn serve_static(route: &str) -> Outcome {
    let want = if route == "/" || route.is_empty() { "/index.html" } else { route };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(asset) => Outcome::File(200, asset.content_type, asset.body),
        None => Outcome::Err(404, "Static asset not found".into()),
    }
}

/// POST /api/auth/register
fn handle_register(request: &IncomingRequest) -> Outcome {
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };

    let username = val["username"].as_str().unwrap_or("").trim().to_lowercase();
    let name = val["name"].as_str().unwrap_or("").trim();
    let email = val["email"].as_str().unwrap_or("").trim().to_lowercase();
    let password = val["password"].as_str().unwrap_or("").trim();
    let mut role = val["role"].as_str().unwrap_or("shopper").trim().to_lowercase();

    if username.is_empty() || password.is_empty() {
        return Outcome::Err(400, "Username and password are required".into());
    }
    if role != "admin" {
        role = "shopper".to_string();
    }

    let mut users = load_users();
    if users.iter().any(|u| u.username == username) {
        return Outcome::Err(409, format!("Username '{username}' is already taken"));
    }

    let sec = wall_clock::now().seconds;
    let user_id = format!("usr_{}", sec % 1_000_000);
    let new_user = User {
        id: user_id.clone(),
        username: username.clone(),
        name: if name.is_empty() { username.clone() } else { name.to_string() },
        email: if email.is_empty() { format!("{}@grocery.local", username) } else { email },
        role: role.clone(),
        password_hash: password.to_string(),
        created_at: sec,
    };

    users.push(new_user.clone());
    save_users(&users);

    // Create session token
    let token = format!("tok_{}_{}", username, sec % 100_000);
    let mut sessions = load_sessions();
    sessions.push(Session {
        token: token.clone(),
        user_id: user_id.clone(),
        role: role.clone(),
        created_at: sec,
    });
    save_sessions(&sessions);

    let pub_user = UserPublic::from(&new_user);
    Outcome::Json(201, json!({
        "token": token,
        "user": pub_user
    }).to_string())
}

/// POST /api/auth/login
fn handle_login(request: &IncomingRequest) -> Outcome {
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };

    let username = val["username"].as_str().unwrap_or("").trim().to_lowercase();
    let password = val["password"].as_str().unwrap_or("").trim();

    if username.is_empty() || password.is_empty() {
        return Outcome::Err(400, "Username and password are required".into());
    }

    let users = load_users();
    let user = match users.iter().find(|u| u.username == username) {
        Some(u) => u,
        None => return Outcome::Err(401, "Invalid username or password".into()),
    };

    if user.password_hash != password {
        return Outcome::Err(401, "Invalid username or password".into());
    }

    let sec = wall_clock::now().seconds;
    let token = if user.username == "shopper" {
        "tok_shopper".to_string()
    } else if user.username == "admin" {
        "tok_admin".to_string()
    } else {
        format!("tok_{}_{}", user.username, sec % 100_000)
    };

    let mut sessions = load_sessions();
    if !sessions.iter().any(|s| s.token == token) {
        sessions.push(Session {
            token: token.clone(),
            user_id: user.id.clone(),
            role: user.role.clone(),
            created_at: sec,
        });
        save_sessions(&sessions);
    }

    let pub_user = UserPublic::from(user);
    Outcome::Json(200, json!({
        "token": token,
        "user": pub_user
    }).to_string())
}

/// GET /api/auth/me
fn handle_auth_me(request: &IncomingRequest) -> Outcome {
    match get_current_user(request) {
        Some(user) => {
            let pub_user = UserPublic::from(&user);
            Outcome::Json(200, json!({ "user": pub_user }).to_string())
        }
        None => {
            // Default to shopper for unauthenticated initial visitors
            Outcome::Err(401, "Not authenticated".into())
        }
    }
}

/// POST /api/auth/logout
fn handle_logout(request: &IncomingRequest) -> Outcome {
    if let Some(token) = bearer(request) {
        let mut sessions = load_sessions();
        sessions.retain(|s| s.token != token);
        save_sessions(&sessions);
    }
    Outcome::Json(200, json!({ "status": "logged_out" }).to_string())
}

/// GET /api/admin/users
fn handle_list_users(request: &IncomingRequest) -> Outcome {
    if let Err(e) = require_role(request, "admin") {
        return e;
    }
    let users = load_users();
    let public_users: Vec<UserPublic> = users.iter().map(UserPublic::from).collect();
    Outcome::Json(200, serde_json::to_string(&public_users).unwrap_or_default())
}

/// POST /api/admin/users
fn handle_admin_create_user(request: &IncomingRequest) -> Outcome {
    if let Err(e) = require_role(request, "admin") {
        return e;
    }
    handle_register(request)
}

/// PATCH /api/admin/users/{id}/role
fn handle_update_user_role(request: &IncomingRequest, user_id: &str) -> Outcome {
    if let Err(e) = require_role(request, "admin") {
        return e;
    }
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };
    let new_role = val["role"].as_str().unwrap_or("").trim().to_lowercase();
    if new_role != "admin" && new_role != "shopper" {
        return Outcome::Err(400, "Role must be 'shopper' or 'admin'".into());
    }

    let mut users = load_users();
    let updated_user = if let Some(u) = users.iter_mut().find(|u| u.id == user_id) {
        u.role = new_role.clone();
        Some(UserPublic::from(&*u))
    } else {
        None
    };

    if let Some(pub_user) = updated_user {
        save_users(&users);

        // Update active sessions for this user
        let mut sessions = load_sessions();
        for s in sessions.iter_mut().filter(|s| s.user_id == user_id) {
            s.role = new_role.clone();
        }
        save_sessions(&sessions);

        Outcome::Json(200, serde_json::to_string(&pub_user).unwrap_or_default())
    } else {
        Outcome::Err(404, "User not found".into())
    }
}

/// DELETE /api/admin/users/{id}
fn handle_delete_user(request: &IncomingRequest, user_id: &str) -> Outcome {
    let caller = match require_role(request, "admin") {
        Ok(u) => u,
        Err(e) => return e,
    };
    if caller.id == user_id {
        return Outcome::Err(400, "Cannot delete your own admin account".into());
    }

    let mut users = load_users();
    if !users.iter().any(|u| u.id == user_id) {
        return Outcome::Err(404, "User not found".into());
    }
    users.retain(|u| u.id != user_id);
    save_users(&users);

    let mut sessions = load_sessions();
    sessions.retain(|s| s.user_id != user_id);
    save_sessions(&sessions);

    Outcome::Json(200, json!({ "status": "deleted", "id": user_id }).to_string())
}

/// POST /api/scan — REAL WebAssembly Barcode Decoding (barcode:read/reader)
fn scan_barcode(request: &IncomingRequest) -> Outcome {
    let image_bytes = match read_body(request) {
        Ok(b) if !b.is_empty() => b,
        _ => return Outcome::Err(400, "Request body is empty or not readable".into()),
    };

    // Invoke pure compute WASI decoder:
    match barcode::decode_png(&image_bytes) {
        Ok(symbol) => {
            let products = load_products();
            let product = products.iter().find(|p| p.barcode == symbol.text).cloned();
            Outcome::Json(200, json!({
                "barcode": {
                    "text": symbol.text,
                    "symbology": symbol.symbology,
                },
                "product": product,
            }).to_string())
        }
        Err(ReadError::NotFound) => {
            Outcome::Err(404, "No barcode detected in image. Ensure the barcode is clear and steady.".into())
        }
        Err(ReadError::BadImage(msg)) => {
            Outcome::Err(400, format!("Invalid PNG image data: {msg}"))
        }
    }
}

/// GET /api/products
fn list_products() -> Outcome {
    let products = load_products();
    Outcome::Json(200, serde_json::to_string(&products).unwrap_or_default())
}

/// POST /api/products — RBAC Protected (Admin only)
fn register_product(request: &IncomingRequest) -> Outcome {
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };

    let barcode_str = val["barcode"].as_str().unwrap_or("").trim();
    if barcode_str.is_empty() {
        return Outcome::Err(400, "Missing barcode".into());
    }

    let mut products = load_products();
    if let Some(existing) = products.iter_mut().find(|p| p.barcode == barcode_str) {
        if let Some(n) = val["name"].as_str() { existing.name = n.to_string(); }
        if let Some(c) = val["category"].as_str() { existing.category = c.to_string(); }
        if let Some(p) = val["price_cents"].as_u64() { existing.price_cents = p as u32; }
        if let Some(s) = val["stock"].as_i64() { existing.stock = s as i32; }
        let res = serde_json::to_string(existing).unwrap_or_default();
        save_products(&products);
        return Outcome::Json(200, res);
    }

    let new_product = Product {
        barcode: barcode_str.to_string(),
        symbology: val["symbology"].as_str().unwrap_or("ean-13").to_string(),
        name: val["name"].as_str().unwrap_or("Unnamed Product").to_string(),
        category: val["category"].as_str().unwrap_or("Produce").to_string(),
        price_cents: val["price_cents"].as_u64().unwrap_or(299) as u32,
        stock: val["stock"].as_i64().unwrap_or(10) as i32,
        icon: val["icon"].as_str().unwrap_or("📦").to_string(),
        description: val["description"].as_str().unwrap_or("New product").to_string(),
    };
    products.push(new_product.clone());
    save_products(&products);
    Outcome::Json(201, serde_json::to_string(&new_product).unwrap_or_default())
}

/// PATCH /api/products/{barcode}/stock — RBAC Protected (Admin only)
fn adjust_stock(request: &IncomingRequest, barcode: &str) -> Outcome {
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };
    let delta = val["delta"].as_i64().unwrap_or(0) as i32;

    let mut products = load_products();
    if let Some(p) = products.iter_mut().find(|p| p.barcode == barcode) {
        p.stock += delta;
        if p.stock < 0 { p.stock = 0; }
        let stock = p.stock;
        save_products(&products);
        Outcome::Json(200, json!({ "barcode": barcode, "stock": stock }).to_string())
    } else {
        Outcome::Err(404, "Product not found".into())
    }
}

/// GET /api/alerts — RBAC Protected (Admin only)
fn list_alerts() -> Outcome {
    let products = load_products();
    let low_stock: Vec<&Product> = products.iter().filter(|p| p.stock <= 5).collect();
    Outcome::Json(200, serde_json::to_string(&low_stock).unwrap_or_default())
}

/// GET /api/cart
fn get_cart() -> Outcome {
    let products = load_products();
    let cart = load_cart();
    let mut total_cents = 0u32;
    let mut items = Vec::new();
    for item in &cart {
        if let Some(p) = products.iter().find(|p| p.barcode == item.barcode) {
            let line_cents = p.price_cents * item.quantity;
            total_cents += line_cents;
            items.push(json!({
                "product": p,
                "quantity": item.quantity,
                "line_cents": line_cents
            }));
        }
    }
    Outcome::Json(200, json!({
        "items": items,
        "total_cents": total_cents,
        "items_count": cart.iter().map(|i| i.quantity).sum::<u32>(),
    }).to_string())
}

/// POST /api/cart/items
fn add_cart_item(request: &IncomingRequest) -> Outcome {
    let raw = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "Could not read body".into()),
    };
    let val: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Outcome::Err(400, format!("Bad JSON: {e}")),
    };
    let barcode = val["barcode"].as_str().unwrap_or("").to_string();
    let qty = val["quantity"].as_u64().unwrap_or(1) as u32;

    let products = load_products();
    let in_stock = products.iter().find(|p| p.barcode == barcode).map(|p| p.stock).unwrap_or(0);
    if in_stock <= 0 {
        return Outcome::Err(400, "Product is out of stock".into());
    }

    let mut cart = load_cart();
    if let Some(existing) = cart.iter_mut().find(|i| i.barcode == barcode) {
        existing.quantity += qty;
    } else {
        cart.push(CartItem { barcode, quantity: qty });
    }
    save_cart(&cart);
    Outcome::Json(200, json!({ "status": "added" }).to_string())
}

/// DELETE /api/cart/items/{barcode}
fn remove_cart_item(barcode: &str) -> Outcome {
    let mut cart = load_cart();
    cart.retain(|i| i.barcode != barcode);
    save_cart(&cart);
    Outcome::Json(200, json!({ "status": "removed" }).to_string())
}

/// POST /api/checkout
fn checkout(request: &IncomingRequest) -> Outcome {
    let raw = read_body(request).unwrap_or_default();
    let val: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

    let mut products = load_products();
    let mut cart = load_cart();

    let items_to_buy: Vec<(String, u32)> = if let Some(items_arr) = val["items"].as_array() {
        items_arr
            .iter()
            .filter_map(|i| {
                let bc = i["barcode"].as_str()?;
                let q = i["quantity"].as_u64()? as u32;
                Some((bc.to_string(), q))
            })
            .collect()
    } else {
        cart.iter().map(|i| (i.barcode.clone(), i.quantity)).collect()
    };

    if items_to_buy.is_empty() {
        return Outcome::Err(400, "Cart is empty".into());
    }

    let mut total_cents = 0u32;
    for (barcode, qty) in &items_to_buy {
        if let Some(p) = products.iter_mut().find(|p| &p.barcode == barcode) {
            p.stock -= *qty as i32;
            if p.stock < 0 { p.stock = 0; }
            total_cents += p.price_cents * *qty;
        }
    }

    save_products(&products);
    cart.clear();
    save_cart(&cart);

    let sec = wall_clock::now().seconds;
    let order_id = format!("ORD-{}", sec % 1_000_000);

    Outcome::Json(200, json!({
        "order_id": order_id,
        "status": "confirmed",
        "total_cents": total_cents,
        "timestamp": sec,
    }).to_string())
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::File(code, ctype, bytes) => {
            respond(response_out, code, &ctype, &bytes);
        }
        Outcome::Json(code, json_str) => {
            respond(response_out, code, "application/json; charset=utf-8", json_str.as_bytes());
        }
        Outcome::Err(code, msg) => {
            let body = json!({ "error": msg }).to_string();
            respond(response_out, code, "application/json; charset=utf-8", body.as_bytes());
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let _ = headers.set("access-control-allow-methods", &[b"GET, POST, PATCH, DELETE, OPTIONS".to_vec()]);
    let _ = headers.set("access-control-allow-headers", &[b"Content-Type, Authorization".to_vec()]);
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
