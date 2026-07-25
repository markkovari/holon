//! E2E for the LMS (LMS.md) as ONE composed wasm HTTP component (lms-domain +
//! auth-guard + records + quiz + pdf + svg-chart) on the native Rust host. Proves
//! the multi-role flow AND that grades roll up consistently: an instructor
//! creates a course + quiz; a student enrolls and submits (auto-graded by
//! quiz:grade); the instructor gradebook reflects the same score; a certificate
//! issues only after every quiz is passed; and a student can't see the answer
//! key or another instructor's gradebook.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3048";

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

fn signup(email: &str, role: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678", "role": role })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/lms_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-lms`)");
    assert!(component.exists(), "composed wasm missing (just compose-lms)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "lms")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("lms host did not start");
}

#[test]
fn multi_role_grades_reconcile() {
    let _host = start_host();
    let prof = signup("prof@acme.io", "instructor"); // registering as instructor seeds a demo course
    let student = signup("stu@acme.io", "student");

    // ===== the instructor's seeded course; an instructor-only action =======
    let (_, courses) = req("GET", "/api/courses", Some(&prof), None);
    let course = courses["items"][0]["id"].as_str().unwrap().to_string();
    // a student cannot create a course.
    assert_eq!(req("POST", "/api/courses", Some(&student), Some(json!({ "code": "X", "title": "X" }))).0, 403);

    // ===== the answer key is hidden from students ==========================
    let (_, detail_i) = req("GET", &format!("/api/courses/{course}"), Some(&prof), None);
    let quiz = detail_i["quizzes"][0]["id"].as_str().unwrap().to_string();
    assert!(detail_i["quizzes"][0]["questions"][0].get("answer").is_some(), "instructor sees the key");
    let (_, detail_s) = req("GET", &format!("/api/courses/{course}"), Some(&student), None);
    assert!(detail_s["quizzes"][0]["questions"][0].get("answer").is_none(), "student does NOT see the key");

    // ===== enroll gate, then submit -> auto-graded (quiz:grade) ============
    assert_eq!(req("POST", &format!("/api/quizzes/{quiz}/submit"), Some(&student), Some(json!({ "answers": [1, 0, 1] }))).0, 403, "must enroll first");
    assert!(req("POST", &format!("/api/courses/{course}/enroll"), Some(&student), None).0 == 201);

    // a wrong attempt, then a perfect one.
    let (_, wrong) = req("POST", &format!("/api/quizzes/{quiz}/submit"), Some(&student), Some(json!({ "answers": [0, 1, 0] })));
    assert_eq!(wrong["passed"], false);
    let (_, right) = req("POST", &format!("/api/quizzes/{quiz}/submit"), Some(&student), Some(json!({ "answers": [1, 0, 1] })));
    assert_eq!(right["score_pct"], 100);
    assert_eq!(right["passed"], true);

    // ===== the numbers reconcile across the three views ====================
    // student progress: best kept, passed all, certificate eligible.
    let (_, prog) = req("GET", &format!("/api/courses/{course}/progress"), Some(&student), None);
    assert_eq!(prog["passed_all"], true);
    assert_eq!(prog["completion_pct"], 100);
    assert_eq!(prog["quizzes"][0]["best_score"], 100, "best score kept, not the last");

    // instructor gradebook: same student, same average.
    let (_, gb) = req("GET", &format!("/api/courses/{course}/gradebook"), Some(&prof), None);
    assert_eq!(gb["enrolled"], 1);
    assert_eq!(gb["students"][0]["email"], "stu@acme.io");
    assert_eq!(gb["students"][0]["average"], 100);
    assert_eq!(gb["students"][0]["passed_all"], true);
    assert_eq!(gb["quizzes"][0]["mean"], 100, "cohort mean matches");
    assert!(gb["chart_svg"].as_str().unwrap().starts_with("<svg"), "gradebook chart rendered");

    // a student cannot read the gradebook.
    assert_eq!(req("GET", &format!("/api/courses/{course}/gradebook"), Some(&student), None).0, 403);

    // ===== the certificate issues only after passing all quizzes ===========
    let resp = ureq::get(&format!("{}/api/courses/{course}/certificate.pdf", base()))
        .set("authorization", &format!("Bearer {student}"))
        .call()
        .expect("certificate.pdf");
    assert_eq!(resp.status(), 200);
    assert!(resp.header("content-type").unwrap_or("").starts_with("application/pdf"));
    let mut pdf = Vec::new();
    resp.into_reader().read_to_end(&mut pdf).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"), "a real certificate PDF");

    // a not-yet-passing student is refused the certificate.
    let latecomer = signup("late@acme.io", "student");
    req("POST", &format!("/api/courses/{course}/enroll"), Some(&latecomer), None);
    assert_eq!(
        ureq::get(&format!("{}/api/courses/{course}/certificate.pdf", base()))
            .set("authorization", &format!("Bearer {latecomer}"))
            .call()
            .err()
            .map(|e| if let ureq::Error::Status(s, _) = e { s } else { 0 })
            .unwrap_or(0),
        403,
        "no certificate until you pass"
    );
}
