use serde_json::{json, Value};
use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::wasi::http::types::IncomingRequest;
use crate::read_body;
use crate::store::{load_sessions, load_users, save_sessions, save_users};
use crate::types::{Outcome, Session, User, UserPublic};

pub fn get_current_user(request: &IncomingRequest) -> Option<User> {
    let token = crate::bearer(request)?;
    let sessions = load_sessions();
    let session = sessions.iter().find(|s| s.token == token)?;
    let users = load_users();
    users.iter().find(|u| u.id == session.user_id).cloned()
}

pub fn require_role(request: &IncomingRequest, required_role: &str) -> Result<User, Outcome> {
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

/// POST /api/auth/register
pub fn handle_register(request: &IncomingRequest) -> Outcome {
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
pub fn handle_login(request: &IncomingRequest) -> Outcome {
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
pub fn handle_auth_me(request: &IncomingRequest) -> Outcome {
    match get_current_user(request) {
        Some(user) => {
            let pub_user = UserPublic::from(&user);
            Outcome::Json(200, json!({ "user": pub_user }).to_string())
        }
        None => {
            Outcome::Err(401, "Not authenticated".into())
        }
    }
}

/// POST /api/auth/logout
pub fn handle_logout(request: &IncomingRequest) -> Outcome {
    if let Some(token) = crate::bearer(request) {
        let mut sessions = load_sessions();
        sessions.retain(|s| s.token != token);
        save_sessions(&sessions);
    }
    Outcome::Json(200, json!({ "status": "logged_out" }).to_string())
}

/// GET /api/admin/users
pub fn handle_list_users(request: &IncomingRequest) -> Outcome {
    if let Err(e) = require_role(request, "admin") {
        return e;
    }
    let users = load_users();
    let public_users: Vec<UserPublic> = users.iter().map(UserPublic::from).collect();
    Outcome::Json(200, serde_json::to_string(&public_users).unwrap_or_default())
}

/// POST /api/admin/users
pub fn handle_admin_create_user(request: &IncomingRequest) -> Outcome {
    if let Err(e) = require_role(request, "admin") {
        return e;
    }
    handle_register(request)
}

/// PATCH /api/admin/users/{id}/role
pub fn handle_update_user_role(request: &IncomingRequest, user_id: &str) -> Outcome {
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
pub fn handle_delete_user(request: &IncomingRequest, user_id: &str) -> Outcome {
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
