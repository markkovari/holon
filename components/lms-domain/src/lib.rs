//! `lms-domain` — a learning-management service (docs/apps/LMS.md) as ONE composed wasm
//! HTTP component. Exports `wasi:http`; imports only WIT contracts: the composed
//! auth-guard (`auth:identity`), `records:store`, `quiz:grade` (auto-grading +
//! gradebook stats), `pdf:codec` (certificates) and `svg:chart` (the gradebook
//! chart). No bespoke auth, storage, grading, PDF, or charting.

#[allow(warnings)]
mod bindings;

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::pdf::codec::codec as pdf;
use bindings::quiz::grade::grader as quiz;
use bindings::records::store::store as records;
use bindings::svg::chart::charts as svg;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "lms";
const USERS: &str = "users";
const COURSES: &str = "courses";
const LESSONS: &str = "lessons";
const QUIZZES: &str = "quizzes";
const ENROLLMENTS: &str = "enrollments";
const SUBMISSIONS: &str = "submissions";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "courses"]) => create_course(&request),
            (Method::Get, ["api", "courses"]) => list_courses(&request),
            (Method::Get, ["api", "courses", id]) => get_course(&request, id),
            (Method::Post, ["api", "courses", id, "lessons"]) => add_lesson(&request, id),
            (Method::Post, ["api", "courses", id, "quizzes"]) => add_quiz(&request, id),
            (Method::Post, ["api", "courses", id, "enroll"]) => enroll(&request, id),
            (Method::Get, ["api", "courses", id, "progress"]) => progress(&request, id),
            (Method::Get, ["api", "courses", id, "gradebook"]) => gradebook(&request, id),
            (Method::Get, ["api", "courses", id, "certificate.pdf"]) => certificate(&request, id),
            (Method::Post, ["api", "quizzes", id, "submit"]) => submit_quiz(&request, id),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    File(u16, String, Option<String>, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "lms",
            "about": "a learning platform — courses/lessons/quizzes, enrollments, auto-graded submissions, gradebook + certificates",
            "auth": "POST /api/register|login|logout (role: instructor|student), GET /api/me",
            "instructor": "POST /api/courses, POST /api/courses/{id}/lessons|quizzes, GET /api/courses/{id}/gradebook",
            "student": "POST /api/courses/{id}/enroll, POST /api/quizzes/{id}/submit {answers}, GET /api/courses/{id}/progress|certificate.pdf"
        })
        .to_string(),
    )
}

// ---- auth -------------------------------------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get(&"authorization".to_string());
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token = bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn is_instructor(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "instructor")
}

fn subject_email(subject: &str) -> String {
    records::find_by(USERS, "subject", &json!(subject).to_string())
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["email"].as_str().map(String::from))
        .unwrap_or_default()
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    let wanted = body["role"].as_str().unwrap_or("student");
    let role = if ["instructor", "student"].contains(&wanted) { wanted } else { "student" };
    let _ = rbac::assign_role(&p.tenant, &p.subject, role);
    let _ = records::create(USERS, &json!({ "subject": p.subject, "email": email }).to_string(), &["subject".to_string(), "email".to_string()]);
    if role == "instructor" {
        seed_demo(&p.subject);
    }
    Outcome::Json(201, json!({ "subject": p.subject, "roles": [role] }).to_string())
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(
            200,
            json!({ "subject": p.subject, "roles": p.roles, "email": subject_email(&p.subject), "is_instructor": is_instructor(&p) }).to_string(),
        ),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

// ---- records helpers --------------------------------------------------------

fn get(coll: &str, id: &str) -> Option<Value> {
    records::get(coll, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(|mut v| {
        v["id"] = json!(id);
        v
    })
}

fn find(coll: &str, field: &str, value: &str) -> Vec<Value> {
    records::find_by(coll, field, &json!(value).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
            v["id"] = json!(e.id);
            v
        }))
        .collect()
}

// ---- courses / lessons / quizzes --------------------------------------------

