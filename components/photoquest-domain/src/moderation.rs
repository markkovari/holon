//! See CONTRACT.md "Game routes" — this module owns the routes listed there as `moderation.rs`.
//! Scaffold: every route answers 501 until it is built.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let _ = (method, route, body);
    Reply::err(501, "not_implemented")
}

// ---- helpers every game module calls (signatures are the contract; scaffold
// bodies are permissive placeholders until moderation is built) ----

use crate::bindings::auth::identity::types::Principal;
use serde_json::{Map, Value};

/// `Ok(())` when `principal` holds `role`, else the `403 forbidden_role` reply.
pub fn require_role(principal: &Principal, role: &str) -> Result<(), Reply> {
    if principal.roles.iter().any(|r| r == role) {
        Ok(())
    } else {
        Err(Reply::err(403, "forbidden_role"))
    }
}

/// `Err(403 suspended)` when the account is suspended.
pub fn require_active(principal: &Principal) -> Result<(), Reply> {
    let _ = principal;
    Ok(())
}

/// True when moderation has hidden this photo.
pub fn is_hidden(photo: &Map<String, Value>) -> bool {
    photo.get("moderation").and_then(|m| m.get("hidden")).and_then(Value::as_bool).unwrap_or(false)
}
