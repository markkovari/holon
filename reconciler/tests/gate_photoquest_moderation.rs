//! `photoquest` moderation — CONTRACT.md "Roles" and "Moderation", judged end to end.
//!
//! - The first admin is the account whose email is `bootstrap-admin-email`; no
//!   request body can ask for a role.
//! - A role an admin grants or revokes reaches the grantee's EXISTING session on its
//!   next request: `auth-guard` re-resolves roles from the RBAC store on every
//!   introspect, so no re-login is needed (asserted below with the old token).
//! - A suspended account can still log in and list its photos, and cannot upload
//!   or report.
//! - A photo is visible to someone other than its owner iff it is not hidden and is
//!   entered in a published competition. Reporting a photo you cannot see is a 404,
//!   the same answer as a photo that does not exist.
//!
//! The report lifecycle needs a photo somebody else can see, which only
//! `competitions.rs` can arrange. While that module still answers 501 the
//! assertions that depend on it are skipped LOUDLY (see `competition_entry`), and
//! everything else still runs.

mod gatelib;
mod photoquest_support;
use gatelib::{field, Gate};
use photoquest_support::*;
use serde_json::{json, Value};

/// Plan, complete and evaluate one photo for `token`; its id. `sha` makes each
/// file distinct, so a competition's same-bytes rule never trips.
fn evaluated_photo(gate: &Gate, token: &str, sha: &str) -> String {
    let (code, out) = gate.post(
        "/api/photos",
        Some(token),
        json!({"filename": "DSC0001.ARW", "size": 1_000_000, "content_type": "image/x-sony-arw"}),
    );
    assert_eq!(code, 201, "upload plan: {out}");
    let id = parse(&out)["photo"]["id"].as_str().unwrap_or_default().to_string();
    let (code, out) = gate.post(
        &format!("/api/photos/{id}/complete"),
        Some(token),
        json!({"parts": [{"number": 1, "etag": "\"e1\""}]}),
    );
    assert_eq!(code, 200, "complete: {out}");
    let mut result = result_for(&id);
    result["sha256"] = json!(sha);
    let (code, out) = gate.with_headers(
        "POST",
        &format!("/internal/photos/{id}/evaluated"),
        None,
        &[("x-media-signature", &sign(&result.to_string(), SECRET))],
        Some(result),
    );
    assert_eq!(code, 200, "callback: {out}");
    id
}

/// A published competition, open now, created by `curator`, with `photo` entered
/// by `entrant`. `None` while `competitions.rs` cannot do that yet.
fn competition_entry(gate: &Gate, curator: &str, entrant: &str, photo: &str) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let (code, out) = gate.post(
        "/api/curator/competitions",
        Some(curator),
        json!({
            "title": "Moderation gate", "brief": "anything",
            "requirements": {"captured_after_start": false},
            "opens_at": now - 60, "closes_at": now + 3600,
            "voting_closes_at": now + 7200, "judging_closes_at": now + 10800,
            "weights": {"auto": 0.4, "votes": 0.3, "judges": 0.3},
            "max_entries_per_user": 5, "prizes_xp": [300, 200, 100], "journey": null
        }),
    );
    if code == 501 || code == 404 {
        eprintln!(
            "PENDING [photoquest moderation]: competitions.rs answers {code} to creating a \
             competition — the report lifecycle (it needs a photo somebody else can see) is skipped"
        );
        return None;
    }
    assert!(code == 200 || code == 201, "create competition: {code} {out}");
    let v = parse(&out);
    let cid = v["id"].as_str().or_else(|| v["competition"]["id"].as_str()).unwrap_or_default();
    assert!(!cid.is_empty(), "the competition has no id: {out}");
    let (code, out) =
        gate.post(&format!("/api/curator/competitions/{cid}/publish"), Some(curator), json!({}));
    assert!(code == 200 || code == 201, "publish competition: {code} {out}");
    let (code, out) = gate.post(
        &format!("/api/competitions/{cid}/entries"),
        Some(entrant),
        json!({"photo_id": photo}),
    );
    assert!(code == 200 || code == 201, "enter competition: {code} {out}");
    Some(cid.to_string())
}

