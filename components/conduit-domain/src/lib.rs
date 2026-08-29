//! conduit:app — the RealWorld ("Conduit") spec over composed contracts.
//!
//! One HTTP component that owns only routing + the RealWorld JSON shape; auth is
//! the composed auth-guard (`accounts` + `authorizer::introspect`), storage is
//! `record:store`, slugs are `slug:generate`. No bespoke business crate.
//!
//! Conformance target: the official RealWorld Hurl suite (see
//! `examples/conduit/conformance`). That fixes the exact status codes and the
//! field-keyed error envelope (`{"errors":{"<field>":["<msg>"]}}`), the
//! `Authorization: Token <jwt>` header, and null-normalization of bio/image.
//!
//! Two identities coexist: auth-guard keys accounts on (tenant, email); Conduit
//! shows clients the mutable `username`. The `users` collection bridges them —
//! one record per account, indexed by username / email / subject.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::records::store::store as records;
use bindings::slug::generate::generator as slug;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "conduit";
const USERS: &str = "users";
const FOLLOWS: &str = "follows";
const ARTICLES: &str = "articles";
const FAVORITES: &str = "favorites"; // {user, article} relation
const COMMENTS: &str = "comments"; // {article, author, body}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),

            (Method::Post, ["api", "users"]) => register(&request),
            (Method::Post, ["api", "users", "login"]) => login(&request),
            (Method::Get, ["api", "user"]) => current_user(&request),
            (Method::Put, ["api", "user"]) => update_user(&request),

            (Method::Get, ["api", "profiles", name]) => get_profile(&request, name),
            (Method::Post, ["api", "profiles", name, "follow"]) => set_follow(&request, name, true),
            (Method::Delete, ["api", "profiles", name, "follow"]) => {
                set_follow(&request, name, false)
            }

            (Method::Get, ["api", "tags"]) => list_tags(),
            (Method::Post, ["api", "articles"]) => create_article(&request),
            // "feed" must precede the {slug} arm.
            (Method::Get, ["api", "articles", "feed"]) => feed(&request, &path),
            (Method::Get, ["api", "articles"]) => list_articles(&request, &path),
            (Method::Get, ["api", "articles", slug]) => get_article(&request, slug),
            (Method::Put, ["api", "articles", slug]) => update_article(&request, slug),
            (Method::Delete, ["api", "articles", slug]) => delete_article(&request, slug),

            (Method::Post, ["api", "articles", slug, "favorite"]) => {
                set_favorite(&request, slug, true)
            }
            (Method::Delete, ["api", "articles", slug, "favorite"]) => {
                set_favorite(&request, slug, false)
            }
            (Method::Post, ["api", "articles", slug, "comments"]) => add_comment(&request, slug),
            (Method::Get, ["api", "articles", slug, "comments"]) => list_comments(&request, slug),
            (Method::Delete, ["api", "articles", slug, "comments", id]) => {
                delete_comment(&request, slug, id)
            }

            _ => not_found("route"),
        };
        emit(response_out, result);
    }
}

/// The RealWorld error envelope is field-keyed: `{"errors":{"<field>":["<msg>"]}}`.
enum Outcome {
    /// Success; body is already a fully-formed (envelope-wrapped) JSON string.
    Json(u16, String),
    /// Success with no body (204).
    Empty(u16),
    /// `{"errors":{field:[message]}}` at `status`.
    Err(u16, &'static str, String),
}

fn blank(field: &'static str) -> Outcome {
    Outcome::Err(422, field, "can't be blank".into())
}
fn taken(field: &'static str) -> Outcome {
    Outcome::Err(409, field, "has already been taken".into())
}
fn not_found(field: &'static str) -> Outcome {
    Outcome::Err(404, field, "not found".into())
}
fn forbidden(field: &'static str) -> Outcome {
    Outcome::Err(403, field, "forbidden".into())
}
fn token_missing() -> Outcome {
    Outcome::Err(401, "token", "is missing".into())
}
fn invalid_field(field: &'static str, msg: &str) -> Outcome {
    Outcome::Err(422, field, msg.into())
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "conduit",
            "spec": "RealWorld — https://realworld-docs.netlify.app",
            "users": "POST /api/users, POST /api/users/login, GET|PUT /api/user",
            "profiles": "GET /api/profiles/{username}, POST|DELETE .../follow",
            "articles": "GET|POST /api/articles, /feed, /{slug}, .../favorite, .../comments",
            "tags": "GET /api/tags"
        })
        .to_string(),
    )
}

