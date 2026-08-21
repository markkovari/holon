mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

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

struct State {
    users: Vec<UserProfile>,
    posts: Vec<Post>,
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| {
    Mutex::new(State {
        users: Vec::new(),
        posts: Vec::new(),
    })
});

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, ["api", "posts"]) => get_posts(),
            (Method::Post, ["api", "posts"]) => create_post(&request),
            (Method::Post, ["api", "posts", id, "like"]) => like_post(id),
            (Method::Get, ["api", "users"]) => get_users(),
            (Method::Post, ["api", "users"]) => create_user(&request),
            (Method::Post, ["api", "users", id, "follow"]) => follow_user(id),
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
    let _ = fields.set(&"content-type".to_string(), &vec![b"application/json".to_vec()]);

    let response = OutgoingResponse::new(fields);
    response.set_status_code(status).unwrap();
    let body = response.body().unwrap();
    ResponseOutparam::set(response_out, Ok(response));

    let stream = body.write().unwrap();
    stream.blocking_write_and_flush(&body_bytes).unwrap();
    drop(stream);
    OutgoingBody::finish(body, None).unwrap();
}

fn get_posts() -> Outcome {
    let state = STATE.lock().unwrap();
    let json = serde_json::to_string(&state.posts).unwrap_or_else(|_| "[]".to_string());
    Outcome::Json(200, json)
}

fn get_users() -> Outcome {
    let state = STATE.lock().unwrap();
    let json = serde_json::to_string(&state.users).unwrap_or_else(|_| "[]".to_string());
    Outcome::Json(200, json)
}

fn create_user(request: &IncomingRequest) -> Outcome {
    let body_bytes = read_body(request).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let username = json["username"].as_str().unwrap_or("anonymous").to_string();
    
    let user = UserProfile {
        id: format!("user_{}", bindings::wasi::random::random::get_random_u64()),
        username,
        followers: Vec::new(),
    };
    
    let mut state = STATE.lock().unwrap();
    state.users.push(user.clone());
    Outcome::Json(201, serde_json::to_string(&user).unwrap())
}

fn create_post(request: &IncomingRequest) -> Outcome {
    let body_bytes = read_body(request).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or(serde_json::json!({}));
    let author_id = json["author_id"].as_str().unwrap_or("anon").to_string();
    let image_url = json["image_url"].as_str().unwrap_or("").to_string();
    let caption = json["caption"].as_str().unwrap_or("").to_string();
    
    let post = Post {
        id: format!("post_{}", bindings::wasi::random::random::get_random_u64()),
        author_id,
        image_url,
        caption,
        likes: Vec::new(),
    };
    
    let mut state = STATE.lock().unwrap();
    state.posts.push(post.clone());
    Outcome::Json(201, serde_json::to_string(&post).unwrap())
}

fn like_post(id: &str) -> Outcome {
    let mut state = STATE.lock().unwrap();
    if let Some(post) = state.posts.iter_mut().find(|p| p.id == id) {
        // Just mocking a user ID for the like
        let user_id = "mock_user".to_string();
        if !post.likes.contains(&user_id) {
            post.likes.push(user_id);
        }
        Outcome::Json(200, serde_json::to_string(post).unwrap())
    } else {
        Outcome::Err(404, "Post not found".into())
    }
}

fn follow_user(id: &str) -> Outcome {
    let mut state = STATE.lock().unwrap();
    if let Some(user) = state.users.iter_mut().find(|u| u.id == id) {
        let follower_id = "mock_user".to_string();
        if !user.followers.contains(&follower_id) {
            user.followers.push(follower_id);
        }
        Outcome::Json(200, serde_json::to_string(user).unwrap())
    } else {
        Outcome::Err(404, "User not found".into())
    }
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    Ok(buf)
}

bindings::export!(Component with_types_in bindings);