fn roles_of(gate: &Gate, token: &str) -> Vec<String> {
    let (code, me) = gate.get("/me", Some(token));
    assert_eq!(code, 200, "/me: {me}");
    parse(&me)["roles"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn ids(list: &str, key: &str) -> Vec<String> {
    parse(list)[key]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["id"].as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn admins_moderate_photos_reports_accounts_and_roles() {
    let run = std::process::id();
    let admin_email = format!("root-{run}@photoquest.test");
    let bootstrap = format!("bootstrap-admin-email={admin_email}");
    let Some((gate, _media)) = start(&[bootstrap.as_str(), "allow-test-routes=true"]) else {
        return;
    };

    // --- accounts: the bootstrap admin, and nobody can ask for a role --------------
    let mut subjects = std::collections::HashMap::new();
    let mut account = |who: &str, email: &str, body_role: Option<&str>| -> String {
        let mut body = json!({"email": email, "password": "correct horse"});
        if let Some(r) = body_role {
            body["role"] = json!(r);
        }
        let (code, reg) = gate.post("/register", None, body);
        assert_eq!(code, 201, "register {who}: {reg}");
        subjects.insert(who.to_string(), field(&reg, "subject"));
        let (code, out) =
            gate.post("/login", None, json!({"email": email, "password": "correct horse"}));
        assert_eq!(code, 200, "login {who}: {out}");
        field(&out, "access_token")
    };
    let root = account("root", &admin_email, Some("curator"));
    let ada = account("ada", &format!("ada-{run}@photoquest.test"), Some("admin"));
    let bob = account("bob", &format!("bob-{run}@photoquest.test"), None);
    let cat = account("cat", &format!("cat-{run}@photoquest.test"), None);
    let sub = |who: &str| subjects[who].clone();

    let r = roles_of(&gate, &root);
    assert!(r.contains(&"admin".into()), "the bootstrap email must register as admin: {r:?}");
    assert!(!r.contains(&"curator".into()), "a role in the body was honoured: {r:?}");
    let r = roles_of(&gate, &ada);
    assert!(!r.contains(&"admin".into()), "register handed out admin on request: {r:?}");
    assert!(r.contains(&"photographer".into()), "everyone is a photographer: {r:?}");

    // --- only admins reach /api/admin ----------------------------------------------
    gatelib::assert_unauthenticated(&gate, "GET", "/api/admin/users", None);
    for (method, path, body) in [
        ("GET", "/api/admin/users".to_string(), None),
        ("GET", "/api/admin/reports".to_string(), None),
        ("POST", format!("/api/admin/users/{}/roles", sub("ada")), Some(json!({"grant": "admin"}))),
        ("POST", format!("/api/admin/users/{}/suspend", sub("bob")), Some(json!({"reason": "x"}))),
    ] {
        let (code, out) = gate.json(method, &path, Some(&ada), body);
        assert_eq!(code, 403, "a photographer reached {method} {path}: {out}");
        assert_eq!(field(&out, "error"), "forbidden_role", "{out}");
    }

    let (code, users) = gate.get("/api/admin/users", Some(&root));
    assert_eq!(code, 200, "{users}");
    let users = parse(&users)["users"].clone();
    let ada_row = users
        .as_array()
        .and_then(|a| a.iter().find(|u| u["subject"] == sub("ada")))
        .cloned()
        .unwrap_or_else(|| panic!("ada is not in the user list: {users}"));
    assert_eq!(ada_row["email"], format!("ada-{run}@photoquest.test"), "{ada_row}");
    assert_eq!(ada_row["suspended"], false, "{ada_row}");
    assert!(ada_row["roles"].as_array().is_some(), "{ada_row}");

    // --- roles: grants reach the grantee's existing session -------------------------
    let roles_path = |who: &str| format!("/api/admin/users/{}/roles", sub(who));
    let (code, out) = gate.post(&roles_path("ada"), Some(&root), json!({"grant": "curator"}));
    assert_eq!(code, 200, "{out}");
    assert!(
        roles_of(&gate, &ada).contains(&"curator".into()),
        "a grant must reach ada's EXISTING token (auth-guard re-resolves roles per request)"
    );
    let (code, _) = gate.post(&roles_path("ada"), Some(&root), json!({"revoke": "curator"}));
    assert_eq!(code, 200);
    assert!(!roles_of(&gate, &ada).contains(&"curator".into()), "a revoke must reach it too");
    let (code, _) = gate.post(&roles_path("ada"), Some(&root), json!({"grant": "curator"}));
    assert_eq!(code, 200);

    let (code, out) = gate.post(&roles_path("ada"), Some(&root), json!({"grant": "photographer"}));
    assert_eq!(code, 400, "only curator/admin can be granted: {out}");
    let (code, out) = gate.post(
        &format!("/api/admin/users/usr_nobody{run}/roles"),
        Some(&root),
        json!({"grant": "curator"}),
    );
    assert_eq!(code, 404, "a role for an account that does not exist: {out}");

    let (code, out) = gate.post(&roles_path("root"), Some(&root), json!({"revoke": "admin"}));
    assert_eq!(code, 409, "an admin revoked their own admin: {out}");
    assert_eq!(field(&out, "error"), "last_word", "{out}");
    assert!(roles_of(&gate, &root).contains(&"admin".into()));
    // Another admin's is fine.
    let (code, _) = gate.post(&roles_path("bob"), Some(&root), json!({"grant": "admin"}));
    assert_eq!(code, 200);
    assert!(roles_of(&gate, &bob).contains(&"admin".into()));
    let (code, _) = gate.post(&roles_path("bob"), Some(&root), json!({"revoke": "admin"}));
    assert_eq!(code, 200);
    assert!(!roles_of(&gate, &bob).contains(&"admin".into()));

    // --- suspension --------------------------------------------------------------------
    let cats_photo = evaluated_photo(&gate, &cat, &"c1".repeat(32));
    let suspend = |who: &str| format!("/api/admin/users/{}/suspend", sub(who));
    let (code, out) = gate.post(&suspend("cat"), Some(&root), json!({}));
    assert_eq!(code, 400, "a suspension needs a reason: {out}");
    let (code, out) = gate.post(
        &format!("/api/admin/users/usr_nobody{run}/suspend"),
        Some(&root),
        json!({"reason": "x"}),
    );
    assert_eq!(code, 404, "{out}");
    let (code, out) = gate.post(&suspend("cat"), Some(&root), json!({"reason": "spamming"}));
    assert_eq!(code, 200, "{out}");

    let (code, out) = gate.post(
        "/api/photos",
        Some(&cat),
        json!({"filename": "b.ARW", "size": 1_000, "content_type": "image/x-sony-arw"}),
    );
    assert_eq!(code, 403, "a suspended account uploaded: {out}");
    assert_eq!(field(&out, "error"), "suspended", "{out}");
    let (code, out) = gate.post(
        &format!("/api/photos/{cats_photo}/complete"),
        Some(&cat),
        json!({"parts": [{"number": 1, "etag": "\"e1\""}]}),
    );
    assert_eq!(code, 403, "a suspended account completed an upload: {out}");
    let (code, list) = gate.get("/api/photos", Some(&cat));
    assert_eq!(code, 200, "a suspended account must still see its own photos: {list}");
    assert_eq!(ids(&list, "photos"), vec![cats_photo.clone()], "{list}");
    let (code, out) = gate.post(
        "/login",
        None,
        json!({"email": format!("cat-{run}@photoquest.test"), "password": "correct horse"}),
    );
    assert_eq!(code, 200, "a suspended account can still log in: {out}");

    let (_, users) = gate.get("/api/admin/users", Some(&root));
    let cat_row = parse(&users)["users"]
        .as_array()
        .and_then(|a| a.iter().find(|u| u["subject"] == sub("cat")).cloned())
        .unwrap_or_default();
    assert_eq!(cat_row["suspended"], true, "the user list must show the suspension: {cat_row}");

    let (code, _) =
        gate.post(&format!("/api/admin/users/{}/unsuspend", sub("cat")), Some(&root), json!({}));
    assert_eq!(code, 200);
    let (code, out) = gate.post(
        "/api/photos",
        Some(&cat),
        json!({"filename": "b.ARW", "size": 1_000, "content_type": "image/x-sony-arw"}),
    );
    assert_eq!(code, 201, "unsuspending must restore uploads: {out}");

    // --- reports: own photo, and a photo nobody else can see ---------------------------
    let bobs = evaluated_photo(&gate, &bob, &"b1".repeat(32));
    let report = |token: &str, reason: &str| {
        gate.post(&format!("/api/photos/{bobs}/reports"), Some(token), json!({"reason": reason}))
    };
    let (code, out) = report(&bob, "spam");
    assert_eq!(code, 400, "reporting your own photo: {out}");
    assert_eq!(field(&out, "error"), "own_photo", "{out}");
    let (code, out) = report(&ada, "spam");
    assert_eq!(code, 404, "a photo in no published competition is not visible to ada: {out}");
    let (code, out) =
        gate.post(&format!("/api/photos/NOPE{run}/reports"), Some(&ada), json!({"reason": "spam"}));
    assert_eq!(code, 404, "{out}");

    // --- reports lifecycle (needs competitions.rs) --------------------------------------
    let entered = competition_entry(&gate, &ada, &bob, &bobs);
    let mut open_reports = Vec::new();
    if entered.is_some() {
        let (code, out) = report(&ada, "nonsense");
        assert_eq!(code, 400, "an unknown reason: {out}");
        let (code, out) = report(&ada, "spam");
        assert_eq!(code, 201, "ada reports a photo on a published competition: {out}");
        assert_eq!(field(&out, "state"), "open", "{out}");
        let adas = field(&out, "id");
        let (code, out) = report(&ada, "stolen");
        assert_eq!(code, 409, "a second open report by the same reporter: {out}");
        assert_eq!(field(&out, "error"), "already_reported", "{out}");

        // Suspended reporters are refused.
        let (code, _) = gate.post(&suspend("cat"), Some(&root), json!({"reason": "again"}));
        assert_eq!(code, 200);
        let (code, out) = report(&cat, "inappropriate");
        assert_eq!(code, 403, "a suspended account reported: {out}");
        assert_eq!(field(&out, "error"), "suspended", "{out}");
        let (code, _) = gate.post(
            &format!("/api/admin/users/{}/unsuspend", sub("cat")),
            Some(&root),
            json!({}),
        );
        assert_eq!(code, 200);
        let (code, out) = report(&cat, "inappropriate");
        assert_eq!(code, 201, "{out}");
        let cats = field(&out, "id");

        let (code, list) = gate.get("/api/admin/reports?state=open", Some(&root));
        assert_eq!(code, 200, "{list}");
        let open = ids(&list, "reports");
        assert!(open.contains(&adas) && open.contains(&cats), "both reports are open: {list}");
        let (code, _) = gate.get("/api/admin/reports?state=bogus", Some(&root));
        assert_eq!(code, 400);

        // open -> dismissed
        let (code, out) = gate.post(
            &format!("/api/admin/reports/{adas}/dismiss"),
            Some(&root),
            json!({"note": "fine"}),
        );
        assert_eq!(code, 200, "{out}");
        assert_eq!(field(&out, "state"), "dismissed", "{out}");
        let (code, out) =
            gate.post(&format!("/api/admin/reports/{adas}/dismiss"), Some(&root), json!({}));
        assert_eq!(code, 409, "a dismissed report dismissed again: {out}");
        let (_, list) = gate.get("/api/admin/reports?state=dismissed", Some(&root));
        assert!(ids(&list, "reports").contains(&adas), "{list}");
        // With nothing open, ada may report again.
        let (code, out) = report(&ada, "stolen");
        assert_eq!(code, 201, "{out}");
        open_reports = vec![cats, field(&out, "id")];
    }

    // --- hide / unhide ----------------------------------------------------------------
    let hide = format!("/api/admin/photos/{bobs}/hide");
    let (code, out) = gate.post(&hide, Some(&root), json!({}));
    assert_eq!(code, 400, "hiding needs a reason: {out}");
    let (code, out) = gate.post(&hide, Some(&root), json!({"reason": "stolen image"}));
    assert_eq!(code, 200, "{out}");

    let (code, got) = gate.get(&format!("/api/photos/{bobs}"), Some(&bob));
    assert_eq!(code, 200, "the owner still sees a hidden photo: {got}");
    let got = parse(&got);
    assert_eq!(got["moderation"]["hidden"], true, "{got}");
    assert_eq!(got["moderation"]["reason"], "stolen image", "{got}");
    assert!(got["moderation"]["at"].as_u64().is_some(), "{got}");
    assert!(got["moderation"]["by"].is_null(), "the owner is not told which admin: {got}");
    assert!(got["urls"]["share"].as_str().is_some(), "the owner keeps a signed link: {got}");
    let (_, list) = gate.get("/api/photos", Some(&bob));
    let row = parse(&list)["photos"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["id"] == bobs.as_str()).cloned())
        .unwrap_or_default();
    assert_eq!(row["moderation"]["hidden"], true, "the owner's gallery shows it: {row}");

    let (code, got) = gate.get(&format!("/api/photos/{bobs}"), Some(&root));
    assert_eq!(code, 200, "{got}");
    let got = parse(&got);
    assert!(
        got["urls"]["share"].is_null(),
        "a hidden photo's share URL was signed for a non-owner: {got}"
    );
    let (code, _) = gate.get(&format!("/api/photos/{bobs}"), Some(&ada));
    assert_eq!(code, 403);

    if entered.is_some() {
        // open -> actioned, on hide
        let (_, list) = gate.get("/api/admin/reports?state=actioned", Some(&root));
        let actioned = ids(&list, "reports");
        for r in &open_reports {
            assert!(actioned.contains(r), "hide must action every open report ({r}): {list}");
        }
        let (_, list) = gate.get("/api/admin/reports?state=open", Some(&root));
        assert!(
            open_reports.iter().all(|r| !ids(&list, "reports").contains(r)),
            "an actioned report is still open: {list}"
        );
        let (code, out) = report(&cat, "spam");
        assert_eq!(code, 404, "a hidden photo is not visible, so not reportable: {out}");
    }

    let (code, out) =
        gate.post(&format!("/api/admin/photos/{bobs}/unhide"), Some(&root), json!({}));
    assert_eq!(code, 200, "{out}");
    let (_, got) = gate.get(&format!("/api/photos/{bobs}"), Some(&bob));
    assert_ne!(parse(&got)["moderation"]["hidden"], true, "still hidden after unhide: {got}");
    let (_, got) = gate.get(&format!("/api/photos/{bobs}"), Some(&root));
    assert!(parse(&got)["urls"]["share"].as_str().is_some(), "unhide restores signing: {got}");
    let (code, _) = gate.post(
        &format!("/api/admin/photos/NOPE{run}/hide"),
        Some(&root),
        json!({"reason": "x"}),
    );
    assert_eq!(code, 404);
}
