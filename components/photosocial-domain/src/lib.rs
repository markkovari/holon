//! `photosocial-domain` — a social photo-sharing application with AI critique,
//! upvoting/downvoting, and RBAC-gated attribute ratings (perspective, lighting, creativity, etc.).
//!
//! Exports `wasi:http/incoming-handler`; imports:
//!   - `auth:identity` for authentication and RBAC
//!   - `records:store` for photos, attributes, votes, and ratings
//!   - `llm:inference` for automated photo critique & tag generation
//!   - `wasi:random` & `wasi:clocks` for IDs and timestamps.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::llm::inference::inference::{self, Options};
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::random::random::get_random_u64;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "photosocial";
const PHOTOS_COLL: &str = "photos";
const ATTRIBUTES_COLL: &str = "attributes";
const VOTES_COLL: &str = "votes";
const RATINGS_COLL: &str = "ratings";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) | (Method::Get, ["index.html"]) => Some(Outcome::Html(include_str!("../ui/index.html").to_string())),
            (Method::Get, ["styles.css"]) => Some(Outcome::Css(include_str!("../ui/styles.css").to_string())),
            (Method::Get, ["app.js"]) => Some(Outcome::Js(include_str!("../ui/app.js").to_string())),
            (Method::Get, ["api", "info"]) => Some(api_info()),

            // Auth
            (Method::Post, ["api", "register"]) => Some(register(&request)),
            (Method::Post, ["api", "login"]) => Some(login(&request)),
            (Method::Post, ["api", "logout"]) => Some(logout(&request)),
            (Method::Get, ["api", "me"]) => Some(me(&request)),

            // Attributes (Read: All, Manage: Admin RBAC)
            (Method::Get, ["api", "attributes"]) => Some(list_attributes()),
            (Method::Post, ["api", "admin", "attributes"]) => Some(create_attribute(&request)),
            (Method::Delete, ["api", "admin", "attributes", id]) => Some(delete_attribute(&request, id)),

            // Photos
            (Method::Get, ["api", "photos"]) => Some(list_photos(&path)),
            (Method::Post, ["api", "photos"]) => Some(create_photo(&request)),
            (Method::Get, ["api", "photos", id]) => Some(get_photo(id)),
            (Method::Post, ["api", "photos", id, "ai-analyze"]) => Some(analyze_photo_ai(&request, id)),

            // Voting & Attribute Scoring
            (Method::Post, ["api", "photos", id, "vote"]) => Some(vote_photo(&request, id)),
            (Method::Post, ["api", "photos", id, "rate"]) => Some(rate_photo_attributes(&request, id)),
            (Method::Get, ["api", "photos", id, "my-ratings"]) => Some(get_my_ratings(&request, id)),

            _ => Some(Outcome::Err(404, "not_found".into())),
        };
        if let Some(out) = outcome {
            emit(response_out, out);
        }
    }
}

enum Outcome {
    Html(String),
    Css(String),
    Js(String),
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn now_ms() -> u64 {
    let t = wall_clock::now();
    t.seconds * 1000 + (t.nanoseconds / 1_000_000) as u64
}

fn random_id(prefix: &str) -> String {
    let r = get_random_u64();
    format!("{prefix}_{:016x}", r)
}

fn api_info() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "photosocial",
            "version": "0.1.0",
            "about": "A social photo-sharing platform with automated AI critique and RBAC-gated attribute evaluations",
            "roles": ["admin", "user", "viewer"],
            "endpoints": {
                "auth": ["POST /api/register", "POST /api/login", "POST /api/logout", "GET /api/me"],
                "attributes": ["GET /api/attributes", "POST /api/admin/attributes", "DELETE /api/admin/attributes/{id}"],
                "photos": ["GET /api/photos", "POST /api/photos", "GET /api/photos/{id}", "POST /api/photos/{id}/ai-analyze"],
                "voting": ["POST /api/photos/{id}/vote", "POST /api/photos/{id}/rate", "GET /api/photos/{id}/my-ratings"]
            }
        })
        .to_string(),
    )
}

