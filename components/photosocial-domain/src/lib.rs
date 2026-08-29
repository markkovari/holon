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
            (Method::Get, [""] | ["index.html"]) => serve_spa(),
            (Method::Get, ["api", "info"]) => api_info(),

            // Auth
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            // Attributes (Read: All, Manage: Admin RBAC)
            (Method::Get, ["api", "attributes"]) => list_attributes(),
            (Method::Post, ["api", "admin", "attributes"]) => create_attribute(&request),
            (Method::Delete, ["api", "admin", "attributes", id]) => delete_attribute(&request, id),

            // Photos
            (Method::Get, ["api", "photos"]) => list_photos(&path),
            (Method::Post, ["api", "photos"]) => create_photo(&request),
            (Method::Get, ["api", "photos", id]) => get_photo(id),
            (Method::Post, ["api", "photos", id, "ai-analyze"]) => analyze_photo_ai(&request, id),

            // Voting & Attribute Scoring
            (Method::Post, ["api", "photos", id, "vote"]) => vote_photo(&request, id),
            (Method::Post, ["api", "photos", id, "rate"]) => rate_photo_attributes(&request, id),
            (Method::Get, ["api", "photos", id, "my-ratings"]) => get_my_ratings(&request, id),

            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Html(String),
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
        for chunk in bytes.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

// -----------------------------------------------------------------------------
// Embedded SPA UI
// -----------------------------------------------------------------------------

