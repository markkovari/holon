//! Held-out specification gates for the 19 scaffold domains.
//!
//! Replaces the legacy `components/*/gate.sh` shell scripts with native Rust integration tests
//! using `gatelib::Gate`. Each domain is composed in-process via `plug::compose_to`, hosted on
//! an OS-assigned ephemeral port (127.0.0.1:0), and tested in parallel via `cargo test`.

mod gatelib;
use gatelib::Gate;

fn test_scaffold_domain(domain: &str) {
    let Some(gate) = Gate::compose_and_start(domain, domain, &[]) else {
        return;
    };
    let scenario_path = format!("components/{domain}/scenario.hurl");
    gate.run_scenario(&scenario_path);
}

#[test]
fn gate_ats_domain() {
    test_scaffold_domain("ats-domain");
}

#[test]
fn gate_billing_domain() {
    test_scaffold_domain("billing-domain");
}

#[test]
fn gate_book_lending_domain() {
    test_scaffold_domain("book-lending-domain");
}

#[test]
fn gate_complaint_escalation_domain() {
    test_scaffold_domain("complaint-escalation-domain");
}

#[test]
fn gate_crm_domain() {
    test_scaffold_domain("crm-domain");
}

#[test]
fn gate_expense_domain() {
    test_scaffold_domain("expense-domain");
}

#[test]
fn gate_feedback_domain() {
    test_scaffold_domain("feedback-domain");
}

#[test]
fn gate_flag_console_domain() {
    test_scaffold_domain("flag-console-domain");
}

#[test]
fn gate_incident_response_domain() {
    test_scaffold_domain("incident-response-domain");
}

#[test]
fn gate_inspection_domain() {
    test_scaffold_domain("inspection-domain");
}

#[test]
fn gate_invoice_approval_domain() {
    test_scaffold_domain("invoice-approval-domain");
}

#[test]
fn gate_oncall_schedule_domain() {
    test_scaffold_domain("oncall-schedule-domain");
}

#[test]
fn gate_parking_domain() {
    test_scaffold_domain("parking-domain");
}

#[test]
fn gate_referral_tracker_domain() {
    test_scaffold_domain("referral-tracker-domain");
}

#[test]
fn gate_room_booking_domain() {
    test_scaffold_domain("room-booking-domain");
}

#[test]
fn gate_survey_domain() {
    test_scaffold_domain("survey-domain");
}

#[test]
fn gate_ticket_triage_domain() {
    test_scaffold_domain("ticket-triage-domain");
}

#[test]
fn gate_timesheet_domain() {
    test_scaffold_domain("timesheet-domain");
}

#[test]
fn gate_volunteer_shift_domain() {
    test_scaffold_domain("volunteer-shift-domain");
}

#[test]
fn gate_jev_router() {
    let mock_script = r#"{"rules": [{"when": "printer", "selected": "helpdesk-domain", "confidence": 0.94}, {"when": "appointment", "selected": "clinic-domain", "confidence": 0.9}, {"when": "*", "selected": "helpdesk-domain", "confidence": 0.2, "flat": true}]}"#;
    let config = ["mock-model=mock-jev-1", &format!("mock-script={mock_script}")];
    let Some(gate) = Gate::compose_and_start_with_overrides(
        "jev-router",
        "jev-router",
        &config,
        &[("jev:decision/decision", "mock-jev-provider")],
    ) else {
        return;
    };
    gate.run_scenario("components/jev-router/scenario.hurl");
}

#[test]
fn gate_card_identify() {
    let manifest = gatelib::repo_root().join("components/Cargo.toml");
    let out = std::process::Command::new("cargo")
        .args([
            "test",
            "--release",
            "--manifest-path",
            manifest.to_str().unwrap(),
            "-p",
            "card-identify",
            "--test",
            "guess",
        ])
        .output()
        .expect("run card-identify guess test");
    assert!(
        out.status.success(),
        "card-identify guess tests failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