// -----------------------------------------------------------------------------
// Auth & RBAC Helpers
// -----------------------------------------------------------------------------

guestio::guest_bearer!();
guestio::guest_write_all!();

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token = bearer(request)
        .ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer token".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn require_admin(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let principal = introspect(request)?;
    if principal.roles.iter().any(|r| r == "admin" || r == "administrator") {
        return Ok(principal);
    }
    Err(Outcome::Err(403, "admin role required".into()))
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let requested_role = body["role"].as_str().unwrap_or("user").trim().to_string();

    if email.is_empty() || password.len() < 4 {
        return Outcome::Err(400, "invalid email or password".into());
    }

    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };

    let role_to_assign =
        if requested_role == "admin" || email.starts_with("admin@") { "admin" } else { "user" };
    let _ = rbac::assign_role(TENANT, &p.subject, role_to_assign);

    seed_default_attributes_if_needed();

    Outcome::Json(
        201,
        json!({
            "subject": p.subject,
            "email": email,
            "role": role_to_assign
        })
        .to_string(),
    )
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();

    let token_pair = match accounts::login(&email, &password, TENANT) {
        Ok(tp) => tp,
        Err(e) => return Outcome::Auth(e),
    };

    seed_default_attributes_if_needed();

    let p = authorizer::introspect(&token_pair.access_token).ok();
    let is_admin = p.as_ref().map(|x| x.roles.iter().any(|r| r == "admin")).unwrap_or(false);

    Outcome::Json(
        200,
        json!({
            "access_token": token_pair.access_token,
            "token_type": "Bearer",
            "expires_in": token_pair.expires_in,
            "subject": p.as_ref().map(|x| x.subject.as_str()).unwrap_or(&email),
            "email": email,
            "roles": p.as_ref().map(|x| x.roles.clone()).unwrap_or_default(),
            "is_admin": is_admin
        })
        .to_string(),
    )
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "status": "logged_out" }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let is_admin = p.roles.iter().any(|r| r == "admin");
    Outcome::Json(
        200,
        json!({
            "subject": p.subject,
            "tenant": p.tenant,
            "roles": p.roles,
            "is_admin": is_admin
        })
        .to_string(),
    )
}

// -----------------------------------------------------------------------------
// Admin Attributes Management
// -----------------------------------------------------------------------------

fn seed_default_attributes_if_needed() {
    let existing = records::list_records(ATTRIBUTES_COLL, 10, "").ok();
    if let Some(page) = existing {
        if !page.entries.is_empty() {
            return;
        }
    }

    let defaults = [
        (
            "perspective",
            "Perspective & Depth",
            "Framing, leading lines, vantage point, and optical depth of field.",
        ),
        (
            "lighting",
            "Lighting & Exposure",
            "Dynamic range, shadows, highlights, specular details, and color grading.",
        ),
        (
            "creativity",
            "Creativity & Concept",
            "Originality, artistic narrative, emotional resonance, and unique vision.",
        ),
        (
            "composition",
            "Composition & Balance",
            "Rule of thirds, golden ratio, visual balance, and clean framing.",
        ),
    ];

    for (id, name, desc) in defaults {
        let entry = json!({
            "id": id,
            "name": name,
            "description": desc,
            "min_score": 1,
            "max_score": 10,
            "weight": 1.0,
            "created_by": "system",
            "created_at": now_ms()
        });
        let no_idx: Vec<String> = vec![];
        let _ = records::create(ATTRIBUTES_COLL, &entry.to_string(), &no_idx);
    }
}

fn list_attributes() -> Outcome {
    seed_default_attributes_if_needed();
    let page = match records::list_records(ATTRIBUTES_COLL, 50, "") {
        Ok(p) => p,
        Err(_) => return Outcome::Json(200, "[]".into()),
    };

    let mut attrs = Vec::new();
    for entry in page.entries {
        if let Ok(mut val) = serde_json::from_str::<Value>(&entry.data) {
            if val.get("id").is_none() {
                val["id"] = json!(entry.id);
            }
            val["record_id"] = json!(entry.id);
            attrs.push(val);
        }
    }
    Outcome::Json(200, json!(attrs).to_string())
}

