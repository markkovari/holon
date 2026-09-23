//! `photoquest` quests — a curator builds a journey of two quests, a photographer
//! submits evaluated photos to them, and every verdict, XP credit, level-up, unlock
//! and badge is what CONTRACT.md "The game" says.
//!
//! Photos go through the real flow (plan, complete, signed callback) against the
//! fake `comp-media` in `photoquest_support`; the callback body is crafted per photo
//! (labels, focus, metadata, format, Vision on/off, sha256) so each requirement is
//! exercised on purpose. Time windows are tested with `POST /test/clock`
//! (config `allow-test-routes=true`).
//!
//! The curator role is granted the way the contract says: config
//! `bootstrap-admin-email` makes that account admin at register, and the admin
//! `POST /api/admin/users/{id}/roles {grant: "curator"}` (moderation.rs).

mod gatelib;
mod photoquest_support;
use gatelib::{field, Gate};
use photoquest_support::*;
use serde_json::{json, Value};

const PASSWORD: &str = "correct horse";

/// A photo taken through the whole pipeline to `evaluated`, with the callback body
/// shaped by `tweak`. Returns the photo id.
fn evaluated_photo(
    gate: &Gate,
    token: &str,
    filename: &str,
    content_type: &str,
    tweak: impl Fn(&mut Value),
) -> String {
    let (code, out) = gate.post(
        "/api/photos",
        Some(token),
        json!({"filename": filename, "size": 1024, "content_type": content_type}),
    );
    assert_eq!(code, 201, "create photo: {out}");
    let id = parse(&out)["photo"]["id"].as_str().unwrap_or_default().to_string();
    let (code, out) = gate.post(
        &format!("/api/photos/{id}/complete"),
        Some(token),
        json!({"parts": [{"number": 1, "etag": "\"e1\""}]}),
    );
    assert_eq!(code, 200, "complete photo: {out}");
    let mut result = result_for(&id);
    tweak(&mut result);
    let raw = result.to_string();
    let (code, out) = gate.with_headers(
        "POST",
        &format!("/internal/photos/{id}/evaluated"),
        None,
        &[("x-media-signature", &sign(&raw, SECRET))],
        Some(result),
    );
    assert_eq!(code, 200, "evaluation callback: {out}");
    id
}

/// `YYYY-MM-DDTHH:MM:SS` — the camera clock, no zone, as `captured_at` carries it.
fn camera_clock(secs: u64) -> String {
    gatelib::rfc3339(secs).trim_end_matches('Z').to_string()
}

/// The shape of one photo's evaluation.
struct Shot {
    sha: String,
    labels: Value,
    focus: f64,
    fnumber: f64,
    captured_at: Option<String>,
    vision: bool,
}

impl Shot {
    fn tweak(&self) -> impl Fn(&mut Value) + '_ {
        move |r: &mut Value| {
            r["sha256"] = json!(self.sha);
            r["sharpness"]["focus_ratio"] = json!(self.focus);
            r["vision"]["labels"] = self.labels.clone();
            r["metadata"]["fnumber"] = json!(self.fnumber);
            match &self.captured_at {
                Some(at) => r["metadata"]["captured_at"] = json!(at),
                None => {
                    r["metadata"].as_object_mut().expect("metadata").remove("captured_at");
                }
            }
            if !self.vision {
                r["backend"]["vision"] = json!(false);
                r["vision"] = Value::Null;
            }
        }
    }
}

fn set_clock(gate: &Gate, offset: i64) -> u64 {
    let (code, out) = gate.post("/test/clock", None, json!({"offset_secs": offset}));
    assert_eq!(code, 200, "POST /test/clock must work with allow-test-routes=true: {out}");
    parse(&out)["now"].as_u64().expect("clock answers now")
}

fn error_of(out: &str) -> String {
    field(out, "error")
}

