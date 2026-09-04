use serde_json::{json, Value};
use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::wasi::http::types::IncomingRequest;
use crate::read_body;
use crate::store::{load_cart, load_products, save_cart, save_products};
use crate::types::{CartItem, Outcome};

/// GET /api/cart
pub fn get_cart() -> Outcome {
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
pub fn add_cart_item(request: &IncomingRequest) -> Outcome {
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
pub fn remove_cart_item(barcode: &str) -> Outcome {
    let mut cart = load_cart();
    cart.retain(|i| i.barcode != barcode);
    save_cart(&cart);
    Outcome::Json(200, json!({ "status": "removed" }).to_string())
}

/// POST /api/checkout
pub fn checkout(request: &IncomingRequest) -> Outcome {
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