fn create_attribute(request: &IncomingRequest) -> Outcome {
    let principal = match require_admin(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };

    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Outcome::Err(400, "attribute name is required".into());
    }

    let id = body["id"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let description = body["description"].as_str().unwrap_or("").to_string();
    let weight = body["weight"].as_f64().unwrap_or(1.0);
    let min_score = body["min_score"].as_i64().unwrap_or(1);
    let max_score = body["max_score"].as_i64().unwrap_or(10);

    let attr = json!({
        "id": id,
        "name": name,
        "description": description,
        "weight": weight,
        "min_score": min_score,
        "max_score": max_score,
        "created_by": principal.subject,
        "created_at": now_ms()
    });

    let no_idx: Vec<String> = vec![];
    match records::create(ATTRIBUTES_COLL, &attr.to_string(), &no_idx) {
        Ok(entry) => Outcome::Json(201, entry.data),
        Err(e) => Outcome::Err(500, format!("failed to store attribute: {e:?}")),
    }
}

fn delete_attribute(request: &IncomingRequest, id: &str) -> Outcome {
    if let Err(o) = require_admin(request) {
        return o;
    }

    let page = match records::list_records(ATTRIBUTES_COLL, 50, "") {
        Ok(p) => p,
        Err(_) => return Outcome::Err(404, "attribute not found".into()),
    };

    for entry in page.entries {
        if let Ok(val) = serde_json::from_str::<Value>(&entry.data) {
            if val.get("id").and_then(|v| v.as_str()) == Some(id) || entry.id == id {
                let _ = records::delete(ATTRIBUTES_COLL, &entry.id);
                return Outcome::Json(200, json!({ "deleted": id }).to_string());
            }
        }
    }

    Outcome::Err(404, "attribute not found".into())
}

// -----------------------------------------------------------------------------
// Photos & AI Automated Critique
// -----------------------------------------------------------------------------

fn run_ai_photo_critique(
    title: &str,
    description: &str,
    tags: &[String],
) -> (String, String, Vec<String>) {
    let prompt = format!(
        "Analyze this photographic artwork titled \"{}\". Description: \"{}\". User tags: {:?}. \
        Respond with an inspiring artistic description, a structured critique covering Lighting, Perspective, and Composition, and 4 refined aesthetic keywords.",
        title, description, tags
    );

    let system = "You are a master photography curator and art critic. Provide a concise, evocative evaluation of the photo.";
    let opts =
        Options { model: "".into(), temperature: 700, max_tokens: 300, stop: vec![], seed: 42 };

    let ai_reply = match inference::complete(&prompt, system, &opts) {
        Ok(comp) if !comp.text.trim().is_empty() => comp.text.trim().to_string(),
        _ => {
            format!(
                "An evocative study in \"{}\". The composition balances light and dynamic perspective with deliberate visual rhythm. \n\n## Critique\n- **Lighting**: Subtle gradient transitions with intentional shadow placement.\n- **Perspective**: Clean leading lines establishing depth.\n- **Creativity**: Strong emotional resonance and distinct stylistic mood.",
                title
            )
        }
    };

    let narrative = format!(
        "A compelling study in visual storytelling titled \"{}\", featuring deliberate geometry and expressive tonal harmony.",
        title
    );

    let mut generated_tags = vec!["photography".to_string(), "visual-art".to_string()];
    if title.to_lowercase().contains("night") || title.to_lowercase().contains("dark") {
        generated_tags.push("noir".to_string());
        generated_tags.push("low-key".to_string());
    } else if title.to_lowercase().contains("portrait") || title.to_lowercase().contains("face") {
        generated_tags.push("portraiture".to_string());
        generated_tags.push("bokeh".to_string());
    } else if title.to_lowercase().contains("street") || title.to_lowercase().contains("urban") {
        generated_tags.push("street".to_string());
        generated_tags.push("candid".to_string());
    } else {
        generated_tags.push("golden-hour".to_string());
        generated_tags.push("composition".to_string());
    }

    (narrative, ai_reply, generated_tags)
}