/// One check out of a verdict, by name.
fn check<'a>(verdict: &'a Value, name: &str) -> &'a Value {
    verdict["checks"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["name"] == name))
        .unwrap_or_else(|| panic!("the verdict has no {name:?} check: {verdict}"))
}

#[test]
fn a_journey_of_quests_is_built_submitted_to_and_levelled_through() {
    let run = std::process::id();
    let admin_email = format!("root-{run}@photoquest.test");
    let bootstrap = format!("bootstrap-admin-email={admin_email}");
    let Some((gate, _media)) = start(&["allow-test-routes=true", bootstrap.as_str()]) else {
        return;
    };

    // --- accounts ---------------------------------------------------------------
    let register = |who: &str| -> (String, String) {
        let email = if who == "root" {
            admin_email.clone()
        } else {
            format!("{who}-{run}@photoquest.test")
        };
        let (code, reg) =
            gate.post("/register", None, json!({"email": email, "password": PASSWORD}));
        assert_eq!(code, 201, "register {who}: {reg}");
        (email, field(&reg, "subject"))
    };
    let login = |email: &str| -> String {
        let (code, out) = gate.post("/login", None, json!({"email": email, "password": PASSWORD}));
        assert_eq!(code, 200, "login {email}: {out}");
        field(&out, "access_token")
    };
    let (root_email, _) = register("root");
    let (cora_email, cora_sub) = register("cora");
    let (pat_email, _) = register("pat");
    let (sam_email, _) = register("sam");
    let (root, pat, sam) = (login(&root_email), login(&pat_email), login(&sam_email));

    gatelib::assert_unauthenticated(&gate, "GET", "/api/journeys", None);
    gatelib::assert_unauthenticated(&gate, "GET", "/api/curator/journeys", None);

    // --- curator routes need the curator role -------------------------------------
    let journey_body = json!({
        "title": "Into the park", "description": "Green things, sharp.",
        "levels": [{"level": 1, "xp": 0}, {"level": 2, "xp": 100}, {"level": 3, "xp": 250}],
        "badge": {"name": "Park ranger"},
    });
    for (who, token) in [("a photographer", &pat), ("an admin who is not a curator", &root)] {
        let (code, out) = gate.post("/api/curator/journeys", Some(token), journey_body.clone());
        assert_eq!(
            (code, error_of(&out)),
            (403, "forbidden_role".into()),
            "{who} must not create a journey: {out}"
        );
        let (code, out) = gate.get("/api/curator/journeys", Some(token));
        assert_eq!(
            (code, error_of(&out)),
            (403, "forbidden_role".into()),
            "{who} listed journeys: {out}"
        );
    }

    let (code, out) = gate.post(
        &format!("/api/admin/users/{cora_sub}/roles"),
        Some(&root),
        json!({"grant": "curator"}),
    );
    assert_eq!(code, 200, "the bootstrap admin grants curator (needs moderation.rs): {out}");
    let cora = login(&cora_email);

    // --- the curator builds a journey ---------------------------------------------
    for bad in [
        json!([{"level": 1, "xp": 10}]),
        json!([{"level": 1, "xp": 0}, {"level": 2, "xp": 100}, {"level": 3, "xp": 50}]),
        json!([]),
    ] {
        let mut b = journey_body.clone();
        b["levels"] = bad.clone();
        let (code, out) = gate.post("/api/curator/journeys", Some(&cora), b);
        assert_eq!(
            (code, error_of(&out)),
            (400, "bad_levels".into()),
            "levels {bad} accepted: {out}"
        );
    }
    let (code, out) = gate.post("/api/curator/journeys", Some(&cora), journey_body.clone());
    assert_eq!(code, 201, "create journey: {out}");
    let journey = parse(&out);
    let jid = journey["id"].as_str().unwrap_or_default().to_string();
    assert_eq!(journey["state"], "draft", "a new journey is a draft: {out}");
    assert_eq!(journey["created_by"], cora_sub, "{out}");

    let now = set_clock(&gate, 0);
    let starts_at = now - 7200;
    let q1_body = json!({
        "journey": jid, "title": "Something green", "description": "Grass, in focus.",
        "xp": 120, "starts_at": starts_at, "ends_at": now + 7 * 86_400,
        "requirements": {"subject": {"label": "grass", "min_confidence": 0.5},
                         "sharpness": {"min_focus_ratio": 5.0}},
    });
    let mut bad = q1_body.clone();
    bad["requirements"] = json!({"subject": {"label": "grass", "min_confidence": 2}});
    let (code, out) = gate.post("/api/curator/quests", Some(&cora), bad);
    assert_eq!((code, error_of(&out)), (400, "bad_requirements".into()), "{out}");
    assert!(!field(&out, "detail").is_empty(), "bad_requirements must say what is wrong: {out}");

    let (code, out) = gate.post("/api/curator/quests", Some(&cora), q1_body.clone());
    assert_eq!(code, 201, "create quest 1: {out}");
    let q1 = field(&out, "id");
    let (code, out) = gate.post(
        "/api/curator/quests",
        Some(&cora),
        json!({
            "journey": jid, "title": "Wide open, in RAW", "xp": 150,
            "starts_at": starts_at, "ends_at": null,
            "requirements": {"format": "raw", "exposure": {"max_fnumber": 1.4}},
        }),
    );
    assert_eq!(code, 201, "create quest 2: {out}");
    let q2 = field(&out, "id");

    let (code, out) = gate.get(&format!("/api/curator/journeys/{jid}"), Some(&cora));
    assert_eq!(code, 200, "{out}");
    assert_eq!(parse(&out)["quests"], json!([q1, q2]), "quests are appended in order: {out}");

    // Nothing is visible to a photographer before it is published.
    let (_, out) = gate.get("/api/journeys", Some(&pat));
    assert!(
        !parse(&out)["journeys"].as_array().into_iter().flatten().any(|j| j["id"] == jid),
        "a draft journey is visible: {out}"
    );
    let (code, _) = gate.get(&format!("/api/quests/{q1}"), Some(&pat));
    assert_eq!(code, 404, "a draft quest is visible");

    for q in [&q1, &q2] {
        let (code, out) =
            gate.post(&format!("/api/curator/quests/{q}/publish"), Some(&cora), json!({}));
        assert_eq!(code, 200, "publish quest: {out}");
    }
    // A published quest in a draft journey is still not visible.
    let (code, _) = gate.get(&format!("/api/quests/{q1}"), Some(&pat));
    assert_eq!(code, 404, "a quest of a draft journey is visible");
    let (code, out) =
        gate.post(&format!("/api/curator/journeys/{jid}/publish"), Some(&cora), json!({}));
    assert_eq!(code, 200, "publish journey: {out}");
    assert_eq!(parse(&out)["quests"], json!([q1, q2]), "publishing keeps the order: {out}");

    let (code, out) = gate.get("/api/curator/quests", Some(&cora));
    assert_eq!(code, 200, "{out}");
    assert!(parse(&out)["quests"].as_array().map(Vec::len).unwrap_or(0) >= 2, "{out}");

    // --- published requirements are fixed --------------------------------------------
    let (code, out) = gate.json(
        "PUT",
        &format!("/api/curator/quests/{q1}"),
        Some(&cora),
        Some(json!({"requirements": {"subject": {"label": "grass", "min_confidence": 0.1}}})),
    );
    assert_eq!((code, error_of(&out)), (409, "quest_published".into()), "{out}");
    let (code, out) = gate.json(
        "PUT",
        &format!("/api/curator/quests/{q1}"),
        Some(&cora),
        Some(json!({"title": "Something green!"})),
    );
    assert_eq!(code, 200, "the title of a published quest stays editable: {out}");
    assert_eq!(parse(&out)["requirements"], q1_body["requirements"], "{out}");
    let (code, out) = gate.json(
        "PUT",
        &format!("/api/curator/quests/{q1}"),
        Some(&pat),
        Some(json!({"title": "mine"})),
    );
    assert_eq!((code, error_of(&out)), (403, "forbidden_role".into()), "{out}");

    // --- the photographer sees the journey --------------------------------------------
    let (code, out) = gate.get(&format!("/api/journeys/{jid}"), Some(&pat));
    assert_eq!(code, 200, "{out}");
    let j = parse(&out);
    assert_eq!(j["progress"]["xp"], 0, "{out}");
    assert_eq!(j["progress"]["level"], 1, "{out}");
    assert_eq!(j["progress"]["next_level_xp"], 100, "{out}");
    assert_eq!(j["quests"][0]["id"], q1, "{out}");
    assert_eq!(j["quests"][0]["state"], "open", "quest 1 is open: {out}");
    assert_eq!(
        j["quests"][1]["state"], "locked",
        "quest 2 is locked until quest 1 is passed: {out}"
    );
    let (_, out) = gate.get("/api/journeys", Some(&pat));
    assert!(
        parse(&out)["journeys"].as_array().into_iter().flatten().any(|x| x["id"] == jid),
        "a published journey is listed: {out}"
    );

    // --- photos -------------------------------------------------------------------------
    let after = Some(camera_clock(now - 3600));
    let grass = json!([{"id": "grass", "confidence": 0.9}]);
    let shot =
        |sha: &str, labels: &Value, focus: f64, fnumber: f64, at: Option<String>, vision: bool| {
            Shot {
                sha: sha.repeat(32),
                labels: labels.clone(),
                focus,
                fnumber,
                captured_at: at,
                vision,
            }
        };
    let jpg = |token: &str, s: &Shot| {
        evaluated_photo(&gate, token, "IMG_0001.JPG", "image/jpeg", s.tweak())
    };
    let arw = |token: &str, s: &Shot| {
        evaluated_photo(&gate, token, "DSC0001.ARW", "image/x-sony-arw", s.tweak())
    };
    let submit = |token: &str, quest: &str, photo: &str| -> (u16, Value, String) {
        let (code, out) = gate.post(
            &format!("/api/quests/{quest}/submissions"),
            Some(token),
            json!({"photo_id": photo}),
        );
        (code, parse(&out), out)
    };

    let people = json!([{"id": "people", "confidence": 0.96}]);
    let p_fail = jpg(&pat, &shot("01", &people, 3.1, 4.0, after.clone(), true));
    let p_novision = jpg(&pat, &shot("02", &grass, 6.1, 4.0, after.clone(), false));
    let p_pass1 = jpg(&pat, &shot("03", &grass, 6.1, 4.0, after.clone(), true));
    let p_dup = jpg(&pat, &shot("03", &grass, 6.1, 4.0, after.clone(), true));
    let p_sam = jpg(&sam, &shot("03", &grass, 6.1, 4.0, after.clone(), true));

    // Refusals before judging.
    let (code, _, out) = submit(&sam, &q1, &p_pass1);
    assert_eq!(
        (code, error_of(&out)),
        (403, "forbidden".into()),
        "sam submitted pat's photo: {out}"
    );
    let (code, out) = gate.post(
        "/api/photos",
        Some(&pat),
        json!({"filename": "wip.jpg", "size": 1024, "content_type": "image/jpeg"}),
    );
    assert_eq!(code, 201, "{out}");
    let p_uploading = parse(&out)["photo"]["id"].as_str().unwrap_or_default().to_string();
    let (code, _, out) = submit(&pat, &q1, &p_uploading);
    assert_eq!((code, error_of(&out)), (409, "not_evaluated".into()), "{out}");
    let (code, _, out) = submit(&pat, &q2, &p_pass1);
    assert_eq!((code, error_of(&out)), (409, "quest_locked".into()), "quest 2 is locked: {out}");

    // The window, by the test clock.
    set_clock(&gate, -3 * 3600);
    let (code, _, out) = submit(&pat, &q1, &p_pass1);
    assert_eq!((code, error_of(&out)), (409, "quest_not_started".into()), "{out}");
    set_clock(&gate, 8 * 86_400);
    let (code, _, out) = submit(&pat, &q1, &p_pass1);
    assert_eq!((code, error_of(&out)), (409, "quest_ended".into()), "{out}");
    set_clock(&gate, 0);

    // A failing verdict names every check and why.
    let (code, v, out) = submit(&pat, &q1, &p_fail);
    assert_eq!(code, 201, "a failing submission is still stored: {out}");
    assert_eq!(v["pass"], false, "{out}");
    assert_eq!(v["verdict"]["pass"], false, "{out}");
    assert_eq!(check(&v["verdict"], "subject")["ok"], false, "{out}");
    assert_eq!(check(&v["verdict"], "subject")["detail"], "no grass label", "{out}");
    assert_eq!(check(&v["verdict"], "sharpness.min_focus_ratio")["ok"], false, "{out}");
    assert_eq!(check(&v["verdict"], "sharpness.min_focus_ratio")["detail"], "3.1 < 5.0", "{out}");
    assert_eq!(check(&v["verdict"], "captured_after_start")["ok"], true, "{out}");
    assert_eq!(v["xp_awarded"], 0, "{out}");

    // No Vision: not looked at, not "bad".
    let (code, v, out) = submit(&pat, &q1, &p_novision);
    assert_eq!(code, 201, "{out}");
    assert_eq!(v["pass"], false, "{out}");
    assert!(check(&v["verdict"], "subject")["ok"].is_null(), "no Vision must be ok:null: {out}");
    assert!(
        check(&v["verdict"], "subject")["detail"].as_str().unwrap_or_default().contains("Vision"),
        "the detail must say Vision did not run: {out}"
    );
    assert_eq!(check(&v["verdict"], "sharpness.min_focus_ratio")["ok"], true, "{out}");

    // A pass: 120 XP, level 1 → 2.
    let (code, v, out) = submit(&pat, &q1, &p_pass1);
    assert_eq!(code, 201, "{out}");
    assert_eq!(v["pass"], true, "{out}");
    assert_eq!(check(&v["verdict"], "subject")["detail"], "grass 0.90 ≥ 0.50", "{out}");
    assert_eq!(v["xp_awarded"], 120, "{out}");
    assert!(v["xp_reason"].is_null(), "{out}");
    assert_eq!(v["level_up"], json!({"journey": jid, "from": 1, "to": 2}), "{out}");
    assert!(v["badge"].is_null(), "one quest of two is no badge: {out}");

    // The same photo again: a verdict, no XP.
    let (code, v, out) = submit(&pat, &q1, &p_pass1);
    assert_eq!(code, 201, "{out}");
    assert_eq!(v["pass"], true, "{out}");
    assert_eq!(v["xp_awarded"], 0, "{out}");
    assert_eq!(v["xp_reason"], "already_rewarded", "{out}");
    assert!(v["level_up"].is_null(), "{out}");

    // A different photo, the same bytes: a verdict, no XP — for pat, and for anyone.
    for (who, token, photo) in [("pat", &pat, &p_dup), ("sam", &sam, &p_sam)] {
        let (code, v, out) = submit(token, &q1, photo);
        assert_eq!(code, 201, "{who}: {out}");
        assert_eq!(v["pass"], true, "{who}: {out}");
        assert_eq!(v["xp_awarded"], 0, "{who}: the same sha256 earned XP twice: {out}");
        assert_eq!(v["xp_reason"], "already_rewarded", "{who}: {out}");
    }

    let (code, out) = gate.get(&format!("/api/quests/{q1}/submissions"), Some(&pat));
    assert_eq!(code, 200, "{out}");
    let subs = parse(&out)["submissions"].as_array().cloned().unwrap_or_default();
    assert_eq!(subs.len(), 5, "pat's five submissions to quest 1: {out}");
    assert!(subs.iter().any(|x| x["photo"] == p_dup && x["xp_awarded"] == 0), "{out}");
    assert_eq!(
        subs.iter().filter(|x| x["xp_awarded"] == 120).count(),
        1,
        "one paid submission: {out}"
    );

    // Quest 2 unlocks.
    let (_, out) = gate.get(&format!("/api/journeys/{jid}"), Some(&pat));
    let j = parse(&out);
    assert_eq!(j["quests"][0]["state"], "passed", "{out}");
    assert_eq!(j["quests"][1]["state"], "open", "quest 2 unlocks: {out}");
    assert_eq!(j["progress"]["xp"], 120, "{out}");
    assert_eq!(j["progress"]["level"], 2, "{out}");
    assert_eq!(j["progress"]["next_level_xp"], 250, "{out}");
    let (code, out) = gate.get(&format!("/api/quests/{q2}"), Some(&pat));
    assert_eq!((code, field(&out, "state")), (200, "open".into()), "{out}");
    let (_, out) = gate.get(&format!("/api/journeys/{jid}"), Some(&sam));
    // Passing is the verdict, not the pay: sam's copy earned no XP but did pass.
    let j = parse(&out);
    assert_eq!(j["quests"][0]["state"], "passed", "sam's verdict passed quest 1: {out}");
    assert_eq!(j["quests"][1]["state"], "open", "{out}");
    assert_eq!(j["progress"]["xp"], 0, "{out}");

    // Quest 2: captured before the start, a JPEG, and no capture time.
    let before = Some(camera_clock(starts_at - 10 * 86_400));
    let p_early = arw(&pat, &shot("04", &grass, 6.1, 1.4, before, true));
    let (code, v, out) = submit(&pat, &q2, &p_early);
    assert_eq!(code, 201, "{out}");
    assert_eq!(v["pass"], false, "a photo taken before the quest started: {out}");
    assert_eq!(check(&v["verdict"], "captured_after_start")["ok"], false, "{out}");
    assert_eq!(check(&v["verdict"], "format")["ok"], true, "{out}");
    assert_eq!(check(&v["verdict"], "exposure.max_fnumber")["ok"], true, "{out}");

    let p_jpeg = jpg(&pat, &shot("05", &grass, 6.1, 1.4, after.clone(), true));
    let (_, v, out) = submit(&pat, &q2, &p_jpeg);
    assert_eq!(v["pass"], false, "{out}");
    assert_eq!(check(&v["verdict"], "format")["ok"], false, "a JPEG is not RAW: {out}");

    let p_nodate = arw(&pat, &shot("06", &grass, 6.1, 1.4, None, true));
    let (_, v, out) = submit(&pat, &q2, &p_nodate);
    assert_eq!(v["pass"], false, "{out}");
    assert!(
        check(&v["verdict"], "captured_after_start")["ok"].is_null(),
        "no captured_at is ok:null: {out}"
    );

    let p_slow = arw(&pat, &shot("07", &grass, 6.1, 2.8, after.clone(), true));
    let (_, v, out) = submit(&pat, &q2, &p_slow);
    assert_eq!(check(&v["verdict"], "exposure.max_fnumber")["ok"], false, "{out}");
    assert_eq!(check(&v["verdict"], "exposure.max_fnumber")["detail"], "2.8 > 1.4", "{out}");

    // Pass quest 2: 150 XP → 270, level 2 → 3, and the badge.
    let p_pass2 = arw(&pat, &shot("08", &grass, 6.1, 1.4, after.clone(), true));
    let (code, v, out) = submit(&pat, &q2, &p_pass2);
    assert_eq!(code, 201, "{out}");
    assert_eq!(v["pass"], true, "{out}");
    assert_eq!(v["xp_awarded"], 150, "{out}");
    assert_eq!(v["level_up"], json!({"journey": jid, "from": 2, "to": 3}), "{out}");
    assert_eq!(v["badge"], json!({"journey": jid, "name": "Park ranger"}), "{out}");

    // Passing quest 2 again with other bytes: no XP (once per quest), no second badge.
    let p_again = arw(&pat, &shot("09", &grass, 6.1, 1.4, after.clone(), true));
    let (_, v, out) = submit(&pat, &q2, &p_again);
    assert_eq!(v["pass"], true, "{out}");
    assert_eq!(v["xp_awarded"], 0, "a quest pays once per photographer: {out}");
    assert!(v["badge"].is_null(), "the badge is granted once: {out}");

    // --- my progress ----------------------------------------------------------------------
    let (code, out) = gate.get("/api/me/progress", Some(&pat));
    assert_eq!(code, 200, "{out}");
    let p = parse(&out);
    let mine = p["journeys"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["journey"] == jid))
        .cloned()
        .unwrap_or_else(|| panic!("progress has no row for the journey: {out}"));
    assert_eq!(mine["xp"], 270, "{out}");
    assert_eq!(mine["level"], 3, "{out}");
    assert!(mine["next_level_xp"].is_null(), "level 3 is the top: {out}");
    assert_eq!(mine["badge"]["name"], "Park ranger", "{out}");
    assert_eq!(p["total_xp"], 270, "{out}");
    assert_eq!(p["badges"].as_array().map(Vec::len), Some(1), "{out}");
    let ledger = p["ledger"].as_array().cloned().unwrap_or_default();
    assert_eq!(ledger.len(), 2, "two credits: {out}");
    let row = |q: &str| {
        ledger
            .iter()
            .find(|r| r["source"] == "quest" && r["source_id"] == q)
            .cloned()
            .unwrap_or_else(|| panic!("no ledger row for quest {q}: {out}"))
    };
    let (r1, r2) = (row(&q1), row(&q2));
    assert_eq!(r2["xp"], 150, "{out}");
    assert_eq!(r2["photo"], p_pass2, "{out}");
    assert_eq!(r1["xp"], 120, "{out}");
    assert_eq!(r1["photo"], p_pass1, "{out}");
    assert_eq!(r1["sha256"], "03".repeat(32), "{out}");
    assert_eq!(r1["journey"], jid, "{out}");

    let (_, out) = gate.get("/api/me/progress", Some(&sam));
    let p = parse(&out);
    assert_eq!(p["total_xp"], 0, "sam's copy of pat's bytes earned nothing: {out}");
    assert_eq!(p["ledger"].as_array().map(Vec::len), Some(0), "{out}");

    // --- archiving --------------------------------------------------------------------------
    let (code, out) =
        gate.post(&format!("/api/curator/quests/{q2}/archive"), Some(&cora), json!({}));
    assert_eq!(code, 200, "{out}");
    let (code, _, out) = submit(&pat, &q2, &p_pass2);
    assert_eq!((code, error_of(&out)), (409, "quest_not_published".into()), "{out}");
    let (code, out) =
        gate.post(&format!("/api/curator/journeys/{jid}/archive"), Some(&cora), json!({}));
    assert_eq!(code, 200, "{out}");
    let (code, _) = gate.get(&format!("/api/journeys/{jid}"), Some(&pat));
    assert_eq!(code, 404, "an archived journey is not shown to photographers");
    let (_, out) = gate.get("/api/me/progress", Some(&pat));
    assert_eq!(parse(&out)["total_xp"], 270, "the ledger is history; archiving keeps it: {out}");
    let (code, out) = gate.post("/api/curator/quests", Some(&cora), q1_body);
    assert_eq!((code, error_of(&out)), (409, "journey_archived".into()), "{out}");
}
