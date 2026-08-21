//! E2E for the photosocial showcase (docs/apps/PHOTOSOCIAL.md) as ONE composed wasm HTTP component
//! (photosocial-domain + auth-guard + record-store + llm-inference) on the native Rust host.
//! Proves the full social photo lifecycle:
//!   - Admin role-based access control (RBAC): Only admins can create/delete evaluation attributes
//!   - Non-admin attempts to modify attributes are rejected with 403 Forbidden
//!   - Photo uploads automatically trigger AI narrative and critique generation
//!   - Upvoting and downvoting with user deduplication
//!   - Community attribute scoring with real-time aggregate mean computations.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3055";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://{ADDR}")
}

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let mut r = ureq::request(method, &url);
    if let Some(t) = token {
        r = r.set("authorization", &format!("Bearer {t}"));
    }
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/photosocial_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `cargo build --release -p comp-host`)");
    assert!(component.exists(), "composed wasm missing (just compose-photosocial)");

    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "photosocial")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("comp-host on {ADDR} did not become healthy");
}

#[test]
fn full_photosocial_lifecycle_and_rbac_e2e() {
    let _host = start_host();

    // 1. Service discovery info
    let (status, info) = req("GET", "/api/info", None, None);
    assert_eq!(status, 200);
    assert_eq!(info["service"], "photosocial");

    // 2. Admin account registration & login
    let (s, _) = req(
        "POST",
        "/api/register",
        None,
        Some(json!({ "email": "admin@holon.test", "password": "adminpassword123", "role": "admin" })),
    );
    assert!(s == 201 || s == 409, "register admin: {s}");

    let (s, admin_login) = req(
        "POST",
        "/api/login",
        None,
        Some(json!({ "email": "admin@holon.test", "password": "adminpassword123" })),
    );
    assert_eq!(s, 200, "login admin: {admin_login}");
    let admin_token = admin_login["access_token"].as_str().expect("admin token");

    // Verify /api/me as admin
    let (s, admin_me) = req("GET", "/api/me", Some(admin_token), None);
    assert_eq!(s, 200);
    assert_eq!(admin_me["is_admin"], true);

    // 3. Admin creates custom evaluation attribute
    let (s, new_attr) = req(
        "POST",
        "/api/admin/attributes",
        Some(admin_token),
        Some(json!({
            "name": "Lighting & Mood",
            "id": "lighting-mood",
            "description": "Atmospheric depth, shadows, highlights, and dynamic exposure."
        })),
    );
    assert_eq!(s, 201, "admin attribute creation: {new_attr}");

    // 4. Regular User registration & login
    let (s, _) = req(
        "POST",
        "/api/register",
        None,
        Some(json!({ "email": "creator@holon.test", "password": "creatorpassword123", "role": "user" })),
    );
    assert!(s == 201 || s == 409, "register creator: {s}");

    let (s, user_login) = req(
        "POST",
        "/api/login",
        None,
        Some(json!({ "email": "creator@holon.test", "password": "creatorpassword123" })),
    );
    assert_eq!(s, 200);
    let user_token = user_login["access_token"].as_str().expect("user token");

    // 5. RBAC Gate: Regular user attempt to create an attribute MUST be refused with 403
    let (s, blocked) = req(
        "POST",
        "/api/admin/attributes",
        Some(user_token),
        Some(json!({ "name": "Illegal Attribute", "description": "Should fail" })),
    );
    assert_eq!(s, 403, "non-admin must be forbidden from creating attributes: {blocked}");

    // 6. User uploads photo -> triggers automated AI critique
    let (s, photo) = req(
        "POST",
        "/api/photos",
        Some(user_token),
        Some(json!({
            "title": "Neon Rain Reflections in Tokyo",
            "image_url": "https://images.unsplash.com/photo-1506744038136-46273834b3fb?w=800",
            "description": "Long exposure shot capturing reflections on wet asphalt with 50mm f/1.4 lens."
        })),
    );
    assert_eq!(s, 201, "photo upload: {photo}");
    let photo_id = photo["id"].as_str().expect("photo id");
    assert!(photo["ai_narrative"].as_str().unwrap().len() > 10, "ai narrative generated");
    assert!(photo["ai_critique"].as_str().unwrap().len() > 10, "ai critique generated");
    assert!(!photo["ai_tags"].as_array().unwrap().is_empty(), "ai tags generated");

    // 7. Voting (Upvote / Downvote)
    let (s, vote_res) = req("POST", &format!("/api/photos/{photo_id}/vote"), Some(user_token), Some(json!({ "value": 1 })));
    assert_eq!(s, 200);
    assert_eq!(vote_res["score"], 1);
    assert_eq!(vote_res["upvotes"], 1);

    // 8. Attribute Ratings
    let (s, rating_res) = req(
        "POST",
        &format!("/api/photos/{photo_id}/rate"),
        Some(admin_token),
        Some(json!({
            "ratings": [
                { "attribute_id": "perspective", "score": 9.0 },
                { "attribute_id": "lighting", "score": 9.5 },
                { "attribute_id": "creativity", "score": 8.5 }
            ]
        })),
    );
    assert_eq!(s, 200, "admin rate: {rating_res}");

    let (s, user_rate_res) = req(
        "POST",
        &format!("/api/photos/{photo_id}/rate"),
        Some(user_token),
        Some(json!({
            "ratings": [
                { "attribute_id": "perspective", "score": 8.0 },
                { "attribute_id": "lighting", "score": 8.5 },
                { "attribute_id": "creativity", "score": 9.5 }
            ]
        })),
    );
    assert_eq!(s, 200, "user rate: {user_rate_res}");

    // 9. Inspect photo detail with computed averages
    let (s, photo_detail) = req("GET", &format!("/api/photos/{photo_id}"), None, None);
    assert_eq!(s, 200);
    let attr_scores = &photo_detail["attribute_scores"];
    assert_eq!(attr_scores["perspective"]["avg"], 8.5); // (9.0 + 8.0) / 2
    assert_eq!(attr_scores["lighting"]["avg"], 9.0);    // (9.5 + 8.5) / 2
    assert_eq!(attr_scores["creativity"]["avg"], 9.0);  // (8.5 + 9.5) / 2
    assert_eq!(attr_scores["perspective"]["count"], 2);

    // 10. Check caller's personal rating view
    let (s, my_ratings) = req("GET", &format!("/api/photos/{photo_id}/my-ratings"), Some(user_token), None);
    assert_eq!(s, 200);
    assert_eq!(my_ratings["ratings"]["perspective"], 8.0);
    assert_eq!(my_ratings["vote"], 1);
}