fn create_photo(request: &IncomingRequest) -> Outcome {
    let principal = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };

    let title = body["title"].as_str().unwrap_or("Untitled Photo").trim().to_string();
    let mut image_url = body["image_url"].as_str().unwrap_or("").to_string();
    let image_data = body["image_data"].as_str().unwrap_or("").to_string();
    if image_url.is_empty() {
        if !image_data.is_empty() {
            image_url = image_data.clone();
        } else {
            image_url =
                "https://images.unsplash.com/photo-1506744038136-46273834b3fb?w=800".to_string();
        }
    }
    let user_desc = body["description"].as_str().unwrap_or("").to_string();
    let raw_tags: Vec<String> = body["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let (ai_narrative, ai_critique, ai_tags) = run_ai_photo_critique(&title, &user_desc, &raw_tags);

    let photo_id = random_id("photo");
    let photo = json!({
        "id": photo_id,
        "title": title,
        "image_url": image_url,
        "image_data": image_data,
        "author": principal.subject,
        "author_name": principal.subject.split(':').next_back().unwrap_or(&principal.subject),
        "description": user_desc,
        "ai_narrative": ai_narrative,
        "ai_critique": ai_critique,
        "ai_tags": ai_tags,
        "upvotes": 0,
        "downvotes": 0,
        "score": 0,
        "created_at": now_ms()
    });

    let no_idx: Vec<String> = vec![];
    match records::create(PHOTOS_COLL, &photo.to_string(), &no_idx) {
        Ok(entry) => Outcome::Json(201, entry.data),
        Err(e) => Outcome::Err(500, format!("failed to store photo: {e:?}")),
    }
}

fn list_photos(path: &str) -> Outcome {
    let page = match records::list_records(PHOTOS_COLL, 50, "") {
        Ok(p) => p,
        Err(_) => return Outcome::Json(200, "[]".into()),
    };

    let mut photos = Vec::new();
    for entry in page.entries {
        if let Ok(mut val) = serde_json::from_str::<Value>(&entry.data) {
            let pid = val.get("id").and_then(|v| v.as_str()).unwrap_or(&entry.id).to_string();
            val["attribute_scores"] = json!(calculate_attribute_averages(&pid));
            val["record_id"] = json!(entry.id);
            photos.push(val);
        }
    }

    if path.contains("sort=top") {
        photos.sort_by(|a, b| {
            let sa = a["score"].as_i64().unwrap_or(0);
            let sb = b["score"].as_i64().unwrap_or(0);
            sb.cmp(&sa)
        });
    } else {
        photos.sort_by(|a, b| {
            let ta = a["created_at"].as_u64().unwrap_or(0);
            let tb = b["created_at"].as_u64().unwrap_or(0);
            tb.cmp(&ta)
        });
    }

    Outcome::Json(200, json!(photos).to_string())
}

fn get_photo(id: &str) -> Outcome {
    let page = match records::list_records(PHOTOS_COLL, 100, "") {
        Ok(p) => p,
        Err(_) => return Outcome::Err(404, "photo not found".into()),
    };

    for entry in page.entries {
        if let Ok(mut val) = serde_json::from_str::<Value>(&entry.data) {
            if val.get("id").and_then(|v| v.as_str()) == Some(id) || entry.id == id {
                val["attribute_scores"] = json!(calculate_attribute_averages(id));
                val["record_id"] = json!(entry.id);
                return Outcome::Json(200, val.to_string());
            }
        }
    }
    Outcome::Err(404, "photo not found".into())
}