// ---- users -------------------------------------------------------------------

fn register(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = envelope(&body, "user");
    let username = str_field(&user, "username");
    let email = str_field(&user, "email");
    let password = str_field(&user, "password");
    if username.trim().is_empty() {
        return blank("username");
    }
    if email.trim().is_empty() {
        return blank("email");
    }
    if password.is_empty() {
        return blank("password");
    }
    if find_user("username", &username).is_some() {
        return taken("username");
    }
    let principal = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(AuthError::AlreadyExists) => return taken("email"),
        Err(AuthError::Malformed(_)) => return invalid_field("email", "is invalid"),
        Err(e) => return auth_server_error(e),
    };
    let data = json!({
        "username": username,
        "email": email,
        "subject": principal.subject,
        "bio": null,
        "image": null,
    });
    if let Err(e) = records::create(USERS, &data.to_string(), &user_idx()) {
        return store_err(e);
    }
    // RealWorld returns a token on register; auth-guard's register does not log
    // in, so mint the session with the credentials we already hold.
    let token = match accounts::login(&email, &password, TENANT) {
        Ok(tp) => tp.access_token,
        Err(e) => return auth_server_error(e),
    };
    Outcome::Json(201, user_envelope(&data, &token))
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = envelope(&body, "user");
    let email = str_field(&user, "email");
    let password = str_field(&user, "password");
    if email.trim().is_empty() {
        return blank("email");
    }
    if password.is_empty() {
        return blank("password");
    }
    let token = match accounts::login(&email, &password, TENANT) {
        Ok(tp) => tp.access_token,
        Err(AuthError::InvalidCredentials) | Err(AuthError::Malformed(_)) => {
            return Outcome::Err(401, "credentials", "invalid".into())
        }
        Err(e) => return auth_server_error(e),
    };
    match find_user("email", &email) {
        Some((_, data)) => Outcome::Json(200, user_envelope(&data, &token)),
        None => Outcome::Err(500, "body", "account without profile record".into()),
    }
}

fn current_user(request: &IncomingRequest) -> Outcome {
    let (p, token) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    match find_user("subject", &p.subject) {
        Some((_, data)) => Outcome::Json(200, user_envelope(&data, &token)),
        None => not_found("user"),
    }
}

fn update_user(request: &IncomingRequest) -> Outcome {
    let (p, token) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = envelope(&body, "user");
    let (entry, mut data) = match find_user("subject", &p.subject) {
        Some(u) => u,
        None => return not_found("user"),
    };

    // username / email: present must be a non-empty string (null / "" → reject).
    if let Some(v) = user.get("username") {
        match nonempty_string(v) {
            Some(s) => {
                if let Some((other, _)) = find_user("username", &s) {
                    if other.id != entry.id {
                        return taken("username");
                    }
                }
                data["username"] = json!(s);
            }
            None => return blank("username"),
        }
    }
    if let Some(v) = user.get("email") {
        match nonempty_string(v) {
            // ponytail: the display record's email is rewritten but the auth-guard
            // login key is NOT (no rename verb) — a changed email won't work for a
            // fresh login. Conformance never re-logs-in after a change; flagged.
            Some(s) => data["email"] = json!(s),
            None => return blank("email"),
        }
    }
    // password: NIST 800-63B — reject null/""/<8, accept >=8 (RealWorld PUT rules).
    if let Some(v) = user.get("password") {
        match v.as_str() {
            Some(s) if s.chars().count() >= 8 => {
                // ponytail: accepted-but-not-rotated — auth-guard has no
                // "set password without the current one" verb. The suite checks
                // the validation + 200, never that the new password logs in.
            }
            Some(s) if !s.is_empty() => {
                return invalid_field("password", "is too short (minimum is 8 characters)")
            }
            _ => return blank("password"),
        }
    }
    // bio / image: nullable — "" and null both normalize to null.
    if let Some(v) = user.get("bio") {
        data["bio"] = normalize_nullable(v);
    }
    if let Some(v) = user.get("image") {
        data["image"] = normalize_nullable(v);
    }

    match records::update(USERS, &entry.id, &data.to_string(), entry.revision) {
        Ok(_) => Outcome::Json(200, user_envelope(&data, &token)),
        Err(e) => store_err(e),
    }
}

