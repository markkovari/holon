//! `photoquest` browsing and the archive (CONTRACT.md "Browsing and the archive"):
//! lists that no longer only grow, and finished work that stays in sight.
//!
//! What this gate pins down:
//!
//! - `GET /api/competitions?view=active|past` splits on the clock (moved with
//!   `POST /test/clock`): active until results are out, past after — and an
//!   archived competition is past at once. A past competition stays readable
//!   (detail, leaderboard, results with winners); entering and voting stay refused.
//! - `GET /api/journeys?view=active|completed|history`: a journey moves to
//!   completed when its badge is earned (or every published quest passed), and an
//!   archived journey the photographer played is in history — its detail answers
//!   read-only (`archived: true`, with the badge and the submissions) to them, and
//!   404s for a stranger, and its quests still take no submissions.
//! - paging over more items than `limit`, in every list: every item exactly once,
//!   in the list's order; cursors are opaque and bound to their list.
//! - `q` narrows every list by title, case-insensitively.
//! - the curator lists filter by `state`.

mod gatelib;
mod photoquest_support;
use gatelib::{field, Gate};
use photoquest_support::*;
use serde_json::{json, Value};
use std::collections::HashSet;

const PASSWORD: &str = "correct horse";

/// A photo through the whole pipeline to `evaluated`, with its own sha256.
fn evaluated_photo(gate: &Gate, token: &str, sha: &str) -> String {
    let (code, out) = gate.post(
        "/api/photos",
        Some(token),
        json!({"filename": "DSC0001.ARW", "size": 1024, "content_type": "image/x-sony-arw"}),
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
    result["sha256"] = json!(sha);
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

fn set_clock(gate: &Gate, offset: i64) -> u64 {
    let (code, out) = gate.post("/test/clock", None, json!({"offset_secs": offset}));
    assert_eq!(code, 200, "POST /test/clock must work with allow-test-routes=true: {out}");
    parse(&out)["now"].as_u64().expect("clock answers now")
}

fn error_of(out: &str) -> String {
    field(out, "error")
}

/// Every page of `path` (which has no `after`), `limit` at a time: the items in
/// page order. Asserts each page respects the limit and the walk ends.
fn walk(gate: &Gate, token: &str, path: &str, key: &str, limit: usize) -> Vec<Value> {
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..200 {
        let url = match &after {
            None => format!("{path}{sep}limit={limit}"),
            Some(c) => format!("{path}{sep}limit={limit}&after={c}"),
        };
        let (code, body) = gate.get(&url, Some(token));
        assert_eq!(code, 200, "GET {url}: {body}");
        let v = parse(&body);
        let items = v[key].as_array().cloned().unwrap_or_default();
        assert!(items.len() <= limit, "GET {url} gave {} items for limit {limit}", items.len());
        out.extend(items);
        match v["next"].as_str() {
            Some(c) => after = Some(c.to_string()),
            None => return out,
        }
    }
    panic!("paging {path} never ended");
}

fn ids(items: &[Value]) -> Vec<String> {
    items.iter().map(|i| i["id"].as_str().unwrap_or_default().to_string()).collect()
}

/// The ids, checked to be free of duplicates.
fn unique(items: &[Value], what: &str) -> HashSet<String> {
    let v = ids(items);
    let set: HashSet<String> = v.iter().cloned().collect();
    assert_eq!(set.len(), v.len(), "{what}: an item came back twice: {v:?}");
    set
}

fn titles(items: &[Value]) -> Vec<String> {
    items.iter().map(|i| i["title"].as_str().unwrap_or_default().to_string()).collect()
}

#[test]
fn lists_split_by_view_page_without_gaps_search_and_keep_the_archive_readable() {
    let run = std::process::id();
    let admin_email = format!("root-{run}@photoquest.test");
    let bootstrap = format!("bootstrap-admin-email={admin_email}");
    let Some((gate, _media)) = start(&["allow-test-routes=true", bootstrap.as_str()]) else {
        return;
    };

    // --- accounts ---------------------------------------------------------------
    let account = |who: &str| -> (String, String) {
        let email =
            if who == "root" { admin_email.clone() } else { format!("{who}-{run}@photoquest.test") };
        let (code, reg) = gate.post("/register", None, json!({"email": email, "password": PASSWORD}));
        assert_eq!(code, 201, "register {who}: {reg}");
        let (code, out) = gate.post("/login", None, json!({"email": email, "password": PASSWORD}));
        assert_eq!(code, 200, "login {who}: {out}");
        (field(&out, "access_token"), field(&reg, "subject"))
    };
    let (root, _) = account("root");
    let (_, cora_sub) = account("cora");
    let (code, out) =
        gate.post(&format!("/api/admin/users/{cora_sub}/roles"), Some(&root), json!({"grant": "curator"}));
    assert_eq!(code, 200, "grant curator: {out}");
    // Roles are re-read on every request; the token cora got before is fine.
    let (code, out) =
        gate.post("/login", None, json!({"email": format!("cora-{run}@photoquest.test"), "password": PASSWORD}));
    assert_eq!(code, 200, "{out}");
    let cora = field(&out, "access_token");
    let (pat, _) = account("pat");
    let (sam, _) = account("sam");

    let now = set_clock(&gate, 0);

    // ==== competitions ============================================================
    let comp = |title: &str, closes: u64, judging: u64, publish: bool| -> String {
        let (code, out) = gate.post(
            "/api/curator/competitions",
            Some(&cora),
            json!({
                "title": title, "brief": "", "requirements": {"captured_after_start": false},
                "opens_at": now - 60, "closes_at": closes, "voting_closes_at": closes + 10,
                "judging_closes_at": judging, "weights": {"auto": 1.0},
                "max_entries_per_user": 1, "prizes_xp": [50], "journey": null,
            }),
        );
        assert_eq!(code, 201, "create competition {title}: {out}");
        let id = field(&out, "id");
        if publish {
            let (code, out) = gate.post(&format!("/api/curator/competitions/{id}/publish"), Some(&cora), json!({}));
            assert_eq!(code, 200, "publish {title}: {out}");
        }
        id
    };
    // Five that finish in a few minutes, twenty-three that run for hours.
    let short: Vec<String> =
        (0..5).map(|i| comp(&format!("Blue minute {i}"), now + 100 + i, now + 200 + i, true)).collect();
    let long: Vec<String> = (0..23)
        .map(|i| comp(&format!("Golden hour {i}"), now + 10_000 + 10 * i, now + 20_000 + 10 * i, true))
        .collect();
    // One archived while it was still running, and one that never left draft.
    let archived_comp = comp("Retired golden", now + 50_000, now + 60_000, true);
    let (code, out) =
        gate.post(&format!("/api/curator/competitions/{archived_comp}/archive"), Some(&cora), json!({}));
    assert_eq!(code, 200, "archive: {out}");
    let draft_comp = comp("Golden draft", now + 1000, now + 2000, false);

    // pat enters one of the short ones, so there is a winner to read later.
    let pat_photo = evaluated_photo(&gate, &pat, &"c1".repeat(32));
    let (code, out) = gate.post(
        &format!("/api/competitions/{}/entries", short[0]),
        Some(&pat),
        json!({"photo_id": pat_photo}),
    );
    assert_eq!(code, 201, "enter: {out}");
    let entry = field(&out, "id");

    // --- now: every published one is active, soonest to close first ------------
    let active = walk(&gate, &sam, "/api/competitions", "competitions", 7);
    let got = unique(&active, "active competitions");
    let want: HashSet<String> = short.iter().chain(&long).cloned().collect();
    assert_eq!(got, want, "active = every published competition, no draft, no archived");
    let closes: Vec<u64> = active.iter().map(|c| c["closes_at"].as_u64().unwrap_or(0)).collect();
    assert!(closes.windows(2).all(|w| w[0] <= w[1]), "soonest closes_at first: {closes:?}");
    assert!(active.iter().all(|c| c["results_available"] == false), "{active:?}");
    // The default view is active; the default page is 20.
    let (_, out) = gate.get("/api/competitions", Some(&sam));
    let v = parse(&out);
    assert_eq!(v["view"], "active", "{out}");
    assert_eq!(v["competitions"].as_array().map(Vec::len), Some(20), "default limit 20: {out}");
    assert!(v["next"].is_string(), "28 items: there is a next page: {out}");

    let past = walk(&gate, &sam, "/api/competitions?view=past", "competitions", 3);
    assert_eq!(ids(&past), vec![archived_comp.clone()], "past = only the archived one, for now");
    assert!(!ids(&active).contains(&draft_comp) && !ids(&past).contains(&draft_comp));

    // --- the clock passes the short ones' judging: they move to past --------------
    set_clock(&gate, 1000);
    let active = walk(&gate, &sam, "/api/competitions?view=active", "competitions", 5);
    assert_eq!(unique(&active, "active later"), long.iter().cloned().collect(), "only the long ones stay active");
    let past = walk(&gate, &sam, "/api/competitions?view=past", "competitions", 4);
    let got = unique(&past, "past later");
    let want: HashSet<String> = short.iter().chain([&archived_comp]).cloned().collect();
    assert_eq!(got, want, "past = results out, or archived");
    let judging: Vec<u64> = past.iter().map(|c| c["judging_closes_at"].as_u64().unwrap_or(0)).collect();
    assert!(judging.windows(2).all(|w| w[0] >= w[1]), "latest results first: {judging:?}");
    assert_eq!(past[0]["id"], archived_comp.as_str(), "{past:?}");
    assert_eq!(past[0]["phase"], "archived", "{past:?}");

    // --- search ----------------------------------------------------------------------
    let found = walk(&gate, &sam, "/api/competitions?view=active&q=GOLDEN%20hour%201", "competitions", 100);
    let mut t = titles(&found);
    t.sort();
    let mut want: Vec<String> = std::iter::once(1).chain(10..20).map(|i| format!("Golden hour {i}")).collect();
    want.sort();
    assert_eq!(t, want, "q is a case-insensitive title substring");
    let found = walk(&gate, &sam, "/api/competitions?view=past&q=blue", "competitions", 2);
    assert_eq!(found.len(), 5, "q narrows past too: {found:?}");
    let found = walk(&gate, &sam, "/api/competitions?view=past&q=nothing%20like%20it", "competitions", 2);
    assert!(found.is_empty(), "{found:?}");

    // --- refusals -----------------------------------------------------------------------
    let (code, out) = gate.get("/api/competitions?view=later", Some(&sam));
    assert_eq!((code, error_of(&out)), (400, "bad_view".into()), "{out}");
    let (code, out) = gate.get("/api/competitions?limit=0", Some(&sam));
    assert_eq!((code, error_of(&out)), (400, "bad_limit".into()), "{out}");
    let (code, out) = gate.get("/api/competitions?after=not-a-cursor", Some(&sam));
    assert_eq!((code, error_of(&out)), (400, "bad_cursor".into()), "{out}");
    let (_, out) = gate.get("/api/competitions?view=active&limit=2", Some(&sam));
    let active_cursor = parse(&out)["next"].as_str().unwrap_or_default().to_string();
    let (code, out) = gate.get(&format!("/api/competitions?view=past&after={active_cursor}"), Some(&sam));
    assert_eq!((code, error_of(&out)), (400, "bad_cursor".into()), "a cursor is bound to its view: {out}");
    let (code, out) = gate.get("/api/competitions?view=active&limit=1000", Some(&sam));
    assert_eq!(code, 200, "{out}");
    assert_eq!(parse(&out)["competitions"].as_array().map(Vec::len), Some(23), "a big limit is cut to 100: {out}");

    // --- a past competition stays readable, and closed ---------------------------------
    let (code, out) = gate.get(&format!("/api/competitions/{}", short[0]), Some(&sam));
    assert_eq!(code, 200, "{out}");
    assert_eq!((field(&out, "phase"), parse(&out)["results_available"].clone()), ("finished".into(), json!(true)));
    let (code, out) = gate.get(&format!("/api/competitions/{}/leaderboard", short[0]), Some(&sam));
    assert_eq!(code, 200, "{out}");
    assert_eq!(parse(&out)["entries"].as_array().map(Vec::len), Some(1), "the final leaderboard: {out}");
    let (code, out) = gate.get(&format!("/api/competitions/{}/results", short[0]), Some(&sam));
    assert_eq!(code, 200, "{out}");
    assert_eq!(parse(&out)["winners"][0]["entry"], entry.as_str(), "the winner is on the results: {out}");
    let sam_photo = evaluated_photo(&gate, &sam, &"c2".repeat(32));
    let (code, out) = gate.post(
        &format!("/api/competitions/{}/entries", short[1]),
        Some(&sam),
        json!({"photo_id": sam_photo}),
    );
    assert_eq!((code, error_of(&out)), (409, "competition_closed".into()), "{out}");
    let (code, out) = gate.json(
        "PUT",
        &format!("/api/competitions/{}/entries/{entry}/vote", short[0]),
        Some(&sam),
        Some(json!({"stars": 5})),
    );
    assert_eq!((code, error_of(&out)), (409, "voting_closed".into()), "{out}");
    // Archived: readable, and not open to anything.
    let (code, out) = gate.get(&format!("/api/competitions/{archived_comp}"), Some(&sam));
    assert_eq!((code, field(&out, "phase")), (200, "archived".into()), "{out}");
    let (code, _) = gate.get(&format!("/api/competitions/{archived_comp}/leaderboard"), Some(&sam));
    assert_eq!(code, 200);
    let (code, _) = gate.post(
        &format!("/api/competitions/{archived_comp}/entries"),
        Some(&sam),
        json!({"photo_id": sam_photo}),
    );
    assert_eq!(code, 404, "an archived competition takes no entries");
    set_clock(&gate, 0);

    // --- curator competitions by state -------------------------------------------------
    let curator_ids = |q: &str, limit: usize| -> HashSet<String> {
        let items = walk(&gate, &cora, &format!("/api/curator/competitions?{q}"), "competitions", limit);
        unique(&items, q)
    };
    assert_eq!(curator_ids("state=draft", 5), HashSet::from([draft_comp.clone()]));
    assert_eq!(curator_ids("state=archived", 5), HashSet::from([archived_comp.clone()]));
    assert_eq!(curator_ids("state=published", 6), short.iter().chain(&long).cloned().collect::<HashSet<_>>());
    assert_eq!(curator_ids("state=all", 9).len(), 30, "every competition, once");
    assert_eq!(curator_ids("", 50).len(), 30, "state defaults to all");
    assert_eq!(curator_ids("state=all&q=golden", 4).len(), 25, "golden hour ×23, retired, draft");
    let (code, out) = gate.get("/api/curator/competitions?state=gone", Some(&cora));
    assert_eq!((code, error_of(&out)), (400, "bad_state".into()), "{out}");
    let (code, out) = gate.get("/api/curator/competitions?state=draft", Some(&pat));
    assert_eq!((code, error_of(&out)), (403, "forbidden_role".into()), "{out}");

    // ==== journeys =================================================================
    // `label` is what the quest's requirement asks for: the fake evaluation labels
    // every photo "people", so "people" passes and "unicorn" does not.
    let journey = |title: &str, badge: Option<&str>, labels: &[&str]| -> (String, Vec<String>) {
        let (code, out) = gate.post(
            "/api/curator/journeys",
            Some(&cora),
            json!({"title": title, "badge": badge.map(|b| json!({"name": b}))}),
        );
        assert_eq!(code, 201, "create journey {title}: {out}");
        let jid = field(&out, "id");
        let mut quests = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            let (code, out) = gate.post(
                "/api/curator/quests",
                Some(&cora),
                json!({"journey": jid, "title": format!("{title} step {i}"), "xp": 10, "starts_at": now - 3600,
                       "requirements": {"subject": {"label": label, "min_confidence": 0.5}, "captured_after_start": false}}),
            );
            assert_eq!(code, 201, "create quest: {out}");
            let qid = field(&out, "id");
            let (code, out) = gate.post(&format!("/api/curator/quests/{qid}/publish"), Some(&cora), json!({}));
            assert_eq!(code, 200, "publish quest: {out}");
            quests.push(qid);
        }
        let (code, out) = gate.post(&format!("/api/curator/journeys/{jid}/publish"), Some(&cora), json!({}));
        assert_eq!(code, 200, "publish journey: {out}");
        (jid, quests)
    };
    let (park, park_q) = journey("Park walk", Some("Park ranger"), &["people", "people"]);
    let (harbour, harbour_q) = journey("Harbour", Some("Sailor"), &["people"]);
    let (untouched, _) = journey("Untouched", None, &["people"]);
    let (hard, hard_q) = journey("Hard one", None, &["unicorn"]);
    let fillers: Vec<String> = (0..21).map(|i| journey(&format!("Filler {i}"), None, &["people"]).0).collect();
    // A draft journey with a draft quest, for the curator filters.
    let (code, out) = gate.post("/api/curator/journeys", Some(&cora), json!({"title": "Draft idea"}));
    assert_eq!(code, 201, "{out}");
    let draft_journey = field(&out, "id");
    let (code, out) = gate.post(
        "/api/curator/quests",
        Some(&cora),
        json!({"journey": draft_journey, "title": "Draft quest", "xp": 5}),
    );
    assert_eq!(code, 201, "{out}");
    let draft_quest = field(&out, "id");

    let mut shas = 0u32;
    let mut submit = |quest: &str| -> Value {
        shas += 1;
        let photo = evaluated_photo(&gate, &pat, &format!("{shas:064x}"));
        let (code, out) =
            gate.post(&format!("/api/quests/{quest}/submissions"), Some(&pat), json!({"photo_id": photo}));
        assert_eq!(code, 201, "submit to {quest}: {out}");
        parse(&out)
    };
    assert_eq!(submit(&park_q[0])["pass"], true);
    let h = submit(&harbour_q[0]);
    assert_eq!(h["badge"]["name"], "Sailor", "finishing harbour earns its badge: {h}");
    assert_eq!(submit(&hard_q[0])["pass"], false, "a submission that did not pass");

    // --- active / completed / history ----------------------------------------------------
    let view = |v: &str, limit: usize| walk(&gate, &pat, &format!("/api/journeys?view={v}"), "journeys", limit);
    let active = view("active", 7);
    let got = unique(&active, "active journeys");
    let want: HashSet<String> =
        [&park, &untouched, &hard].into_iter().chain(&fillers).cloned().collect();
    assert_eq!(got, want, "active = published and not completed (harbour is done)");
    assert_eq!(active.len(), 24, "more than one page, every one once");
    let published_at: Vec<u64> = active.iter().map(|j| j["id"].as_str().map(|_| 0).unwrap_or(0)).collect();
    assert_eq!(published_at.len(), 24);
    let park_row = active.iter().find(|j| j["id"] == park.as_str()).cloned().unwrap_or_default();
    assert_eq!((park_row["passed_count"].clone(), park_row["quest_count"].clone()), (json!(1), json!(2)), "{park_row}");
    assert_eq!(park_row["completed"], false, "{park_row}");
    // The default view is active, and a stranger's active list has harbour in it.
    let (_, out) = gate.get("/api/journeys", Some(&pat));
    assert_eq!(parse(&out)["view"], "active", "{out}");
    let sam_active = walk(&gate, &sam, "/api/journeys", "journeys", 30);
    assert!(ids(&sam_active).contains(&harbour), "completion is per photographer");

    let completed = view("completed", 5);
    assert_eq!(ids(&completed), vec![harbour.clone()], "{completed:?}");
    assert_eq!(completed[0]["progress"]["badge"]["name"], "Sailor", "{completed:?}");
    assert_eq!(completed[0]["completed"], true);
    assert!(view("history", 5).is_empty(), "nothing is archived yet");

    // Search narrows the view it is given.
    let found = walk(&gate, &pat, "/api/journeys?view=active&q=PARK", "journeys", 5);
    assert_eq!(ids(&found), vec![park.clone()], "{found:?}");
    let found = walk(&gate, &pat, "/api/journeys?view=active&q=filler%201", "journeys", 3);
    assert_eq!(found.len(), 11, "Filler 1, 10–19: {:?}", titles(&found));
    let found = walk(&gate, &pat, "/api/journeys?view=completed&q=park", "journeys", 5);
    assert!(found.is_empty(), "park is not completed yet: {found:?}");
    let (code, out) = gate.get("/api/journeys?view=everything", Some(&pat));
    assert_eq!((code, error_of(&out)), (400, "bad_view".into()), "{out}");

    // Passing the last quest completes park: it moves from active to completed.
    let last = submit(&park_q[1]);
    assert_eq!(last["badge"]["name"], "Park ranger", "{last}");
    assert!(!ids(&view("active", 50)).contains(&park), "a completed journey leaves active");
    let done: HashSet<String> = ids(&view("completed", 1)).into_iter().collect();
    assert_eq!(done, HashSet::from([park.clone(), harbour.clone()]));

    // --- archiving: history, read-only, for the players only -------------------------------
    for j in [&park, &untouched, &hard] {
        let (code, out) = gate.post(&format!("/api/curator/journeys/{j}/archive"), Some(&cora), json!({}));
        assert_eq!(code, 200, "archive: {out}");
    }
    let history = view("history", 1);
    let got = unique(&history, "history");
    assert_eq!(
        got,
        HashSet::from([park.clone(), hard.clone()]),
        "history = archived journeys pat has a submission in (a failed one counts), not untouched"
    );
    let park_row = history.iter().find(|j| j["id"] == park.as_str()).cloned().unwrap_or_default();
    assert_eq!((park_row["archived"].clone(), park_row["completed"].clone()), (json!(true), json!(true)), "{park_row}");
    assert_eq!(park_row["progress"]["badge"]["name"], "Park ranger", "the badge stays: {park_row}");
    assert_eq!(park_row["progress"]["xp"], 20, "{park_row}");
    let hard_row = history.iter().find(|j| j["id"] == hard.as_str()).cloned().unwrap_or_default();
    assert_eq!(hard_row["completed"], false, "{hard_row}");
    assert_eq!(ids(&view("completed", 5)), vec![harbour.clone()], "an archived journey leaves completed");
    let active = view("active", 50);
    assert!(!ids(&active).iter().any(|j| [&park, &untouched, &hard].contains(&j)), "{active:?}");
    let found = walk(&gate, &pat, "/api/journeys?view=history&q=hard", "journeys", 5);
    assert_eq!(ids(&found), vec![hard.clone()]);

    let (code, out) = gate.get(&format!("/api/journeys/{park}"), Some(&pat));
    assert_eq!(code, 200, "a player still reads an archived journey: {out}");
    let d = parse(&out);
    assert_eq!(d["archived"], true, "{out}");
    assert_eq!(d["progress"]["badge"]["name"], "Park ranger", "{out}");
    assert_eq!(d["submissions"].as_array().map(Vec::len), Some(2), "my submissions to it: {out}");
    assert_eq!(d["submissions"][0]["quest_title"], "Park walk step 1", "newest first, titled: {out}");
    let states: Vec<Value> = d["quests"].as_array().into_iter().flatten().map(|q| q["state"].clone()).collect();
    assert_eq!(states, vec![json!("passed"), json!("passed")], "{out}");
    let (code, _) = gate.get(&format!("/api/journeys/{park}"), Some(&sam));
    assert_eq!(code, 404, "a stranger gets a 404, as for a draft");
    let (code, _) = gate.get(&format!("/api/journeys/{untouched}"), Some(&pat));
    assert_eq!(code, 404, "pat never played untouched");
    let (code, _) = gate.get(&format!("/api/journeys/{draft_journey}"), Some(&pat));
    assert_eq!(code, 404, "a draft is still a 404");
    // Read only: its quests take nothing, the way they did before.
    let photo = evaluated_photo(&gate, &pat, &"ee".repeat(32));
    let (code, _) =
        gate.post(&format!("/api/quests/{}/submissions", park_q[0]), Some(&pat), json!({"photo_id": photo}));
    assert_eq!(code, 404, "a quest of an archived journey takes no submissions");
    let (code, _) = gate.get(&format!("/api/quests/{}", park_q[0]), Some(&pat));
    assert_eq!(code, 404);
    // The ledger is history: my progress still lists it.
    let (_, out) = gate.get("/api/me/progress", Some(&pat));
    let p = parse(&out);
    assert_eq!(p["total_xp"], 30, "park 20 + harbour 10: {out}");
    assert!(
        p["journeys"].as_array().into_iter().flatten().any(|j| j["journey"] == park.as_str() && j["state"] == "archived"),
        "{out}"
    );

    // --- curator journeys and quests by state ----------------------------------------------
    let cur = |list: &str, q: &str, limit: usize| -> HashSet<String> {
        let items = walk(&gate, &cora, &format!("/api/curator/{list}?{q}"), list, limit);
        unique(&items, &format!("{list}?{q}"))
    };
    assert_eq!(cur("journeys", "state=draft", 5), HashSet::from([draft_journey.clone()]));
    assert_eq!(cur("journeys", "state=archived", 2), HashSet::from([park.clone(), untouched.clone(), hard.clone()]));
    let published: HashSet<String> = std::iter::once(harbour.clone()).chain(fillers.iter().cloned()).collect();
    assert_eq!(cur("journeys", "state=published", 4), published);
    assert_eq!(cur("journeys", "state=all", 6).len(), 26, "every journey once");
    assert_eq!(cur("journeys", "q=HARBOUR", 6), HashSet::from([harbour.clone()]));
    assert_eq!(cur("quests", "state=draft", 5), HashSet::from([draft_quest.clone()]));
    assert_eq!(cur("quests", "state=all", 7).len(), 28, "2 + 1 + 1 + 1 + 21 published, 1 draft, once each");
    assert_eq!(cur("quests", "state=published&q=park%20walk", 1).len(), 2);
    let (code, out) = gate.get("/api/curator/journeys?state=nope", Some(&cora));
    assert_eq!((code, error_of(&out)), (400, "bad_state".into()), "{out}");
    let (code, out) = gate.get("/api/curator/quests", Some(&pat));
    assert_eq!((code, error_of(&out)), (403, "forbidden_role".into()), "{out}");
}