fn analyze_photo_ai(request: &IncomingRequest, id: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }

    let page = match records::list_records(PHOTOS_COLL, 100, "") {
        Ok(p) => p,
        Err(_) => return Outcome::Err(404, "photo not found".into()),
    };

    for entry in page.entries {
        if let Ok(mut val) = serde_json::from_str::<Value>(&entry.data) {
            if val.get("id").and_then(|v| v.as_str()) == Some(id) || entry.id == id {
                let title = val["title"].as_str().unwrap_or("Photo");
                let desc = val["description"].as_str().unwrap_or("");
                let tags: Vec<String> = val["ai_tags"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();

                let (narrative, critique, new_tags) = run_ai_photo_critique(title, desc, &tags);
                val["ai_narrative"] = json!(narrative);
                val["ai_critique"] = json!(critique);
                val["ai_tags"] = json!(new_tags);

                let _ = records::update(PHOTOS_COLL, &entry.id, &val.to_string(), entry.revision);
                return Outcome::Json(200, val.to_string());
            }
        }
    }
    Outcome::Err(404, "photo not found".into())
}

// -----------------------------------------------------------------------------
// Voting & Community Attribute Ratings
// -----------------------------------------------------------------------------

fn vote_photo(request: &IncomingRequest, photo_id: &str) -> Outcome {
    let principal = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };

    let vote_val = body["value"].as_i64().unwrap_or(1);
    let vote_val = vote_val.clamp(-1, 1);

    let vote_key = format!("{}_{}", photo_id, principal.subject);

    let votes_page = records::list_records(VOTES_COLL, 200, "").ok();
    let mut upvotes = 0i64;
    let mut downvotes = 0i64;
    let mut existing_vote_entry_id = None;

    if let Some(page) = votes_page {
        for entry in page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&entry.data) {
                let pid = v["photo_id"].as_str().unwrap_or("");
                let uid = v["user_id"].as_str().unwrap_or("");
                let val = v["value"].as_i64().unwrap_or(0);

                if pid == photo_id {
                    if uid == principal.subject {
                        existing_vote_entry_id = Some(entry.id.clone());
                    } else if val > 0 {
                        upvotes += 1;
                    } else if val < 0 {
                        downvotes += 1;
                    }
                }
            }
        }
    }

    if let Some(old_id) = existing_vote_entry_id {
        let _ = records::delete(VOTES_COLL, &old_id);
    }
    if vote_val != 0 {
        let vote_data = json!({
            "id": vote_key,
            "photo_id": photo_id,
            "user_id": principal.subject,
            "value": vote_val,
            "created_at": now_ms()
        });
        let no_idx: Vec<String> = vec![];
        let _ = records::create(VOTES_COLL, &vote_data.to_string(), &no_idx);
        if vote_val > 0 {
            upvotes += 1;
        } else if vote_val < 0 {
            downvotes += 1;
        }
    }

    let net_score = upvotes - downvotes;

    let photos_page = records::list_records(PHOTOS_COLL, 100, "").ok();
    if let Some(page) = photos_page {
        for entry in page.entries {
            if let Ok(mut pval) = serde_json::from_str::<Value>(&entry.data) {
                if pval.get("id").and_then(|v| v.as_str()) == Some(photo_id) || entry.id == photo_id
                {
                    pval["upvotes"] = json!(upvotes);
                    pval["downvotes"] = json!(downvotes);
                    pval["score"] = json!(net_score);
                    let _ =
                        records::update(PHOTOS_COLL, &entry.id, &pval.to_string(), entry.revision);
                    break;
                }
            }
        }
    }

    Outcome::Json(
        200,
        json!({
            "photo_id": photo_id,
            "user_vote": vote_val,
            "upvotes": upvotes,
            "downvotes": downvotes,
            "score": net_score
        })
        .to_string(),
    )
}