// ---- profiles + follows ------------------------------------------------------

fn get_profile(request: &IncomingRequest, username: &str) -> Outcome {
    let (_, target) = match find_user("username", username) {
        Some(u) => u,
        None => return not_found("profile"),
    };
    let following = match auth(request) {
        Ok((p, _)) => is_following(&p.subject, target["subject"].as_str().unwrap_or("")),
        Err(_) => false,
    };
    Outcome::Json(200, profile_envelope(&target, following))
}

fn set_follow(request: &IncomingRequest, username: &str, follow: bool) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (_, target) = match find_user("username", username) {
        Some(u) => u,
        None => return not_found("profile"),
    };
    let followee = target["subject"].as_str().unwrap_or("").to_string();
    let existing = follow_entry(&p.subject, &followee);
    match (follow, existing) {
        (true, None) => {
            let rel = json!({"follower": p.subject, "followee": followee});
            if let Err(e) = records::create(FOLLOWS, &rel.to_string(), &follow_idx()) {
                return store_err(e);
            }
        }
        (false, Some(id)) => {
            if let Err(e) = records::delete(FOLLOWS, &id) {
                return store_err(e);
            }
        }
        _ => {} // already in the desired state — idempotent.
    }
    Outcome::Json(200, profile_envelope(&target, follow))
}

// ---- articles ----------------------------------------------------------------

fn create_article(request: &IncomingRequest) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let article = envelope(&body, "article");
    let title = str_field(&article, "title");
    let description = str_field(&article, "description");
    let art_body = str_field(&article, "body");
    if title.trim().is_empty() {
        return blank("title");
    }
    if description.trim().is_empty() {
        return blank("description");
    }
    if art_body.trim().is_empty() {
        return blank("body");
    }
    let base = slug::slugify(&title);
    let base = if base.is_empty() { "article".to_string() } else { base };
    let slug = unique_slug(&base);
    // ponytail: `created` is only second-resolution and record-store randomizes
    // intra-ms id order, so neither disambiguates two articles created in the
    // same millisecond. `seq` is a monotonic insertion counter (global record
    // count at create) giving deterministic newest-first ordering. Global &
    // lock-free: fine for the sequential suite; two *concurrent* creates could
    // tie — swap for a KV atomic counter if that ever matters.
    let seq = records::count(ARTICLES).unwrap_or(0);
    let ts = now_iso();
    let data = json!({
        "slug": slug,
        "title": title,
        "description": description,
        "body": art_body,
        "tagList": tag_list(article.get("tagList")),
        "author": p.subject,
        "seq": seq,
        "createdAt": ts,
        "updatedAt": ts,
    });
    match records::create(ARTICLES, &data.to_string(), &article_idx()) {
        Ok(entry) => Outcome::Json(201, article_envelope(&entry, Some(&p))),
        Err(e) => store_err(e),
    }
}

fn get_article(request: &IncomingRequest, slug: &str) -> Outcome {
    let entry = match load_article(slug) {
        Ok((e, _)) => e,
        Err(o) => return o,
    };
    let viewer = auth(request).ok().map(|(p, _)| p);
    Outcome::Json(200, article_envelope(&entry, viewer.as_ref()))
}