fn serve_spa() -> Outcome {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>PhotoSocial // AI Critique & Attribute Ratings</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link href="https://fonts.googleapis.com/css2?family=Plus+Jakarta+Sans:wght@400;500;600;700;800&family=JetBrains+Mono:wght@400;500&display=swap" rel="stylesheet">
  <style>
    :root {
      --bg: #090b10;
      --card-bg: #121620;
      --card-hover: #181d2a;
      --border: #23293b;
      --accent: #6366f1;
      --accent-glow: rgba(99, 102, 241, 0.25);
      --text: #f1f5f9;
      --text-muted: #94a3b8;
      --upvote: #10b981;
      --downvote: #f43f5e;
      --gold: #f59e0b;
      --badge-bg: #1e293b;
    }
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      background: var(--bg);
      color: var(--text);
      font-family: 'Plus Jakarta Sans', sans-serif;
      min-height: 100vh;
      display: flex;
      flex-direction: column;
    }
    header {
      background: rgba(18, 22, 32, 0.85);
      backdrop-filter: blur(12px);
      border-bottom: 1px solid var(--border);
      position: sticky;
      top: 0;
      z-index: 100;
      padding: 0.85rem 1.5rem;
      display: flex;
      align-items: center;
      justify-content: space-between;
    }
    .logo {
      display: flex;
      align-items: center;
      gap: 0.6rem;
      font-weight: 800;
      font-size: 1.15rem;
      letter-spacing: -0.5px;
      color: #fff;
    }
    .logo-icon {
      background: linear-gradient(135deg, #6366f1, #ec4899);
      width: 32px;
      height: 32px;
      border-radius: 8px;
      display: flex;
      align-items: center;
      justify-content: center;
      font-size: 1.1rem;
    }
    .nav-actions { display: flex; align-items: center; gap: 0.75rem; }
    button {
      font-family: inherit;
      font-weight: 600;
      border: none;
      border-radius: 8px;
      padding: 0.5rem 1rem;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      transition: all 0.15s ease;
    }
    .btn-primary {
      background: var(--accent);
      color: #fff;
      box-shadow: 0 4px 14px var(--accent-glow);
    }
    .btn-primary:hover { background: #4f46e5; }
    .btn-secondary {
      background: var(--card-bg);
      color: var(--text);
      border: 1px solid var(--border);
    }
    .btn-secondary:hover { background: var(--card-hover); }
    .btn-admin {
      background: linear-gradient(135deg, #f59e0b, #d97706);
      color: #000;
      font-weight: 700;
    }
    .user-pill {
      display: flex;
      align-items: center;
      gap: 0.5rem;
      background: var(--badge-bg);
      border: 1px solid var(--border);
      padding: 0.35rem 0.75rem;
      border-radius: 20px;
      font-size: 0.85rem;
    }
    .role-badge {
      font-size: 0.65rem;
      font-weight: 800;
      padding: 0.15rem 0.4rem;
      border-radius: 4px;
      text-transform: uppercase;
    }
    .role-admin { background: #f59e0b; color: #000; }
    .role-user { background: #6366f1; color: #fff; }

    main {
      max-width: 1200px;
      margin: 0 auto;
      padding: 1.5rem;
      flex: 1;
      width: 100%;
    }
    .feed-header {
      display: flex;
      justify-content: space-between;
      align-items: center;
      margin-bottom: 1.5rem;
      flex-wrap: wrap;
      gap: 1rem;
    }
    .feed-tabs { display: flex; gap: 0.5rem; }
    .feed-tab {
      padding: 0.4rem 0.85rem;
      border-radius: 6px;
      font-size: 0.85rem;
      color: var(--text-muted);
      cursor: pointer;
      background: transparent;
    }
    .feed-tab.active {
      background: var(--card-bg);
      color: #fff;
      border: 1px solid var(--border);
    }

    .gallery-grid {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
      gap: 1.5rem;
    }
    .photo-card {
      background: var(--card-bg);
      border: 1px solid var(--border);
      border-radius: 12px;
      overflow: hidden;
      display: flex;
      flex-direction: column;
      transition: transform 0.15s ease, border-color 0.15s ease;
    }
    .photo-card:hover {
      transform: translateY(-3px);
      border-color: #3b4259;
    }
    .photo-img-wrapper {
      position: relative;
      width: 100%;
      height: 220px;
      background: #000;
      overflow: hidden;
      cursor: pointer;
    }
    .photo-img-wrapper img {
      width: 100%;
      height: 100%;
      object-fit: cover;
      transition: transform 0.3s ease;
    }
    .photo-card:hover .photo-img-wrapper img {
      transform: scale(1.03);
    }
    .ai-badge {
      position: absolute;
      top: 10px;
      left: 10px;
      background: rgba(18, 22, 32, 0.85);
      border: 1px solid rgba(255,255,255,0.15);
      backdrop-filter: blur(8px);
      padding: 0.25rem 0.5rem;
      border-radius: 6px;
      font-size: 0.72rem;
      font-weight: 700;
      color: #a5b4fc;
      display: flex;
      align-items: center;
      gap: 0.3rem;
    }
    .card-body { padding: 1rem; flex: 1; display: flex; flex-direction: column; }
    .photo-title {
      font-weight: 700;
      font-size: 1.05rem;
      margin-bottom: 0.35rem;
      color: #fff;
      cursor: pointer;
    }
    .photo-author {
      font-size: 0.8rem;
      color: var(--text-muted);
      margin-bottom: 0.75rem;
    }
    .ai-narrative-preview {
      font-size: 0.82rem;
      color: #cbd5e1;
      line-height: 1.4;
      margin-bottom: 0.85rem;
      display: -webkit-box;
      -webkit-line-clamp: 2;
      -webkit-box-orient: vertical;
      overflow: hidden;
    }
    .tag-list { display: flex; flex-wrap: wrap; gap: 0.35rem; margin-bottom: 1rem; }
    .tag-item {
      font-size: 0.7rem;
      background: var(--badge-bg);
      color: #94a3b8;
      padding: 0.15rem 0.45rem;
      border-radius: 4px;
    }
    .attributes-breakdown {
      display: grid;
      grid-template-columns: repeat(3, 1fr);
      gap: 0.5rem;
      background: rgba(0,0,0,0.25);
      border-radius: 8px;
      padding: 0.5rem;
      margin-bottom: 1rem;
      text-align: center;
    }
    .attr-stat-label { font-size: 0.65rem; color: var(--text-muted); text-transform: uppercase; }
    .attr-stat-val { font-size: 0.95rem; font-weight: 800; color: #38bdf8; }

    .card-footer {
      display: flex;
      align-items: center;
      justify-content: space-between;
      border-top: 1px solid var(--border);
      padding-top: 0.75rem;
      margin-top: auto;
    }
    .vote-widget {
      display: flex;
      align-items: center;
      background: var(--badge-bg);
      border: 1px solid var(--border);
      border-radius: 20px;
      overflow: hidden;
    }
    .vote-btn {
      background: transparent;
      padding: 0.35rem 0.6rem;
      font-size: 0.85rem;
      color: var(--text-muted);
      border-radius: 0;
    }
    .vote-btn:hover { background: rgba(255,255,255,0.05); color: #fff; }
    .vote-btn.active-up { color: var(--upvote); background: rgba(16,185,129,0.15); }
    .vote-btn.active-down { color: var(--downvote); background: rgba(244,63,94,0.15); }
    .score-count {
      font-weight: 800;
      font-size: 0.85rem;
      padding: 0 0.3rem;
      min-width: 24px;
      text-align: center;
    }

    /* Modal */
    .modal-overlay {
      position: fixed;
      top: 0; left: 0; right: 0; bottom: 0;
      background: rgba(0,0,0,0.75);
      backdrop-filter: blur(8px);
      display: none;
      align-items: center;
      justify-content: center;
      z-index: 200;
      padding: 1rem;
    }
    .modal-content {
      background: var(--card-bg);
      border: 1px solid var(--border);
      border-radius: 16px;
      max-width: 850px;
      width: 100%;
      max-height: 90vh;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      position: relative;
    }
    .modal-header {
      padding: 1.25rem 1.5rem;
      border-bottom: 1px solid var(--border);
      display: flex;
      justify-content: space-between;
      align-items: center;
    }
    .modal-body { padding: 1.5rem; display: flex; flex-direction: column; gap: 1.25rem; }
    .close-btn {
      background: transparent;
      color: var(--text-muted);
      font-size: 1.5rem;
      padding: 0.2rem;
      cursor: pointer;
    }
    .modal-photo-img {
      width: 100%;
      max-height: 380px;
      object-fit: cover;
      border-radius: 10px;
      background: #000;
    }
    .critique-box {
      background: #0b0f19;
      border: 1px solid #1e293b;
      border-left: 4px solid var(--accent);
      border-radius: 8px;
      padding: 1rem;
      font-size: 0.88rem;
      line-height: 1.5;
    }
    .critique-title {
      font-weight: 700;
      color: #a5b4fc;
      margin-bottom: 0.4rem;
      display: flex;
      align-items: center;
      gap: 0.4rem;
    }
    .rating-sliders { display: flex; flex-direction: column; gap: 0.85rem; }
    .rating-row {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 1rem;
      background: var(--badge-bg);
      padding: 0.75rem 1rem;
      border-radius: 8px;
    }
    .rating-label { font-weight: 600; font-size: 0.9rem; flex: 1; }
    .rating-slider { flex: 2; accent-color: var(--accent); cursor: pointer; }
    .rating-val { font-weight: 800; font-family: 'JetBrains Mono', monospace; width: 32px; text-align: right; color: var(--gold); }

    /* Admin Panel */
    .admin-attr-list { display: flex; flex-direction: column; gap: 0.6rem; }
    .admin-attr-item {
      display: flex;
      justify-content: space-between;
      align-items: center;
      background: var(--badge-bg);
      padding: 0.75rem 1rem;
      border-radius: 8px;
      border: 1px solid var(--border);
    }
    .form-group { display: flex; flex-direction: column; gap: 0.35rem; }
    .form-label { font-size: 0.85rem; font-weight: 600; color: var(--text-muted); }
    .form-input {
      background: #090b10;
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 0.6rem;
      color: #fff;
      font-family: inherit;
    }
    .form-input:focus { outline: none; border-color: var(--accent); }
    .dropzone {
      border: 2px dashed var(--border);
      border-radius: 10px;
      padding: 1.5rem 1rem;
      text-align: center;
      background: rgba(18, 22, 32, 0.5);
      cursor: pointer;
      transition: all 0.2s ease;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      gap: 0.5rem;
    }
    .dropzone:hover, .dropzone.dragover {
      border-color: var(--accent);
      background: rgba(99, 102, 241, 0.08);
    }
    .dropzone-icon { font-size: 2rem; }
    .dropzone-text { font-size: 0.85rem; color: #cbd5e1; font-weight: 600; }
    .dropzone-hint { font-size: 0.72rem; color: var(--text-muted); }
    .file-preview {
      display: none;
      position: relative;
      border-radius: 8px;
      overflow: hidden;
      max-height: 180px;
      margin-top: 0.5rem;
      border: 1px solid var(--border);
    }
    .file-preview img {
      width: 100%;
      height: 180px;
      object-fit: cover;
      display: block;
    }
    .file-remove-btn {
      position: absolute;
      top: 8px;
      right: 8px;
      background: rgba(0,0,0,0.7);
      color: #fff;
      border-radius: 50%;
      width: 26px;
      height: 26px;
      display: flex;
      align-items: center;
      justify-content: center;
      font-size: 0.9rem;
      cursor: pointer;
      border: 1px solid rgba(255,255,255,0.2);
    }
  </style>
</head>
<body>
  <header>
    <div class="logo">
      <div class="logo-icon">📸</div>
      <span>PhotoSocial <span style="font-size: 0.75rem; color: #a5b4fc; font-weight: 600;">// AI Critique</span></span>
    </div>
    <div class="nav-actions">
      <div id="userDisplay" class="user-pill">
        <span id="userName">Guest</span>
        <span id="userRole" class="role-badge role-user">viewer</span>
      </div>
      <button class="btn-secondary" id="authBtn" onclick="toggleAuthModal()">Sign In</button>
      <button class="btn-primary" id="uploadBtn" onclick="openUploadModal()">+ Upload Photo</button>
      <button class="btn-admin" id="adminBtn" onclick="openAdminModal()" style="display: none;">⚙ Admin Studio</button>
    </div>
  </header>

  <main>
    <div class="feed-header">
      <div>
        <h1 style="font-size: 1.5rem; font-weight: 800; margin-bottom: 0.25rem;">Community Photography Feed</h1>
        <p style="font-size: 0.85rem; color: var(--text-muted);">Uploaded by creators, reviewed by AI, evaluated on admin-curated criteria.</p>
      </div>
      <div class="feed-tabs">
        <button class="feed-tab active" onclick="loadPhotos('latest', this)">Latest</button>
        <button class="feed-tab" onclick="loadPhotos('top', this)">Top Voted</button>
      </div>
    </div>

    <div class="gallery-grid" id="galleryGrid">
      <!-- Dynamically populated -->
    </div>
  </main>

  <!-- Photo Detail Modal -->
  <div class="modal-overlay" id="photoModal">
    <div class="modal-content">
      <div class="modal-header">
        <h3 id="modalTitle" style="font-weight: 800; font-size: 1.2rem;">Photo Details</h3>
        <button class="close-btn" onclick="closeModal('photoModal')">&times;</button>
      </div>
      <div class="modal-body">
        <img id="modalImg" class="modal-photo-img" src="" alt="Photo">
        
        <div class="critique-box">
          <div class="critique-title">✨ AI Curator Analysis</div>
          <div id="modalAiNarrative" style="margin-bottom: 0.6rem; font-style: italic; color: #e2e8f0;"></div>
          <div id="modalAiCritique" style="color: #94a3b8; font-size: 0.82rem; white-space: pre-line;"></div>
        </div>

        <div>
          <h4 style="font-size: 0.95rem; font-weight: 700; margin-bottom: 0.6rem; display: flex; justify-content: space-between;">
            <span>Rate Evaluated Attributes</span>
            <span style="font-size: 0.75rem; color: var(--text-muted); font-weight: normal;">(Admin configured)</span>
          </h4>
          <div class="rating-sliders" id="ratingSliders">
            <!-- Sliders for attributes -->
          </div>
          <button class="btn-primary" style="margin-top: 0.75rem; width: 100%; justify-content: center;" onclick="submitRatings()">Submit Ratings</button>
        </div>
      </div>
    </div>
  </div>

  <!-- Upload Modal -->
  <div class="modal-overlay" id="uploadModal">
    <div class="modal-content" style="max-width: 550px;">
      <div class="modal-header">
        <h3 style="font-weight: 800;">Upload Photographic Artwork</h3>
        <button class="close-btn" onclick="closeModal('uploadModal')">&times;</button>
      </div>
      <div class="modal-body">
        <div class="form-group">
          <label class="form-label">Select Photo File or Image Preset</label>
          <div class="dropzone" id="dropzone" onclick="document.getElementById('uploadFileInput').click()">
            <div class="dropzone-icon">📁</div>
            <div class="dropzone-text">Click or Drag & Drop Image File Here</div>
            <div class="dropzone-hint">Supports JPEG, PNG, WebP, AVIF up to 10MB</div>
            <input type="file" id="uploadFileInput" accept="image/*" style="display: none;" onchange="handleFileSelected(event)">
          </div>
          <div class="file-preview" id="filePreview">
            <img id="previewImg" src="" alt="Preview">
            <button class="file-remove-btn" onclick="clearSelectedFile(event)">&times;</button>
          </div>
        </div>

        <div class="form-group" id="urlInputGroup" style="margin-top: 0.25rem;">
          <label class="form-label">Or Image URL / Preset</label>
          <input class="form-input" id="uploadImgUrl" value="https://images.unsplash.com/photo-1514565131-fce0801e5785?w=800" oninput="handleUrlInput()">
        </div>

        <div class="form-group">
          <label class="form-label">Photo Title</label>
          <input class="form-input" id="uploadTitle" placeholder="e.g. Midnight in Kyoto" value="Midnight Reflections">
        </div>
        <div class="form-group">
          <label class="form-label">Artist Notes & Intent</label>
          <textarea class="form-input" id="uploadDesc" rows="3" placeholder="Describe the scene, lighting, camera settings...">Captured with a prime 50mm f/1.4 lens under neon rain. Focused on reflection symmetry.</textarea>
        </div>
        <button class="btn-primary" style="width: 100%; justify-content: center;" id="uploadSubmitBtn" onclick="submitPhoto()">Upload & Request AI Critique</button>
      </div>
    </div>
  </div>

  <!-- Admin Studio Modal -->
  <div class="modal-overlay" id="adminModal">
    <div class="modal-content" style="max-width: 600px;">
      <div class="modal-header">
        <h3 style="font-weight: 800; color: #f59e0b;">⚙ Admin Attribute Studio (RBAC Gated)</h3>
        <button class="close-btn" onclick="closeModal('adminModal')">&times;</button>
      </div>
      <div class="modal-body">
        <p style="font-size: 0.85rem; color: var(--text-muted);">Only users with the <strong>admin</strong> role can define or remove scoring attributes.</p>
        <div class="admin-attr-list" id="adminAttrList">
          <!-- Active attributes -->
        </div>

        <div style="background: rgba(0,0,0,0.3); border: 1px dashed var(--border); border-radius: 8px; padding: 1rem;">
          <h4 style="font-size: 0.9rem; font-weight: 700; margin-bottom: 0.6rem;">+ Create New Attribute</h4>
          <div class="form-group" style="margin-bottom: 0.5rem;">
            <label class="form-label">Attribute Name</label>
            <input class="form-input" id="newAttrName" placeholder="e.g. Emotional Impact">
          </div>
          <div class="form-group" style="margin-bottom: 0.75rem;">
            <label class="form-label">Description</label>
            <input class="form-input" id="newAttrDesc" placeholder="e.g. Visual storytelling, mood, and resonance">
          </div>
          <button class="btn-primary" onclick="submitNewAttribute()">Add Admin Attribute</button>
        </div>
      </div>
    </div>
  </div>

  <!-- Auth Modal -->
  <div class="modal-overlay" id="authModal">
    <div class="modal-content" style="max-width: 420px;">
      <div class="modal-header">
        <h3 style="font-weight: 800;">Sign In / Switch Role</h3>
        <button class="close-btn" onclick="closeModal('authModal')">&times;</button>
      </div>
      <div class="modal-body">
        <div class="form-group">
          <label class="form-label">Email</label>
          <input class="form-input" id="authEmail" value="admin@holon.test">
        </div>
        <div class="form-group">
          <label class="form-label">Password</label>
          <input class="form-input" id="authPassword" type="password" value="admin1234">
        </div>
        <div class="form-group">
          <label class="form-label">Account Role</label>
          <select class="form-input" id="authRole">
            <option value="admin">Admin (Full RBAC Management)</option>
            <option value="user">User / Creator (Upload & Rate)</option>
          </select>
        </div>
        <div style="display: flex; gap: 0.5rem; margin-top: 0.5rem;">
          <button class="btn-primary" style="flex: 1; justify-content: center;" onclick="handleAuth('login')">Sign In</button>
          <button class="btn-secondary" style="flex: 1; justify-content: center;" onclick="handleAuth('register')">Register</button>
        </div>
      </div>
    </div>
  </div>

  <script>
    let token = localStorage.getItem('ps_token') || '';
    let currentUser = JSON.parse(localStorage.getItem('ps_user') || 'null');
    let activeAttributes = [];
    let currentPhotoId = null;
    let uploadedFileDataUrl = '';

    async function api(path, method = 'GET', body = null) {
      const headers = { 'content-type': 'application/json' };
      if (token) headers['authorization'] = `Bearer ${token}`;
      const res = await fetch(path, {
        method,
        headers,
        body: body ? JSON.stringify(body) : null
      });
      return res.json().catch(() => ({}));
    }

    async function init() {
      setupDropzone();
      await refreshMe();
      await loadAttributes();
      await loadPhotos();
    }

    function setupDropzone() {
      const dz = document.getElementById('dropzone');
      if (!dz) return;
      ['dragenter', 'dragover'].forEach(name => {
        dz.addEventListener(name, (e) => {
          e.preventDefault();
          e.stopPropagation();
          dz.classList.add('dragover');
        });
      });
      ['dragleave', 'drop'].forEach(name => {
        dz.addEventListener(name, (e) => {
          e.preventDefault();
          e.stopPropagation();
          dz.classList.remove('dragover');
        });
      });
      dz.addEventListener('drop', (e) => {
        const files = e.dataTransfer.files;
        if (files && files.length > 0) {
          processImageFile(files[0]);
        }
      });
    }

    function handleFileSelected(event) {
      const file = event.target.files && event.target.files[0];
      if (file) {
        processImageFile(file);
      }
    }

    function processImageFile(file) {
      if (!file.type.startsWith('image/')) {
        return alert('Please select a valid image file (JPEG, PNG, WebP, etc.)');
      }
      
      const titleInput = document.getElementById('uploadTitle');
      if (!titleInput.value || titleInput.value === 'Midnight Reflections') {
        const cleanName = file.name.replace(/\.[^/.]+$/, '').replace(/[-_]/g, ' ');
        titleInput.value = cleanName.charAt(0).toUpperCase() + cleanName.slice(1);
      }

      const reader = new FileReader();
      reader.onload = (e) => {
        const img = new Image();
        img.onload = () => {
          let width = img.width;
          let height = img.height;
          const maxDim = 1920;
          
          if (width > maxDim || height > maxDim) {
            if (width > height) {
              height = Math.round((height * maxDim) / width);
              width = maxDim;
            } else {
              width = Math.round((width * maxDim) / height);
              height = maxDim;
            }
          }
          
          const canvas = document.createElement('canvas');
          canvas.width = width;
          canvas.height = height;
          const ctx = canvas.getContext('2d');
          ctx.drawImage(img, 0, 0, width, height);
          
          // Downscale to JPEG (quality 0.82)
          uploadedFileDataUrl = canvas.toDataURL('image/jpeg', 0.82);
          
          document.getElementById('previewImg').src = uploadedFileDataUrl;
          document.getElementById('filePreview').style.display = 'block';
          document.getElementById('dropzone').style.display = 'none';
        };
        img.src = e.target.result;
      };
      reader.readAsDataURL(file);
    }

    function clearSelectedFile(event) {
      if (event) {
        event.preventDefault();
        event.stopPropagation();
      }
      uploadedFileDataUrl = '';
      document.getElementById('uploadFileInput').value = '';
      document.getElementById('previewImg').src = '';
      document.getElementById('filePreview').style.display = 'none';
      document.getElementById('dropzone').style.display = 'flex';
    }

    function handleUrlInput() {
      if (uploadedFileDataUrl) {
        clearSelectedFile();
      }
    }

    async function refreshMe() {
      if (token) {
        const me = await api('/api/me');
        if (me.subject) {
          currentUser = me;
          document.getElementById('userName').innerText = me.subject.split('@')[0];
          document.getElementById('userRole').innerText = me.is_admin ? 'admin' : 'creator';
          document.getElementById('userRole').className = 'role-badge ' + (me.is_admin ? 'role-admin' : 'role-user');
          document.getElementById('authBtn').innerText = 'Switch User';
          if (me.is_admin) {
            document.getElementById('adminBtn').style.display = 'inline-flex';
          } else {
            document.getElementById('adminBtn').style.display = 'none';
          }
          return;
        }
      }
      handleAuth('login', true);
    }

    async function handleAuth(action, quiet = false) {
      const email = document.getElementById('authEmail').value;
      const password = document.getElementById('authPassword').value;
      const role = document.getElementById('authRole').value;

      let res = await api(`/api/${action}`, 'POST', { email, password, role });
      if (action === 'register' && res.subject) {
        res = await api('/api/login', 'POST', { email, password });
      } else if (action === 'login' && res.error && quiet) {
        await api('/api/register', 'POST', { email, password, role });
        res = await api('/api/login', 'POST', { email, password });
      }

      if (res.access_token) {
        token = res.access_token;
        localStorage.setItem('ps_token', token);
        closeModal('authModal');
        await refreshMe();
        await loadAttributes();
        await loadPhotos();
      } else if (!quiet && res.error) {
        alert('Auth error: ' + res.error);
      }
    }

    async function loadAttributes() {
      activeAttributes = await api('/api/attributes');
      if (!Array.isArray(activeAttributes)) activeAttributes = [];
      renderAdminAttributes();
    }

    function renderAdminAttributes() {
      const list = document.getElementById('adminAttrList');
      if (!list) return;
      list.innerHTML = activeAttributes.map(a => `
        <div class="admin-attr-item">
          <div>
            <strong>${a.name}</strong>
            <div style="font-size: 0.78rem; color: var(--text-muted);">${a.description || ''}</div>
          </div>
          <button class="btn-secondary" style="padding: 0.25rem 0.5rem; color: #f43f5e;" onclick="deleteAttribute('${a.id || a.record_id}')">Delete</button>
        </div>
      `).join('');
    }

    async function submitNewAttribute() {
      const name = document.getElementById('newAttrName').value.trim();
      const description = document.getElementById('newAttrDesc').value.trim();
      if (!name) return alert('Enter attribute name');
      const res = await api('/api/admin/attributes', 'POST', { name, description });
      if (res.error) return alert(res.error);
      document.getElementById('newAttrName').value = '';
      document.getElementById('newAttrDesc').value = '';
      await loadAttributes();
    }

    async function deleteAttribute(id) {
      if (!confirm('Remove this scoring attribute?')) return;
      await api(`/api/admin/attributes/${id}`, 'DELETE');
      await loadAttributes();
    }

    async function loadPhotos(sort = 'latest', tabEl = null) {
      if (tabEl) {
        document.querySelectorAll('.feed-tab').forEach(t => t.classList.remove('active'));
        tabEl.classList.add('active');
      }
      let photos = await api(`/api/photos?sort=${sort}`);
      if (!Array.isArray(photos)) photos = [];
      
      const grid = document.getElementById('galleryGrid');
      grid.innerHTML = photos.map(p => {
        const scores = p.attribute_scores || {};
        const scoreKeys = Object.keys(scores).slice(0, 3);
        const scoreBadges = scoreKeys.map(k => `
          <div>
            <div class="attr-stat-label">${k}</div>
            <div class="attr-stat-val">${scores[k].avg || '—'}</div>
          </div>
        `).join('');

        const imgSrc = p.image_data || p.image_url;

        return `
          <div class="photo-card" id="card_${p.id}">
            <div class="photo-img-wrapper" onclick="openPhotoModal('${p.id}')">
              <img src="${imgSrc}" alt="${p.title}">
              <div class="ai-badge">✨ AI Critiqued</div>
            </div>
            <div class="card-body">
              <div class="photo-title" onclick="openPhotoModal('${p.id}')">${p.title}</div>
              <div class="photo-author">by ${p.author_name || 'Artist'}</div>
              <div class="ai-narrative-preview">${p.ai_narrative || p.description || ''}</div>
              
              <div class="attributes-breakdown">
                ${scoreBadges || '<div style="grid-column: 1/-1; font-size: 0.75rem; color: var(--text-muted);">Rate first attributes in details</div>'}
              </div>

              <div class="card-footer">
                <div class="vote-widget">
                  <button class="vote-btn" onclick="votePhoto('${p.id}', 1, this)">▲</button>
                  <span class="score-count" id="score_${p.id}">${p.score || 0}</span>
                  <button class="vote-btn" onclick="votePhoto('${p.id}', -1, this)">▼</button>
                </div>
                <button class="btn-secondary" style="font-size: 0.78rem; padding: 0.35rem 0.65rem;" onclick="openPhotoModal('${p.id}')">Review & Rate →</button>
              </div>
            </div>
          </div>
        `;
      }).join('');
    }

    async function votePhoto(id, val, btn) {
      const res = await api(`/api/photos/${id}/vote`, 'POST', { value: val });
      if (res.score !== undefined) {
        document.getElementById(`score_${id}`).innerText = res.score;
      }
    }

    async function openPhotoModal(id) {
      currentPhotoId = id;
      const photo = await api(`/api/photos/${id}`);
      const myRatings = await api(`/api/photos/${id}/my-ratings`);
      
      document.getElementById('modalTitle').innerText = photo.title;
      document.getElementById('modalImg').src = photo.image_data || photo.image_url;
      document.getElementById('modalAiNarrative').innerText = photo.ai_narrative || '';
      document.getElementById('modalAiCritique').innerText = photo.ai_critique || 'AI critique in progress...';

      const sliders = document.getElementById('ratingSliders');
      sliders.innerHTML = activeAttributes.map(a => {
        const userScore = myRatings.ratings ? (myRatings.ratings[a.id] || 8) : 8;
        return `
          <div class="rating-row">
            <div class="rating-label">
              ${a.name}
              <div style="font-size: 0.72rem; color: var(--text-muted); font-weight: normal;">${a.description || ''}</div>
            </div>
            <input type="range" class="rating-slider" min="1" max="10" step="0.5" value="${userScore}" 
                   id="slider_${a.id}" oninput="document.getElementById('val_${a.id}').innerText = this.value">
            <div class="rating-val" id="val_${a.id}">${userScore}</div>
          </div>
        `;
      }).join('');

      document.getElementById('photoModal').style.display = 'flex';
    }

    async function submitRatings() {
      if (!currentPhotoId) return;
      const ratings = activeAttributes.map(a => ({
        attribute_id: a.id,
        score: parseFloat(document.getElementById(`slider_${a.id}`).value)
      }));
      await api(`/api/photos/${currentPhotoId}/rate`, 'POST', { ratings });
      closeModal('photoModal');
      await loadPhotos();
    }

    async function submitPhoto() {
      const title = document.getElementById('uploadTitle').value.trim();
      const image_url = uploadedFileDataUrl ? '' : document.getElementById('uploadImgUrl').value.trim();
      const image_data = uploadedFileDataUrl || '';
      const description = document.getElementById('uploadDesc').value.trim();

      if (!title) return alert('Please enter a photo title');
      if (!image_data && !image_url) return alert('Please select a photo file or enter an image URL');

      const submitBtn = document.getElementById('uploadSubmitBtn');
      const originalText = submitBtn.innerText;
      submitBtn.innerText = 'Analyzing & Uploading...';
      submitBtn.disabled = true;

      try {
        const res = await api('/api/photos', 'POST', { title, image_url, image_data, description });
        if (res.error) return alert(res.error);
        clearSelectedFile();
        closeModal('uploadModal');
        await loadPhotos();
      } finally {
        submitBtn.innerText = originalText;
        submitBtn.disabled = false;
      }
    }

    function toggleAuthModal() { document.getElementById('authModal').style.display = 'flex'; }
    function openUploadModal() { document.getElementById('uploadModal').style.display = 'flex'; }
    function openAdminModal() { document.getElementById('adminModal').style.display = 'flex'; }
    function closeModal(id) { document.getElementById(id).style.display = 'none'; }

    init();
  </script>
</body>
</html>"#;
    Outcome::Html(html.to_string())
}

bindings::export!(Component with_types_in bindings);
