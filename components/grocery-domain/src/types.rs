use serde::{Deserialize, Serialize};

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

pub enum Outcome {
    Json(u16, String),
    File(u16, String, Vec<u8>),
    Err(u16, String),
}