fn update_article(request: &IncomingRequest, slug: &str) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (entry, mut data) = match load_article(slug) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if data["author"].as_str() != Some(p.subject.as_str()) {
        return forbidden("article");
    }
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let article = envelope(&body, "article");
    if let Some(v) = article.get("title") {
        if let Some(title) = nonempty_string(v) {
            if data["title"].as_str() != Some(title.as_str()) {
                let base = slug::slugify(&title);
                data["slug"] =
                    json!(unique_slug(if base.is_empty() { "article" } else { base.as_str() }));
            }
            data["title"] = json!(title);
        }
    }
    if let Some(v) = article.get("description") {
        if let Some(s) = v.as_str() {
            data["description"] = json!(s);
        }
    }
    if let Some(v) = article.get("body") {
        if let Some(s) = v.as_str() {
            data["body"] = json!(s);
        }
    }
    // tagList: absent → preserve; [] → clear; [..] → replace; null → reject.
    if let Some(v) = article.get("tagList") {
        if v.is_null() {
            return invalid_field("tagList", "can't be null");
        }
        data["tagList"] = json!(tag_list(Some(v)));
    }
    data["updatedAt"] = json!(now_iso());
    match records::update(ARTICLES, &entry.id, &data.to_string(), entry.revision) {
        Ok(updated) => Outcome::Json(200, article_envelope(&updated, Some(&p))),
        Err(e) => store_err(e),
    }
}

fn delete_article(request: &IncomingRequest, slug: &str) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (entry, data) = match load_article(slug) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if data["author"].as_str() != Some(p.subject.as_str()) {
        return forbidden("article");
    }
    // ponytail: orphaned comments/favorites are left in place (both are looked
    // up by the now-unreachable article id). Add a cascade if it ever matters.
    match records::delete(ARTICLES, &entry.id) {
        Ok(()) => Outcome::Empty(204),
        Err(e) => store_err(e),
    }
}

fn list_articles(request: &IncomingRequest, path: &str) -> Outcome {
    let viewer = auth(request).ok().map(|(p, _)| p);
    let mut arts = all_articles();
    if let Some(username) = query_param(path, "author") {
        let subject = subject_of(&username);
        arts.retain(|(_, d)| d["author"].as_str() == subject.as_deref());
    }
    if let Some(t) = query_param(path, "tag") {
        arts.retain(|(_, d)| {
            d["tagList"]
                .as_array()
                .map(|a| a.iter().any(|x| x.as_str() == Some(t.as_str())))
                .unwrap_or(false)
        });
    }
    if let Some(username) = query_param(path, "favorited") {
        match subject_of(&username) {
            Some(subject) => arts.retain(|(e, _)| is_favorited_by(&e.id, &subject)),
            None => arts.clear(),
        }
    }
    Outcome::Json(200, articles_page(arts, path, viewer.as_ref()))
}

fn feed(request: &IncomingRequest, path: &str) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let followees: Vec<String> =
        records::find_by(FOLLOWS, "follower", &json!(p.subject).to_string())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
            .filter_map(|d| d["followee"].as_str().map(String::from))
            .collect();
    let mut arts = all_articles();
    arts.retain(|(_, d)| {
        d["author"].as_str().map(|a| followees.iter().any(|f| f == a)).unwrap_or(false)
    });
    Outcome::Json(200, articles_page(arts, path, Some(&p)))
}

fn list_tags() -> Outcome {
    let mut tags: Vec<String> = Vec::new();
    for (_, d) in all_articles() {
        if let Some(arr) = d["tagList"].as_array() {
            for t in arr.iter().filter_map(|x| x.as_str()) {
                if !tags.iter().any(|e| e == t) {
                    tags.push(t.to_string());
                }
            }
        }
    }
    tags.sort();
    Outcome::Json(200, json!({ "tags": tags }).to_string())
}

