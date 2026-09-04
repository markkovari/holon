use serde_json::{json, Value};
use crate::bindings::wasi::http::types::IncomingRequest;
use crate::read_body;
use crate::store::{load_products, save_products};
use crate::types::{Outcome, Product};

/// GET /api/products
pub fn list_products() -> Outcome {
    let products = load_products();
    Outcome::Json(200, serde_json::to_string(&products).unwrap_or_default())
}

/// POST /api/products — RBAC Protected (Admin only)
pub fn register_product(request: &IncomingRequest) -> Outcome {
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
pub fn adjust_stock(request: &IncomingRequest, barcode: &str) -> Outcome {
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
pub fn list_alerts() -> Outcome {
    let products = load_products();
    let low_stock: Vec<&Product> = products.iter().filter(|p| p.stock <= 5).collect();
    Outcome::Json(200, serde_json::to_string(&low_stock).unwrap_or_default())
}