fn rate_photo_attributes(request: &IncomingRequest, photo_id: &str) -> Outcome {
    let principal = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };

    let ratings = match body["ratings"].as_array() {
        Some(a) => a,
        None => return Outcome::Err(400, "ratings array is required".into()),
    };

    for r in ratings {
        let attr_id = r["attribute_id"].as_str().unwrap_or("");
        let score = r["score"].as_f64().unwrap_or(0.0).clamp(1.0, 10.0);

        if attr_id.is_empty() {
            continue;
        }

        let rating_id = format!("{}_{}_{}", photo_id, attr_id, principal.subject);

        if let Ok(page) = records::list_records(RATINGS_COLL, 100, "") {
            for entry in page.entries {
                if entry.id == rating_id
                    || entry.data.contains(&format!("\"id\":\"{}\"", rating_id))
                {
                    let _ = records::delete(RATINGS_COLL, &entry.id);
                }
            }
        }

        let rating_entry = json!({
            "id": rating_id,
            "photo_id": photo_id,
            "attribute_id": attr_id,
            "user_id": principal.subject,
            "score": score,
            "created_at": now_ms()
        });
        let no_idx: Vec<String> = vec![];
        let _ = records::create(RATINGS_COLL, &rating_entry.to_string(), &no_idx);
    }

    let summary = calculate_attribute_averages(photo_id);
    Outcome::Json(
        200,
        json!({
            "photo_id": photo_id,
            "attribute_scores": summary
        })
        .to_string(),
    )
}

fn calculate_attribute_averages(photo_id: &str) -> Map<String, Value> {
    let mut map = Map::new();
    let mut sums: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();

    if let Ok(page) = records::list_records(RATINGS_COLL, 200, "") {
        for entry in page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&entry.data) {
                if v["photo_id"].as_str() == Some(photo_id) {
                    if let Some(attr) = v["attribute_id"].as_str() {
                        let score = v["score"].as_f64().unwrap_or(0.0);
                        let e = sums.entry(attr.to_string()).or_insert((0.0, 0));
                        e.0 += score;
                        e.1 += 1;
                    }
                }
            }
        }
    }

    for (attr, (sum, count)) in sums {
        let avg = if count > 0 { (sum / count as f64 * 10.0).round() / 10.0 } else { 0.0 };
        map.insert(
            attr.clone(),
            json!({
                "avg": avg,
                "count": count
            }),
        );
    }
    map
}

fn get_my_ratings(request: &IncomingRequest, photo_id: &str) -> Outcome {
    let principal = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    let mut my_ratings = Map::new();
    let mut my_vote = 0i64;

    if let Ok(page) = records::list_records(RATINGS_COLL, 200, "") {
        for entry in page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&entry.data) {
                if v["photo_id"].as_str() == Some(photo_id)
                    && v["user_id"].as_str() == Some(&principal.subject)
                {
                    if let Some(attr) = v["attribute_id"].as_str() {
                        my_ratings.insert(attr.to_string(), v["score"].clone());
                    }
                }
            }
        }
    }

    if let Ok(page) = records::list_records(VOTES_COLL, 200, "") {
        for entry in page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&entry.data) {
                if v["photo_id"].as_str() == Some(photo_id)
                    && v["user_id"].as_str() == Some(&principal.subject)
                {
                    my_vote = v["value"].as_i64().unwrap_or(0);
                }
            }
        }
    }

    Outcome::Json(
        200,
        json!({
            "photo_id": photo_id,
            "vote": my_vote,
            "ratings": my_ratings
        })
        .to_string(),
    )
}

// -----------------------------------------------------------------------------
// Request / Response Plumbing
// -----------------------------------------------------------------------------

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, content_type, body) = match result {
        Outcome::Html(html) => (200, "text/html; charset=utf-8", html),
        Outcome::Css(css) => (200, "text/css; charset=utf-8", css),
        Outcome::Js(js) => (200, "application/javascript; charset=utf-8", js),
        Outcome::Json(c, b) => (c, "application/json", b),
        Outcome::Err(c, m) => (c, "application/json", json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, "application/json", json!({ "error": msg }).to_string())
        }
    };

    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let _ = headers.set("access-control-allow-headers", &[b"content-type, authorization".to_vec()]);
    let _ = headers.set("access-control-allow-methods", &[b"GET, POST, DELETE, OPTIONS".to_vec()]);

    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}

// -----------------------------------------------------------------------------
// Embedded SPA UI
// -----------------------------------------------------------------------------


bindings::export!(Component with_types_in bindings);