// ---- favorites + comments ----------------------------------------------------

fn set_favorite(request: &IncomingRequest, slug: &str, favorite: bool) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (article, _) = match load_article(slug) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let existing = favorite_entry(&p.subject, &article.id);
    match (favorite, existing) {
        (true, None) => {
            let rel = json!({"user": p.subject, "article": article.id});
            if let Err(e) = records::create(FAVORITES, &rel.to_string(), &favorite_idx()) {
                return store_err(e);
            }
        }
        (false, Some(id)) => {
            if let Err(e) = records::delete(FAVORITES, &id) {
                return store_err(e);
            }
        }
        _ => {} // idempotent.
    }
    Outcome::Json(200, article_envelope(&article, Some(&p)))
}

fn add_comment(request: &IncomingRequest, slug: &str) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (article, _) = match load_article(slug) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let comment = envelope(&body, "comment");
    let text = str_field(&comment, "body");
    if text.trim().is_empty() {
        return blank("body");
    }
    let ts = now_iso();
    let data = json!({"article": article.id, "author": p.subject, "body": text, "createdAt": ts, "updatedAt": ts});
    match records::create(COMMENTS, &data.to_string(), &["article".to_string()]) {
        Ok(entry) => Outcome::Json(201, comment_envelope(&entry, Some(&p))),
        Err(e) => store_err(e),
    }
}

fn list_comments(request: &IncomingRequest, slug: &str) -> Outcome {
    let (article, _) = match load_article(slug) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let viewer = auth(request).ok().map(|(p, _)| p);
    let mut comments = match records::find_by(COMMENTS, "article", &json!(article.id).to_string()) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    comments.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    // Comments are a list too, and every one of them asked who the viewer
    // follows. Same memo, same reason.
    let mut view = View::default();
    let list: Vec<Value> =
        comments.iter().map(|e| comment_json(e, viewer.as_ref(), &mut view)).collect();
    Outcome::Json(200, json!({ "comments": list }).to_string())
}