fn create_course(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_instructor(&p) {
        return Outcome::Err(403, "instructors only".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let code = b["code"].as_str().unwrap_or("").trim().to_string();
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    if code.is_empty() || title.is_empty() {
        return Outcome::Err(422, "code and title required".into());
    }
    let d = json!({ "code": code, "title": title, "description": b["description"].as_str().unwrap_or(""), "instructor": p.subject, "instructor_email": subject_email(&p.subject), "created": now() });
    match records::create(COURSES, &d.to_string(), &["instructor".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn list_courses(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let my_enrolled: std::collections::HashSet<String> = find(ENROLLMENTS, "student", &p.subject)
        .iter()
        .filter_map(|e| e["course"].as_str().map(String::from))
        .collect();
    let items: Vec<Value> = records::list_records(COURSES, 1000, "")
        .map(|pg| pg.entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
            let id = e.id.clone();
            v["id"] = json!(id);
            v["enrolled"] = json!(my_enrolled.contains(&id));
            v["is_mine"] = json!(v["instructor"].as_str() == Some(&p.subject));
            v["lessons"] = json!(find(LESSONS, "course", &id).len());
            v["quizzes"] = json!(find(QUIZZES, "course", &id).len());
            v
        }))
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn lessons_of(course: &str) -> Vec<Value> {
    let mut v = find(LESSONS, "course", course);
    v.sort_by_key(|l| (l["order"].as_i64().unwrap_or(0), l["created"].as_u64().unwrap_or(0)));
    v
}

fn quizzes_of(course: &str) -> Vec<Value> {
    let mut v = find(QUIZZES, "course", course);
    v.sort_by_key(|q| q["created"].as_u64().unwrap_or(0));
    v
}

/// Strip the answer key from a quiz for a student view.
fn quiz_public(q: &Value) -> Value {
    let questions: Vec<Value> = q["questions"]
        .as_array()
        .map(|a| a.iter().map(|qu| json!({ "prompt": qu["prompt"], "options": qu["options"] })).collect())
        .unwrap_or_default();
    json!({ "id": q["id"], "title": q["title"], "pass_mark": q["pass_mark"], "questions": questions })
}

fn get_course(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let course = match get(COURSES, id) {
        Some(c) => c,
        None => return Outcome::Err(404, "not_found".into()),
    };
    let owner = course["instructor"].as_str() == Some(&p.subject);
    let quizzes: Vec<Value> = quizzes_of(id).iter().map(|q| if owner { q.clone() } else { quiz_public(q) }).collect();
    let enrolled = !find(ENROLLMENTS, "student", &p.subject).iter().all(|e| e["course"].as_str() != Some(id));
    Outcome::Json(
        200,
        json!({ "course": course, "lessons": lessons_of(id), "quizzes": quizzes, "enrolled": enrolled, "is_mine": owner }).to_string(),
    )
}

/// Ensure the caller owns the course (instructor); returns the course or an error.
fn owned_course(p: &Principal, id: &str) -> Result<Value, Outcome> {
    let c = get(COURSES, id).ok_or(Outcome::Err(404, "no such course".into()))?;
    if c["instructor"].as_str() != Some(&p.subject) {
        return Err(Outcome::Err(403, "not your course".into()));
    }
    Ok(c)
}

fn add_lesson(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(o) = owned_course(&p, id) {
        return o;
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() {
        return Outcome::Err(422, "title required".into());
    }
    let order = lessons_of(id).len() as i64;
    let d = json!({ "course": id, "title": title, "body": b["body"].as_str().unwrap_or(""), "order": order, "created": now() });
    match records::create(LESSONS, &d.to_string(), &["course".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn add_quiz(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(o) = owned_course(&p, id) {
        return o;
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    let questions = b["questions"].as_array().cloned().unwrap_or_default();
    if title.is_empty() || questions.is_empty() {
        return Outcome::Err(422, "title and at least one question required".into());
    }
    // validate each question: a prompt, >= 2 options, an answer index in range.
    for q in &questions {
        let opts = q["options"].as_array().map(|a| a.len()).unwrap_or(0);
        let ans = q["answer"].as_u64().unwrap_or(u64::MAX);
        if q["prompt"].as_str().unwrap_or("").is_empty() || opts < 2 || ans as usize >= opts {
            return Outcome::Err(422, "each question needs a prompt, >=2 options, and a valid answer index".into());
        }
    }
    let pass_mark = b["pass_mark"].as_u64().unwrap_or(60).min(100);
    let d = json!({ "course": id, "title": title, "pass_mark": pass_mark, "questions": questions, "created": now() });
    match records::create(QUIZZES, &d.to_string(), &["course".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

// ---- enroll / submit --------------------------------------------------------

fn enroll(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if get(COURSES, id).is_none() {
        return Outcome::Err(404, "no such course".into());
    }
    if find(ENROLLMENTS, "course", id).iter().any(|e| e["student"].as_str() == Some(&p.subject)) {
        return Outcome::Json(200, json!({ "ok": true, "already": true }).to_string());
    }
    let d = json!({ "course": id, "student": p.subject, "email": subject_email(&p.subject), "created": now() });
    match records::create(ENROLLMENTS, &d.to_string(), &["course".to_string(), "student".to_string()]) {
        Ok(_) => Outcome::Json(201, json!({ "ok": true }).to_string()),
        Err(e) => store_err(e),
    }
}

fn submit_quiz(request: &IncomingRequest, quiz_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let quiz = match get(QUIZZES, quiz_id) {
        Some(q) => q,
        None => return Outcome::Err(404, "no such quiz".into()),
    };
    let course = quiz["course"].as_str().unwrap_or("").to_string();
    if !find(ENROLLMENTS, "course", &course).iter().any(|e| e["student"].as_str() == Some(&p.subject)) {
        return Outcome::Err(403, "enroll in the course first".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let answers: Vec<u32> = b["answers"].as_array().map(|a| a.iter().map(|x| x.as_u64().unwrap_or(u64::MAX) as u32).collect()).unwrap_or_default();
    let key: Vec<u32> = quiz["questions"].as_array().map(|a| a.iter().map(|q| q["answer"].as_u64().unwrap_or(0) as u32).collect()).unwrap_or_default();
    let pass_mark = quiz["pass_mark"].as_u64().unwrap_or(60) as u32;

    // the grading lives in quiz:grade.
    let r = quiz::grade(&answers, &key, pass_mark);
    let d = json!({
        "quiz": quiz_id, "course": course, "student": p.subject, "email": subject_email(&p.subject),
        "answers": answers, "correct": r.correct, "total": r.total, "score_pct": r.score_pct, "passed": r.passed, "created": now()
    });
    let _ = records::create(SUBMISSIONS, &d.to_string(), &["quiz".to_string(), "student".to_string(), "course".to_string()]);
    Outcome::Json(
        200,
        json!({ "correct": r.correct, "total": r.total, "score_pct": r.score_pct, "passed": r.passed }).to_string(),
    )
}

// ---- progress / gradebook / certificate -------------------------------------

/// A student's best submission per quiz in a course: quiz_id -> (best_pct, passed).
fn best_scores(course: &str, student: &str) -> BTreeMap<String, (u32, bool)> {
    let mut best: BTreeMap<String, (u32, bool)> = BTreeMap::new();
    for s in find(SUBMISSIONS, "student", student) {
        if s["course"].as_str() != Some(course) {
            continue;
        }
        let q = s["quiz"].as_str().unwrap_or("").to_string();
        let pct = s["score_pct"].as_u64().unwrap_or(0) as u32;
        let entry = best.entry(q).or_insert((0, false));
        if pct >= entry.0 {
            *entry = (pct, s["passed"].as_bool().unwrap_or(false) || entry.1);
        } else {
            entry.1 = entry.1 || s["passed"].as_bool().unwrap_or(false);
        }
    }
    best
}

fn progress(request: &IncomingRequest, course: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if get(COURSES, course).is_none() {
        return Outcome::Err(404, "not_found".into());
    }
    let quizzes = quizzes_of(course);
    let best = best_scores(course, &p.subject);
    let rows: Vec<Value> = quizzes
        .iter()
        .map(|q| {
            let qid = q["id"].as_str().unwrap_or("");
            let (pct, passed) = best.get(qid).copied().unwrap_or((0, false));
            let attempted = best.contains_key(qid);
            json!({ "quiz": qid, "title": q["title"], "best_score": pct, "passed": passed, "attempted": attempted })
        })
        .collect();
    let passed_all = !quizzes.is_empty() && quizzes.iter().all(|q| best.get(q["id"].as_str().unwrap_or("")).map(|(_, p)| *p).unwrap_or(false));
    let done = rows.iter().filter(|r| r["passed"].as_bool().unwrap_or(false)).count();
    let completion = if quizzes.is_empty() { 0 } else { (done * 100 / quizzes.len()) as u32 };
    Outcome::Json(
        200,
        json!({ "quizzes": rows, "passed_all": passed_all, "completion_pct": completion, "certificate_eligible": passed_all }).to_string(),
    )
}

fn gradebook(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(o) = owned_course(&p, id) {
        return o;
    }
    let quizzes = quizzes_of(id);
    let students = find(ENROLLMENTS, "course", id);

    // per-student row: best score per quiz + their average.
    let mut student_rows = Vec::new();
    // collect per-quiz score lists for the distribution.
    let mut per_quiz_scores: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for e in &students {
        let sub = e["student"].as_str().unwrap_or("");
        let best = best_scores(id, sub);
        let mut scores = Map::new();
        let mut sum = 0u32;
        for q in &quizzes {
            let qid = q["id"].as_str().unwrap_or("");
            let pct = best.get(qid).map(|(p, _)| *p).unwrap_or(0);
            scores.insert(qid.to_string(), json!(pct));
            sum += pct;
            per_quiz_scores.entry(qid.to_string()).or_default().push(pct);
        }
        let avg = if quizzes.is_empty() { 0 } else { sum / quizzes.len() as u32 };
        student_rows.push(json!({ "email": e["email"], "scores": scores, "average": avg,
            "passed_all": !quizzes.is_empty() && quizzes.iter().all(|q| best.get(q["id"].as_str().unwrap_or("")).map(|(_, p)| *p).unwrap_or(false)) }));
    }

    // per-quiz stats via quiz:grade, and a class-average bar chart via svg:chart.
    let mut quiz_meta = Vec::new();
    let mut chart_slices = Vec::new();
    for q in &quizzes {
        let qid = q["id"].as_str().unwrap_or("");
        let pm = q["pass_mark"].as_u64().unwrap_or(60) as u32;
        let scores = per_quiz_scores.get(qid).cloned().unwrap_or_default();
        let st = quiz::distribution(&scores, pm);
        quiz_meta.push(json!({ "id": qid, "title": q["title"], "pass_mark": pm,
            "mean": st.mean, "median": st.median, "min": st.min, "max": st.max, "pass_count": st.pass_count, "count": st.count, "buckets": st.buckets }));
        chart_slices.push(svg::Slice { label: q["title"].as_str().unwrap_or("").to_string(), value: st.mean as f64, color: String::new() });
    }
    let chart = svg::Chart { kind: svg::Kind::Bar, title: "Class average by quiz (%)".to_string(), data: chart_slices, width: 480, height: 240 };
    let chart_svg = svg::render(&chart);

    Outcome::Json(
        200,
        json!({ "students": student_rows, "quizzes": quiz_meta, "enrolled": students.len(), "chart_svg": chart_svg }).to_string(),
    )
}

fn certificate(request: &IncomingRequest, course: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let c = match get(COURSES, course) {
        Some(c) => c,
        None => return Outcome::Err(404, "not_found".into()),
    };
    let quizzes = quizzes_of(course);
    let best = best_scores(course, &p.subject);
    let passed_all = !quizzes.is_empty() && quizzes.iter().all(|q| best.get(q["id"].as_str().unwrap_or("")).map(|(_, p)| *p).unwrap_or(false));
    if !passed_all {
        return Outcome::Err(403, "not eligible — pass every quiz first".into());
    }
    let avg: u32 = if quizzes.is_empty() { 0 } else { quizzes.iter().map(|q| best.get(q["id"].as_str().unwrap_or("")).map(|(p, _)| *p).unwrap_or(0)).sum::<u32>() / quizzes.len() as u32 };
    let line = |text: String, size: u32, bold: bool, gap: u32| pdf::Block { text, size, bold, gap_before: gap };
    let blocks = vec![
        line("Certificate of Completion".into(), 22, true, 40),
        line("This certifies that".into(), 12, false, 30),
        line(subject_email(&p.subject), 16, true, 10),
        line("has successfully completed".into(), 12, false, 20),
        line(c["title"].as_str().unwrap_or("the course").to_string(), 16, true, 10),
        line(format!("passing all {} quizzes with an average of {}%.", quizzes.len(), avg), 12, false, 24),
        line(format!("Instructor: {}", c["instructor_email"].as_str().unwrap_or("")), 11, false, 40),
    ];
    let doc = pdf::Document { title: "Certificate".to_string(), blocks };
    Outcome::File(200, "application/pdf".into(), Some("certificate.pdf".into()), pdf::render(&doc))
}

// ---- demo seed --------------------------------------------------------------

fn seed_demo(subject: &str) {
    let email = subject_email(subject);
    let c = json!({ "code": "WIT101", "title": "Intro to WIT Components", "description": "Build capabilities as composable WebAssembly components.", "instructor": subject, "instructor_email": email, "created": now() });
    let course = match records::create(COURSES, &c.to_string(), &["instructor".to_string()]) {
        Ok(r) => r.id,
        Err(_) => return,
    };
    for (i, (title, bodytext)) in [
        ("What is a component?", "A WIT world defines imports/exports; a component satisfies it. Storage, auth, and codecs are *link choices*, not code."),
        ("Composition with wac", "`wac plug` wires one component's imports to another's exports — no glue, no rebuild."),
    ]
    .iter()
    .enumerate()
    {
        let d = json!({ "course": course, "title": title, "body": bodytext, "order": i, "created": now() });
        let _ = records::create(LESSONS, &d.to_string(), &["course".to_string()]);
    }
    let quiz = json!({
        "course": course, "title": "Fundamentals quiz", "pass_mark": 60,
        "questions": [
            { "prompt": "What does a WIT world describe?", "options": ["A UI theme", "A component's imports and exports", "A database schema"], "answer": 1 },
            { "prompt": "How are two components wired together?", "options": ["wac plug", "docker compose", "a REST call"], "answer": 0 },
            { "prompt": "Where does a component's storage backend get chosen?", "options": ["Hard-coded in the component", "At link/deploy time", "By the browser"], "answer": 1 }
        ]
    });
    let _ = records::create(QUIZZES, &quiz.to_string(), &["course".to_string()]);
}

// ---- http plumbing ----------------------------------------------------------

fn hydrate(id: &str, data: &str) -> String {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    v["id"] = json!(id);
    v.to_string()
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    if let Outcome::File(code, ctype, name, bytes) = result {
        let disp = name.map(|n| format!("attachment; filename=\"{}\"", n));
        return respond(response_out, code, &ctype, disp.as_deref(), &bytes);
    }
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
        Outcome::File(..) => unreachable!(),
    };
    respond(response_out, code, "application/json", None, body.as_bytes());
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, disposition: Option<&str>, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set(&"content-disposition".to_string(), &[d.as_bytes().to_vec()]);
    }
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
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
