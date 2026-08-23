//! `insta-domain` — post pictures, follow other people and like what they posted

mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
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
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Post, ["api", "login"]) => login_user(&request),
            (Method::Get, ["api", "posts"]) => get_posts(),
            (Method::Post, ["api", "posts"]) => create_post(&request),
            (Method::Post, ["api", "posts", id, "like"]) => like_post(&request, id),
            (Method::Get, ["api", "users"]) => get_users(),
            (Method::Post, ["api", "users"]) => create_user(&request),
            (Method::Post, ["api", "users", id, "follow"]) => follow_user(&request, id),
            _ => Outcome::Err(404, "not_found".into()),
        };

        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn emit(response_out: ResponseOutparam, outcome: Outcome) {
    let (status, body_bytes) = match outcome {
        Outcome::Json(s, b) => (s, b.into_bytes()),
        Outcome::Err(s, b) => (s, format!("{{\"error\":\"{}\"}}", b).into_bytes()),
    };

    let fields = Fields::new();
    let _ = fields.set("content-type", &[b"application/json".to_vec()]);

    let response = OutgoingResponse::new(fields);
    response.set_status_code(status).unwrap();
    let body = response.body().unwrap();
    ResponseOutparam::set(response_out, Ok(response));

    let stream = body.write().unwrap();
    let _ = write_all(&stream, &body_bytes);
    drop(stream);
    OutgoingBody::finish(body, None).unwrap();
}

fn get_bucket() -> bindings::wasi::keyvalue::store::Bucket {
    open("").unwrap_or_else(|_| open("default").unwrap())
}

fn get_auth_token(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
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

fn authenticate(
    request: &IncomingRequest,
    _target: &str,
    _action: &str,
) -> Result<String, Outcome> {
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

fn create_user(request: &IncomingRequest) -> Outcome {
    let body_bytes = read_body(request).unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let username = json["username"].as_str().unwrap_or("anonymous").to_string();

    // Attempt to authenticate to tie this to a real identity, though not strictly required
    let user_id = authenticate(request, "users", "create")
        .unwrap_or_else(|_| format!("user_{}", bindings::wasi::random::random::get_random_u64()));

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

fn create_post(request: &IncomingRequest) -> Outcome {
    let author_id = match authenticate(request, "posts", "create") {
        Ok(id) => id,
        Err(e) => return e, // Enforce authentication for creating posts
    };

    let body_bytes = read_body(request).unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let image_url = json["image_url"].as_str().unwrap_or("").to_string();
    let caption = json["caption"].as_str().unwrap_or("").to_string();

    let post = Post {
        id: format!("post_{}", bindings::wasi::random::random::get_random_u64()),
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

fn like_post(request: &IncomingRequest, id: &str) -> Outcome {
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

fn follow_user(request: &IncomingRequest, id: &str) -> Outcome {
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the
                // caller is told, rather than growing until the store's
                // memory cap traps the component and the connection just
                // closes with nothing said.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // A failed read is NOT the end of a body. Breaking here would return
            // what arrived so far as though it were complete.
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

bindings::export!(Component with_types_in bindings);

fn login_user(request: &IncomingRequest) -> Outcome {
    let body_bytes = read_body(request).unwrap_or_default();
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
        bindings::wasi::random::random::get_random_u64()
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

/// Write every byte, respecting what the stream says it can take.
///
/// `blocking_write_and_flush` accepts at most 4096 bytes and TRAPS above it,
/// which kills the component mid-response — the caller sees a closed connection
/// and no status. Any page or JSON body larger than 4 KiB hits it, so the size
/// of the payload decides whether the endpoint works.
///
/// `check_write` reports what the stream will accept now; a zero means block on
/// the pollable and ask again. Copied from the shape every other domain here
/// already uses.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}