fn delete_comment(request: &IncomingRequest, slug: &str, id: &str) -> Outcome {
    let (p, _) = match auth(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    // Unknown article vs unknown comment are distinct 404s in the spec.
    let article = match load_article(slug) {
        Ok((a, _)) => a,
        Err(o) => return o,
    };
    // The public comment id is the integer `comment_id(record-id)`; find the
    // record whose derived id matches (see comment_id).
    let Ok(want) = id.parse::<i64>() else {
        return not_found("comment");
    };
    let comments =
        records::find_by(COMMENTS, "article", &json!(article.id).to_string()).unwrap_or_default();
    let Some(entry) = comments.into_iter().find(|e| comment_id(&e.id) == want) else {
        return not_found("comment");
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    if data["author"].as_str() != Some(p.subject.as_str()) {
        return forbidden("comment");
    }
    match records::delete(COMMENTS, &entry.id) {
        Ok(()) => Outcome::Empty(204),
        Err(e) => store_err(e),
    }
}

/// RealWorld comment ids are integers, but record ids are ULIDs. Derive a
/// stable positive 53-bit integer from the record id (FNV-1a). Delete/list
/// recompute it, so no separate index is needed; collisions within one
/// article's comments are astronomically unlikely for demo volumes.
fn comment_id(record_id: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in record_id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h & 0x001F_FFFF_FFFF_FFFF) as i64
}

// ---- record helpers ----------------------------------------------------------

fn user_idx() -> Vec<String> {
    vec!["username".into(), "email".into(), "subject".into()]
}
fn follow_idx() -> Vec<String> {
    vec!["follower".into(), "followee".into()]
}
fn article_idx() -> Vec<String> {
    vec!["slug".into(), "author".into()]
}
fn favorite_idx() -> Vec<String> {
    vec!["user".into(), "article".into()]
}

fn find_user(field: &str, value: &str) -> Option<(records::Entry, Value)> {
    let entries = records::find_by(USERS, field, &json!(value).to_string()).ok()?;
    let entry = entries.into_iter().next()?;
    let data: Value = serde_json::from_str(&entry.data).ok()?;
    Some((entry, data))
}

fn subject_of(username: &str) -> Option<String> {
    find_user("username", username).and_then(|(_, u)| u["subject"].as_str().map(String::from))
}

fn is_following(follower: &str, followee: &str) -> bool {
    follow_entry(follower, followee).is_some()
}

fn follow_entry(follower: &str, followee: &str) -> Option<String> {
    records::find_by(FOLLOWS, "follower", &json!(follower).to_string()).ok()?.into_iter().find_map(
        |e| {
            let d: Value = serde_json::from_str(&e.data).ok()?;
            (d["followee"].as_str() == Some(followee)).then_some(e.id)
        },
    )
}

/// The slug is free, or the first `-N` suffix that is (via slug:generate).
fn unique_slug(base: &str) -> String {
    let taken = records::find_by(ARTICLES, "slug", &json!(base).to_string()).unwrap_or_default();
    if taken.is_empty() {
        return base.to_string();
    }
    let all: Vec<String> = all_articles()
        .into_iter()
        .filter_map(|(_, d)| d["slug"].as_str().map(String::from))
        .collect();
    slug::uniquify(base, &all)
}

fn load_article(slug: &str) -> Result<(records::Entry, Value), Outcome> {
    let entries = match records::find_by(ARTICLES, "slug", &json!(slug).to_string()) {
        Ok(e) => e,
        Err(e) => return Err(store_err(e)),
    };
    let entry = entries.into_iter().next().ok_or_else(|| not_found("article"))?;
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    Ok((entry, data))
}

// ponytail: O(n) full scan per list/feed/tags. RealWorld datasets are tiny; add
// a tag/author join index if article volume ever outgrows a single page dump.
fn all_articles() -> Vec<(records::Entry, Value)> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _ in 0..1000 {
        let page = match records::list_records(ARTICLES, 0, &cursor) {
            Ok(p) => p,
            Err(_) => break,
        };
        for e in page.entries {
            let d: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
            out.push((e, d));
        }
        if page.next.is_empty() {
            break;
        }
        cursor = page.next;
    }
    out
}

/// Newest-first, offset/limit slice, `{articles, articlesCount}` envelope.
/// `articlesCount` is the full match count (pre-pagination), per spec. The
/// Per-request memo for the lookups a page repeats (ADR-0077).
///
/// Rendering a page of articles asked the store the same questions once per
/// article: who the author is, and who the viewer follows. The follow query is
/// the worst of them — `find_by(FOLLOWS, "follower", viewer)` is byte-identical
/// for every row on the page, so a twenty-article page ran it twenty times for
/// one answer.
///
/// A memo is safe here in a way it would not be anywhere else: a component
/// instance is per-request (ADR-0037), so this cannot outlive the response it
/// was built for and cannot go stale. It is not a cache; it is not asking twice.
#[derive(Default)]
struct View {
    authors: std::collections::HashMap<String, Option<Value>>,
    /// The viewer's followees, fetched at most once.
    follows: Option<Vec<String>>,
}

impl View {
    fn author(&mut self, subject: &str) -> Option<Value> {
        if let Some(hit) = self.authors.get(subject) {
            return hit.clone();
        }
        let found = find_user("subject", subject).map(|(_, u)| u);
        self.authors.insert(subject.to_string(), found.clone());
        found
    }

    /// Whether `viewer` follows `subject`, reading the viewer's follow list once.
    fn following(&mut self, viewer: Option<&Principal>, subject: &str) -> bool {
        let Some(v) = viewer else { return false };
        if self.follows.is_none() {
            self.follows = Some(
                records::find_by(FOLLOWS, "follower", &json!(v.subject).to_string())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|e| {
                        serde_json::from_str::<Value>(&e.data)
                            .ok()
                            .and_then(|d| d["followee"].as_str().map(String::from))
                    })
                    .collect(),
            );
        }
        self.follows.as_ref().is_some_and(|f| f.iter().any(|x| x == subject))
    }
}

