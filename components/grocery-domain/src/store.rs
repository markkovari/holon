use crate::bindings::wasi::keyvalue::store::{self as kv, Bucket};
use crate::types::{CartItem, Product, Session, User};

pub fn get_bucket() -> Option<Bucket> {
    kv::open("default").ok()
}

pub fn initial_products() -> Vec<Product> {
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

pub fn initial_users() -> Vec<User> {
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

pub fn initial_sessions() -> Vec<Session> {
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

pub fn load_products() -> Vec<Product> {
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

pub fn save_products(products: &[Product]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(products) {
            let _ = bucket.set("products", &bytes);
        }
    }
}

pub fn load_cart() -> Vec<CartItem> {
    if let Some(bucket) = get_bucket() {
        if let Ok(Some(bytes)) = bucket.get("cart") {
            if let Ok(c) = serde_json::from_slice::<Vec<CartItem>>(&bytes) {
                return c;
            }
        }
    }
    Vec::new()
}

pub fn save_cart(cart: &[CartItem]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(cart) {
            let _ = bucket.set("cart", &bytes);
        }
    }
}

pub fn load_users() -> Vec<User> {
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

pub fn save_users(users: &[User]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(users) {
            let _ = bucket.set("users", &bytes);
        }
    }
}

pub fn load_sessions() -> Vec<Session> {
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

pub fn save_sessions(sessions: &[Session]) {
    if let Some(bucket) = get_bucket() {
        if let Ok(bytes) = serde_json::to_vec(sessions) {
            let _ = bucket.set("sessions", &bytes);
        }
    }
}
