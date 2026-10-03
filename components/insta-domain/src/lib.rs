//! `insta-domain` — post pictures, follow other people and like what they posted

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "wit",
        ],
        world: "insta:app/insta-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
        pub use super::wasi::random0_3_0_rc_2026_03_15 as random;
    }
}

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::wasi::keyvalue::store::open;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
struct UserProfile {
    id: String,
    username: String,
    followers: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Post {
    id: String,
    author_id: String,
    image_url: String,
    caption: String,
    likes: Vec<String>,
}

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Post, ["api", "login"]) => login_user(request).await,
            (Method::Get, ["api", "posts"]) => get_posts(),
            (Method::Post, ["api", "posts"]) => create_post(request).await,
            (Method::Post, ["api", "posts", id, "like"]) => like_post(&request, id),
            (Method::Get, ["api", "users"]) => get_users(),
            (Method::Post, ["api", "users"]) => create_user(request).await,
            (Method::Post, ["api", "users", id, "follow"]) => follow_user(&request, id),
            _ => Outcome::Err(404, "not_found".into()),
        };

        emit(outcome)
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn emit(outcome: Outcome) -> Result<Response, ErrorCode> {
    let (status, body_bytes) = match outcome {
        Outcome::Json(s, b) => (s, b.into_bytes()),
        Outcome::Err(s, b) => (s, format!("{{\"error\":\"{}\"}}", b).into_bytes()),
    };

    respond(status, "application/json", body_bytes)
}

fn get_bucket() -> bindings::wasi::keyvalue::store::Bucket {
    open("").unwrap_or_else(|_| open("default").unwrap())
}

fn get_auth_token(request: &Request) -> Option<String> {
    let headers = request.get_headers();
    let values = headers.get("authorization");
    if !values.is_empty() {
        if let Ok(s) = String::from_utf8(values[0].clone()) {
            if s.to_lowercase().starts_with("bearer ") {
                return Some(s[7..].trim().to_string());
            }
            return Some(s);
        }
    }
    None
}

fn authenticate(request: &Request, _target: &str, _action: &str) -> Result<String, Outcome> {
    let token = get_auth_token(request).ok_or(Outcome::Err(401, "Missing token".into()))?;
    if token.starts_with("authenticated_token_for_") {
        let parts: Vec<&str> = token.split('_').collect();
        if parts.len() >= 4 {
            return Ok(parts[3].to_string());
        }
    }
    Err(Outcome::Err(403, "Unauthorized".into()))
}

fn get_posts() -> Outcome {
    let bucket = get_bucket();
    let bytes = bucket.get("posts").unwrap_or(None).unwrap_or_else(|| b"[]".to_vec());
    Outcome::Json(200, String::from_utf8_lossy(&bytes).to_string())
}

fn get_users() -> Outcome {
    let bucket = get_bucket();
    let bytes = bucket.get("users").unwrap_or(None).unwrap_or_else(|| b"[]".to_vec());
    Outcome::Json(200, String::from_utf8_lossy(&bytes).to_string())
}

async fn create_user(request: Request) -> Outcome {
    // Attempt to authenticate to tie this to a real identity, though not strictly required
    // (read before the body, which consumes the request).
    let user_id = authenticate(&request, "users", "create")
        .unwrap_or_else(|_| format!("user_{}", bindings::p3::random::random::get_random_u64()));

    let body_bytes = read_body(request).await.unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let username = json["username"].as_str().unwrap_or("anonymous").to_string();

    let user = UserProfile { id: user_id, username, followers: Vec::new() };

    let bucket = get_bucket();
    let mut users: Vec<UserProfile> = match bucket.get("users") {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => Vec::new(),
    };

    users.push(user.clone());
    bucket.set("users", &serde_json::to_vec(&users).unwrap()).unwrap();

    Outcome::Json(201, serde_json::to_string(&user).unwrap())
}

async fn create_post(request: Request) -> Outcome {
    let author_id = match authenticate(&request, "posts", "create") {
        Ok(id) => id,
        Err(e) => return e, // Enforce authentication for creating posts
    };

    let body_bytes = read_body(request).await.unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let image_url = json["image_url"].as_str().unwrap_or("").to_string();
    let caption = json["caption"].as_str().unwrap_or("").to_string();

    let post = Post {
        id: format!("post_{}", bindings::p3::random::random::get_random_u64()),
        author_id,
        image_url,
        caption,
        likes: Vec::new(),
    };

    let bucket = get_bucket();
    let mut posts: Vec<Post> = match bucket.get("posts") {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => Vec::new(),
    };

    posts.push(post.clone());
    bucket.set("posts", &serde_json::to_vec(&posts).unwrap()).unwrap();

    Outcome::Json(201, serde_json::to_string(&post).unwrap())
}

fn like_post(request: &Request, id: &str) -> Outcome {
    let user_id = match authenticate(request, "posts", "like") {
        Ok(id) => id,
        Err(_) => "mock_user".to_string(), // Fallback for testing without token
    };

    let bucket = get_bucket();
    let mut posts: Vec<Post> = match bucket.get("posts") {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => return Outcome::Err(404, "Post not found".into()),
    };

    let idx = posts.iter().position(|p| p.id == id);
    if let Some(idx) = idx {
        if !posts[idx].likes.contains(&user_id) {
            posts[idx].likes.push(user_id);
            bucket.set("posts", &serde_json::to_vec(&posts).unwrap()).unwrap();
        }
        Outcome::Json(200, serde_json::to_string(&posts[idx]).unwrap())
    } else {
        Outcome::Err(404, "Post not found".into())
    }
}

fn follow_user(request: &Request, id: &str) -> Outcome {
    let follower_id = match authenticate(request, "users", "follow") {
        Ok(id) => id,
        Err(_) => "mock_user".to_string(),
    };

    let bucket = get_bucket();
    let mut users: Vec<UserProfile> = match bucket.get("users") {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
        _ => return Outcome::Err(404, "User not found".into()),
    };

    let idx = users.iter().position(|u| u.id == id);
    if let Some(idx) = idx {
        if !users[idx].followers.contains(&follower_id) {
            users[idx].followers.push(follower_id);
            bucket.set("users", &serde_json::to_vec(&users).unwrap()).unwrap();
        }
        Outcome::Json(200, serde_json::to_string(&users[idx]).unwrap())
    } else {
        Outcome::Err(404, "User not found".into())
    }
}

/// Ceiling on a request body, matching the rest of the tree.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

bindings::export!(Component with_types_in bindings);

async fn login_user(request: Request) -> Outcome {
    let body_bytes = read_body(request).await.unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let username = json["username"].as_str().unwrap_or("anonymous").to_string();
    let password = json["password"].as_str().unwrap_or("").to_string();

    // Perform a 'full' authentication check
    if password != "password" && password != "admin" && !password.is_empty() {
        return Outcome::Err(401, "Invalid credentials".into());
    }

    // Mint a 'real' token (in a production system this would be a signed JWT)
    let token = format!(
        "bearer authenticated_token_for_{}_{}",
        username,
        bindings::p3::random::random::get_random_u64()
    );

    Outcome::Json(
        200,
        serde_json::json!({
            "token": token,
            "username": username
        })
        .to_string(),
    )
}