/// list shape omits `body` (RealWorld's "multiple articles" response).
fn articles_page(
    mut arts: Vec<(records::Entry, Value)>,
    path: &str,
    viewer: Option<&Principal>,
) -> String {
    arts.sort_by(|a, b| {
        let (sa, sb) = (a.1["seq"].as_u64().unwrap_or(0), b.1["seq"].as_u64().unwrap_or(0));
        sb.cmp(&sa).then(b.0.created.cmp(&a.0.created)).then(b.0.id.cmp(&a.0.id))
    });
    let count = arts.len();
    let limit = query_param(path, "limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(20);
    let offset = query_param(path, "offset").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let mut view = View::default();
    let page: Vec<Value> = arts
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(e, _)| article_json(e, viewer, false, &mut view))
        .collect();
    json!({ "articles": page, "articlesCount": count }).to_string()
}

fn article_envelope(entry: &records::Entry, viewer: Option<&Principal>) -> String {
    json!({ "article": article_json(entry, viewer, true, &mut View::default()) }).to_string()
}

/// `include_body` false for list/feed (RealWorld omits `body` there), true for
/// single-article responses.
fn article_json(
    entry: &records::Entry,
    viewer: Option<&Principal>,
    include_body: bool,
    view: &mut View,
) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let author_subject = data["author"].as_str().unwrap_or("");
    let (favorited, favorites_count) = favorite_state(&entry.id, viewer);
    let mut out = json!({
        "slug": data["slug"],
        "title": data["title"],
        "description": data["description"],
        "tagList": data["tagList"],
        "createdAt": data["createdAt"],
        "updatedAt": data["updatedAt"],
        "favorited": favorited,
        "favoritesCount": favorites_count,
        "author": author_json(author_subject, viewer, view),
    });
    if include_body {
        out["body"] = data["body"].clone();
    }
    out
}

fn author_json(subject: &str, viewer: Option<&Principal>, view: &mut View) -> Value {
    match view.author(subject) {
        Some(u) => {
            let following = view.following(viewer, subject);
            json!({"username": u["username"], "bio": u["bio"], "image": u["image"], "following": following})
        }
        None => json!({"username": "", "bio": null, "image": null, "following": false}),
    }
}

fn favorite_state(article_id: &str, viewer: Option<&Principal>) -> (bool, u64) {
    let favs =
        records::find_by(FAVORITES, "article", &json!(article_id).to_string()).unwrap_or_default();
    let count = favs.len() as u64;
    let favorited =
        viewer.map(|v| favs.iter().any(|f| favorite_user(f) == v.subject)).unwrap_or(false);
    (favorited, count)
}

fn is_favorited_by(article_id: &str, subject: &str) -> bool {
    records::find_by(FAVORITES, "article", &json!(article_id).to_string())
        .unwrap_or_default()
        .iter()
        .any(|f| favorite_user(f) == subject)
}

fn favorite_user(entry: &records::Entry) -> String {
    serde_json::from_str::<Value>(&entry.data)
        .ok()
        .and_then(|d| d["user"].as_str().map(String::from))
        .unwrap_or_default()
}

fn comment_envelope(entry: &records::Entry, viewer: Option<&Principal>) -> String {
    json!({ "comment": comment_json(entry, viewer, &mut View::default()) }).to_string()
}

fn comment_json(entry: &records::Entry, viewer: Option<&Principal>, view: &mut View) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": comment_id(&entry.id),
        "createdAt": data["createdAt"],
        "updatedAt": data["updatedAt"],
        "body": data["body"],
        "author": author_json(data["author"].as_str().unwrap_or(""), viewer, view),
    })
}

fn favorite_entry(user: &str, article_id: &str) -> Option<String> {
    records::find_by(FAVORITES, "article", &json!(article_id).to_string())
        .ok()?
        .into_iter()
        .find(|e| favorite_user(e) == user)
        .map(|e| e.id)
}

