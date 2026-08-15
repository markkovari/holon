//! Owners and their pets. **This file is the goal of the `owners-and-pets` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer, and
//! `crate::bindings::records::store::store` is where things are kept.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

const OWNERS: &str = "owners";
const PETS: &str = "pets";

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "owners"]) => create_owner(body),
        (Method::Get, ["api", "owners", id]) => get_owner(id),
        (Method::Get, ["api", "owners"]) => search_owners(route),
        (Method::Post, ["api", "owners", owner_id, "pets"]) => create_pet(owner_id, body),
        (Method::Get, ["api", "owners", owner_id, "pets"]) => list_pets(owner_id),
        (Method::Get, ["api", "pets", id]) => get_pet(id),
        _ => Reply::err(404, "not_found"),
    }
}

fn parse_body(body: &str) -> Result<Value, Reply> {
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(body).map_err(|_| Reply::err(400, "invalid"))
}

fn owner_view(id: &str, doc: &Value) -> Value {
    json!({ "id": id, "name": doc["name"], "email": doc["email"] })
}

fn pet_view(id: &str, doc: &Value) -> Value {
    json!({
        "id": id,
        "owner_id": doc["owner_id"],
        "name": doc["name"],
        "species": doc["species"],
        "born": doc["born"],
    })
}

fn store_err(e: records::StoreError) -> Reply {
    match e {
        records::StoreError::NotFound => Reply::err(404, "not_found"),
        records::StoreError::InvalidJson(_) => Reply::err(400, "invalid"),
        records::StoreError::RevisionConflict(_) => Reply::err(409, "conflict"),
        records::StoreError::BackendUnavailable(_) => Reply::err(503, "unavailable"),
    }
}

fn create_owner(body: &str) -> Reply {
    let doc = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let name = doc["name"].as_str().unwrap_or("").trim().to_string();
    let email = doc["email"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() || !email.contains('@') {
        return Reply::err(400, "invalid");
    }
    let stored = json!({ "name": name, "email": email });
    match records::create(OWNERS, &stored.to_string(), &[]) {
        Ok(entry) => Reply::json(201, owner_view(&entry.id, &stored)),
        Err(e) => store_err(e),
    }
}

fn get_owner(id: &str) -> Reply {
    match records::get(OWNERS, id) {
        Ok(entry) => {
            let doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            Reply::json(200, owner_view(id, &doc))
        }
        Err(e) => store_err(e),
    }
}

fn search_owners(route: &Route) -> Reply {
    let q = route.param("q").to_lowercase();
    let page = match records::list_records(OWNERS, 1000, "") {
        Ok(p) => p,
        Err(e) => return store_err(e),
    };
    let owners: Vec<Value> = page
        .entries
        .iter()
        .filter_map(|e| {
            let doc: Value = serde_json::from_str(&e.data).ok()?;
            if q.is_empty() {
                return Some(owner_view(&e.id, &doc));
            }
            let name = doc["name"].as_str().unwrap_or("").to_lowercase();
            let email = doc["email"].as_str().unwrap_or("").to_lowercase();
            if name.contains(&q) || email.contains(&q) {
                Some(owner_view(&e.id, &doc))
            } else {
                None
            }
        })
        .collect();
    Reply::json(200, json!({ "owners": owners }))
}

fn create_pet(owner_id: &str, body: &str) -> Reply {
    match records::get(OWNERS, owner_id) {
        Ok(_) => {}
        Err(_) => return Reply::err(404, "not_found"),
    }
    let doc = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let name = doc["name"].as_str().unwrap_or("").trim().to_string();
    let species = doc["species"].as_str().unwrap_or("").trim().to_string();
    let born = doc["born"].as_str().unwrap_or("").trim().to_string();
    if !["dog", "cat", "bird", "other"].contains(&species.as_str()) {
        return Reply::err(400, "invalid");
    }
    let stored = json!({ "owner_id": owner_id, "name": name, "species": species, "born": born });
    match records::create(PETS, &stored.to_string(), &["owner_id".to_string()]) {
        Ok(entry) => Reply::json(201, pet_view(&entry.id, &stored)),
        Err(e) => store_err(e),
    }
}

fn get_pet(id: &str) -> Reply {
    match records::get(PETS, id) {
        Ok(entry) => {
            let doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            Reply::json(200, pet_view(id, &doc))
        }
        Err(e) => store_err(e),
    }
}

fn list_pets(owner_id: &str) -> Reply {
    if records::get(OWNERS, owner_id).is_err() {
        return Reply::err(404, "not_found");
    }
    let page = match records::list_records(PETS, 1000, "") {
        Ok(p) => p,
        Err(e) => return store_err(e),
    };
    let pets: Vec<Value> = page
        .entries
        .iter()
        .filter_map(|e| {
            let doc: Value = serde_json::from_str(&e.data).ok()?;
            if doc["owner_id"].as_str() == Some(owner_id) {
                Some(pet_view(&e.id, &doc))
            } else {
                None
            }
        })
        .collect();
    Reply::json(200, json!({ "pets": pets }))
}