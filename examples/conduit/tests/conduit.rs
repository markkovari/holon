//! E2E for the conduit domain (docs/apps/CONDUIT.md rung 1) running as ONE composed wasm
//! HTTP component on the NATIVE Rust host (`host/` comp-host over wasmtime). No
//! Node, no jco: every route is the Rust conduit-domain component orchestrating
//! auth-guard + records:store, linked into one .wasm and served over real HTTP.
//!
//! Flow: register two users -> tokens issued -> current user + isolation ->
//! update profile (bio/image) -> follow/unfollow with the `following` flag ->
//! RealWorld envelopes and the `Token <jwt>` header throughout.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3018";

/// Kills the spawned host when the test ends (even on panic).
struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve repo root")
}

fn base() -> String {
    format!("http://{ADDR}")
}

/// Send a request; return (status, json-body). Non-2xx is a value, not a panic.
fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let mut r = ureq::request(method, &url);
    if let Some(t) = token {
        r = r.set("authorization", &format!("Token {t}"));
    }
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: transport error: {e}"),
    };
    let status = resp.status();
    let text = resp.into_string().unwrap_or_default();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, value)
}

fn start_host() -> HostGuard {
    let root = repo_root();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/conduit_domain.composed.wasm");
    assert!(bin.exists(), "host binary not built: {bin:?} (run `just e2e-conduit`)");
    assert!(component.exists(), "composed wasm missing: {component:?} (run `just compose-conduit`)");

    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "conduit")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);

    // Wait for the listener to come up (GET / -> 200 usage).
    for _ in 0..200 {
        if let Ok(resp) = ureq::get(&base()).call() {
            if resp.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("host did not become ready at {}", base());
}

fn register(username: &str, email: &str, password: &str) -> String {
    let (status, body) = req(
        "POST",
        "/api/users",
        None,
        Some(json!({"user": {"username": username, "email": email, "password": password}})),
    );
    assert_eq!(status, 201, "register {username}: {body}");
    let user = &body["user"];
    assert_eq!(user["username"], username);
    assert_eq!(user["email"], email);
    user["token"].as_str().expect("token on register").to_string()
}

#[test]
fn conduit_rungs() {
    let _host = start_host();

    // --- register mints tokens -----------------------------------------------
    let alice = register("alice", "alice@example.test", "alice-pass-1");
    let bob = register("bob", "bob@example.test", "bob-pass-1");

    // duplicate username -> 422
    let (status, body) = req(
        "POST",
        "/api/users",
        None,
        Some(json!({"user": {"username": "alice", "email": "other@example.test", "password": "pw123456"}})),
    );
    assert_eq!(status, 409, "dup username should 409: {body}");

    // --- login round-trips the same identity ---------------------------------
    let (status, body) = req(
        "POST",
        "/api/users/login",
        None,
        Some(json!({"user": {"email": "alice@example.test", "password": "alice-pass-1"}})),
    );
    assert_eq!(status, 200, "login: {body}");
    assert_eq!(body["user"]["username"], "alice");
    assert!(body["user"]["token"].as_str().is_some());

    // wrong password -> 4xx, no token
    let (status, _) = req(
        "POST",
        "/api/users/login",
        None,
        Some(json!({"user": {"email": "alice@example.test", "password": "wrong"}})),
    );
    assert!(status >= 400, "bad login must fail, got {status}");

    // --- current user + auth required ----------------------------------------
    let (status, body) = req("GET", "/api/user", Some(&alice), None);
    assert_eq!(status, 200, "current user: {body}");
    assert_eq!(body["user"]["email"], "alice@example.test");

    let (status, _) = req("GET", "/api/user", None, None);
    assert_eq!(status, 401, "no token must 401");

    // --- update profile ------------------------------------------------------
    let (status, body) = req(
        "PUT",
        "/api/user",
        Some(&alice),
        Some(json!({"user": {"bio": "I like trains", "image": "https://img.test/a.png"}})),
    );
    assert_eq!(status, 200, "update: {body}");
    assert_eq!(body["user"]["bio"], "I like trains");
    assert_eq!(body["user"]["image"], "https://img.test/a.png");
    // persisted
    let (_, body) = req("GET", "/api/user", Some(&alice), None);
    assert_eq!(body["user"]["bio"], "I like trains");

    // --- profiles + follow flag ----------------------------------------------
    // bob views alice's profile, not yet following
    let (status, body) = req("GET", "/api/profiles/alice", Some(&bob), None);
    assert_eq!(status, 200, "profile: {body}");
    assert_eq!(body["profile"]["username"], "alice");
    assert_eq!(body["profile"]["following"], false);

    // bob follows alice
    let (status, body) = req("POST", "/api/profiles/alice/follow", Some(&bob), None);
    assert_eq!(status, 200, "follow: {body}");
    assert_eq!(body["profile"]["following"], true);

    // idempotent: following again still true
    let (_, body) = req("POST", "/api/profiles/alice/follow", Some(&bob), None);
    assert_eq!(body["profile"]["following"], true);

    // reflected on a fresh profile read
    let (_, body) = req("GET", "/api/profiles/alice", Some(&bob), None);
    assert_eq!(body["profile"]["following"], true);

    // anonymous read never shows following
    let (_, body) = req("GET", "/api/profiles/alice", None, None);
    assert_eq!(body["profile"]["following"], false);

    // bob unfollows
    let (status, body) = req("DELETE", "/api/profiles/alice/follow", Some(&bob), None);
    assert_eq!(status, 200, "unfollow: {body}");
    assert_eq!(body["profile"]["following"], false);

    // unknown profile -> 404
    let (status, _) = req("GET", "/api/profiles/nobody", Some(&bob), None);
    assert_eq!(status, 404, "unknown profile must 404");

    // ===== rung 2: articles =================================================

    // alice writes an article
    let (status, body) = req(
        "POST",
        "/api/articles",
        Some(&alice),
        Some(json!({"article": {
            "title": "How to train your dragon",
            "description": "Ever wonder how?",
            "body": "You have to believe.",
            "tagList": ["dragons", "training"]
        }})),
    );
    assert_eq!(status, 201, "create article: {body}");
    let art = &body["article"];
    let slug = art["slug"].as_str().expect("slug").to_string();
    assert_eq!(slug, "how-to-train-your-dragon");
    assert_eq!(art["author"]["username"], "alice");
    assert_eq!(art["favorited"], false);
    assert_eq!(art["favoritesCount"], 0);
    let created = art["createdAt"].as_str().unwrap_or("");
    assert!(
        created.len() == 24 && created.ends_with('Z') && created.as_bytes()[4] == b'-' && created.contains('T'),
        "createdAt must be ISO8601, got {created:?}"
    );
    assert_eq!(art["tagList"], json!(["dragons", "training"]));

    // fetch by slug (anonymous)
    let (status, body) = req("GET", &format!("/api/articles/{slug}"), None, None);
    assert_eq!(status, 200, "get article: {body}");
    assert_eq!(body["article"]["title"], "How to train your dragon");

    // list — count + filters
    let (_, body) = req("GET", "/api/articles", None, None);
    assert!(body["articlesCount"].as_u64().unwrap_or(0) >= 1, "list: {body}");
    let (_, body) = req("GET", "/api/articles?tag=dragons", None, None);
    assert_eq!(body["articlesCount"], 1, "tag filter: {body}");
    let (_, body) = req("GET", "/api/articles?tag=nope", None, None);
    assert_eq!(body["articlesCount"], 0, "unknown tag: {body}");
    let (_, body) = req("GET", "/api/articles?author=alice", None, None);
    assert_eq!(body["articlesCount"], 1, "author filter: {body}");
    let (_, body) = req("GET", "/api/articles?author=bob", None, None);
    assert_eq!(body["articlesCount"], 0, "author with no articles: {body}");

    // tags
    let (status, body) = req("GET", "/api/tags", None, None);
    assert_eq!(status, 200, "tags: {body}");
    let tags = body["tags"].as_array().cloned().unwrap_or_default();
    assert!(tags.iter().any(|t| t == "dragons") && tags.iter().any(|t| t == "training"), "tags: {body}");

    // feed: bob follows alice, sees her article, author.following == true
    req("POST", "/api/profiles/alice/follow", Some(&bob), None);
    let (status, body) = req("GET", "/api/articles/feed", Some(&bob), None);
    assert_eq!(status, 200, "feed: {body}");
    assert_eq!(body["articlesCount"], 1, "feed should show followed author: {body}");
    assert_eq!(body["articles"][0]["author"]["following"], true);
    // an unfollowed user's feed is empty
    let (_, body) = req("GET", "/api/articles/feed", Some(&alice), None);
    assert_eq!(body["articlesCount"], 0, "alice follows no one: {body}");

    // update (author only), slug stable when title unchanged
    let (status, body) = req(
        "PUT",
        &format!("/api/articles/{slug}"),
        Some(&alice),
        Some(json!({"article": {"body": "You have to believe in yourself."}})),
    );
    assert_eq!(status, 200, "update: {body}");
    assert_eq!(body["article"]["body"], "You have to believe in yourself.");
    assert_eq!(body["article"]["slug"], slug, "slug stable when title unchanged");

    // non-author cannot update -> 403
    let (status, _) = req("PUT", &format!("/api/articles/{slug}"), Some(&bob), Some(json!({"article": {"body": "hax"}})));
    assert_eq!(status, 403, "non-author update must 403");

    // ===== rung 3: favorites + comments =====================================

    // bob favorites alice's article
    let (status, body) = req("POST", &format!("/api/articles/{slug}/favorite"), Some(&bob), None);
    assert_eq!(status, 200, "favorite: {body}");
    assert_eq!(body["article"]["favorited"], true);
    assert_eq!(body["article"]["favoritesCount"], 1);

    // idempotent
    let (_, body) = req("POST", &format!("/api/articles/{slug}/favorite"), Some(&bob), None);
    assert_eq!(body["article"]["favoritesCount"], 1, "favorite is idempotent");

    // alice (not the favoriter) sees the count but favorited=false for her
    let (_, body) = req("GET", &format!("/api/articles/{slug}"), Some(&alice), None);
    assert_eq!(body["article"]["favoritesCount"], 1);
    assert_eq!(body["article"]["favorited"], false);

    // list ?favorited=bob returns it
    let (_, body) = req("GET", "/api/articles?favorited=bob", None, None);
    assert_eq!(body["articlesCount"], 1, "favorited filter: {body}");

    // unfavorite
    let (status, body) = req("DELETE", &format!("/api/articles/{slug}/favorite"), Some(&bob), None);
    assert_eq!(status, 200, "unfavorite: {body}");
    assert_eq!(body["article"]["favorited"], false);
    assert_eq!(body["article"]["favoritesCount"], 0);

    // bob comments on alice's article
    let (status, body) = req(
        "POST",
        &format!("/api/articles/{slug}/comments"),
        Some(&bob),
        Some(json!({"comment": {"body": "Great article!"}})),
    );
    assert_eq!(status, 201, "add comment: {body}");
    let comment_id = body["comment"]["id"].as_i64().expect("integer comment id").to_string();
    assert_eq!(body["comment"]["body"], "Great article!");
    assert_eq!(body["comment"]["author"]["username"], "bob");

    // list comments (anonymous)
    let (status, body) = req("GET", &format!("/api/articles/{slug}/comments"), None, None);
    assert_eq!(status, 200, "list comments: {body}");
    assert_eq!(body["comments"].as_array().map(|a| a.len()).unwrap_or(0), 1);

    // non-author cannot delete the comment -> 403
    let (status, _) = req("DELETE", &format!("/api/articles/{slug}/comments/{comment_id}"), Some(&alice), None);
    assert_eq!(status, 403, "non-author comment delete must 403");

    // author deletes their comment
    let (status, _) = req("DELETE", &format!("/api/articles/{slug}/comments/{comment_id}"), Some(&bob), None);
    assert_eq!(status, 204, "author comment delete");
    let (_, body) = req("GET", &format!("/api/articles/{slug}/comments"), None, None);
    assert_eq!(body["comments"].as_array().map(|a| a.len()).unwrap_or(9), 0, "comment gone");

    // ===== teardown: delete the article =====================================
    let (status, _) = req("DELETE", &format!("/api/articles/{slug}"), Some(&bob), None);
    assert_eq!(status, 403, "non-author delete must 403");
    let (status, _) = req("DELETE", &format!("/api/articles/{slug}"), Some(&alice), None);
    assert_eq!(status, 204, "author delete");
    let (status, _) = req("GET", &format!("/api/articles/{slug}"), None, None);
    assert_eq!(status, 404, "deleted article gone");
}