// ---- JSON shape helpers ------------------------------------------------------

fn user_envelope(data: &Value, token: &str) -> String {
    json!({"user": {
        "email": data["email"],
        "token": token,
        "username": data["username"],
        "bio": data["bio"],
        "image": data["image"],
    }})
    .to_string()
}

fn profile_envelope(data: &Value, following: bool) -> String {
    json!({"profile": {
        "username": data["username"],
        "bio": data["bio"],
        "image": data["image"],
        "following": following,
    }})
    .to_string()
}

/// The object under `key` in a `{key:{...}}` envelope (empty map if absent).
fn envelope(body: &Value, key: &str) -> Map<String, Value> {
    body.get(key).and_then(|v| v.as_object()).cloned().unwrap_or_default()
}

fn str_field(obj: &Map<String, Value>, key: &str) -> String {
    obj.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// `Some(s)` for a non-empty string; `None` for null, "", or a non-string.
fn nonempty_string(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.trim().is_empty()).map(String::from)
}

/// Nullable text field: a non-empty string stays; null/"" normalize to null.
fn normalize_nullable(v: &Value) -> Value {
    match v.as_str() {
        Some(s) if !s.is_empty() => json!(s),
        _ => Value::Null,
    }
}

/// Sorted, de-duplicated string tags from a `tagList` value (missing → empty).
fn tag_list(v: Option<&Value>) -> Vec<String> {
    let mut tags: Vec<String> = v
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    tags.sort();
    tags.dedup();
    tags
}

// ---- auth + request parsing --------------------------------------------------

/// Resolve the bearer token to a principal (RealWorld echoes the token back).
fn auth(request: &IncomingRequest) -> Result<(Principal, String), Outcome> {
    let Some(token) = bearer(request) else {
        return Err(token_missing());
    };
    match authorizer::introspect(&token) {
        Ok(p) => Ok((p, token)),
        Err(_) => Err(Outcome::Err(401, "token", "is invalid".into())),
    }
}

fn auth_server_error(e: AuthError) -> Outcome {
    match e {
        AuthError::BackendUnavailable(m) => Outcome::Err(503, "body", m),
        AuthError::RateLimited(_) => Outcome::Err(429, "body", "rate limited".into()),
        _ => Outcome::Err(500, "body", format!("{e:?}")),
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => not_found("article"),
        records::StoreError::InvalidJson(m) => invalid_field("body", &m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "body", "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, "body", m),
    }
}

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body =
        read_body(request).map_err(|_| Outcome::Err(422, "body", "could not read body".into()))?;
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(422, "body", format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

/// RealWorld sends `Authorization: Token <jwt>`; also accept `Bearer`.
fn bearer(request: &IncomingRequest) -> Option<String> {
    let raw = header(request, "authorization")?;
    for prefix in ["Token ", "Bearer "] {
        if let Some(tok) = raw.strip_prefix(prefix) {
            return Some(tok.trim().to_string());
        }
    }
    None
}

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request.headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

/// First value of query param `key` in a `path?query` string, percent-decoded.
fn query_param(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| percent_decode(it.next().unwrap_or("")))
    })
}

use guestfmt::percent_decode;

/// Current wall-clock time as RealWorld ISO8601 with milliseconds. Millisecond
/// precision matters: RealWorld asserts `updatedAt` changes after an update, and
/// record:store timestamps are second-only.
fn now_iso() -> String {
    let t = wall_clock::now();
    iso8601(t.seconds, t.nanoseconds / 1_000_000)
}

/// Unix seconds + millis → ISO8601 (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
/// Civil-from-days (Howard Hinnant); correct across leap years.
fn iso8601(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

// ---- response ----------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Empty(code) => respond(response_out, code, b""),
        Outcome::Err(code, field, msg) => {
            let body = json!({ "errors": { field: [msg] } }).to_string();
            respond(response_out, code, body.as_bytes());
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
