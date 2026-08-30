//! The `docsearch:agent` step-up gate, ported from
//! `components/doc-search-domain/e2e-stepup.sh`.
//!
//! The one gate that needed the harness to do ARITHMETIC rather than start something:
//! a correct TOTP code, computed from the secret the part provisions, is the only way
//! to prove the verification path works at all. The shell does it with `python3`'s
//! `hmac`+`hashlib.sha1`; `gatelib::totp_now` does it with `hmac`+`sha1`, which is two
//! crates added for this and RFC 6238 says which algorithm.

mod gatelib;
use gatelib::{field, totp_now, Gate};
use serde_json::{json, Value};

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}

#[test]
fn enrolling_is_not_verifying_and_a_wrong_code_cannot_log_anyone_out() {
    let Some(gate) = Gate::compose_and_start("docsearch", "doc-search-domain", &[]) else { return };
    let (_, tok) = gate.post("/test/token", None, json!({"subject":"ada"}));
    let t = field(&tok, "token");
    assert!(!t.is_empty(), "POST /test/token returned no token — the scaffold is broken, not the part");

    let mfa = || parse(&gate.get("/api/mfa", Some(&t)).1);
    let verify = |code: &str| gate.post("/api/mfa/verify", Some(&t), json!({ "code": code }));

    // --- the refusals ---------------------------------------------------------------
    let (c, _) = gate.post("/api/mfa/verify", None, json!({"code":"000000"}));
    assert_eq!(c, 401, "verifying with no bearer must be 401");
    let (c, _) = verify("000000");
    assert_eq!(
        c, 409,
        "verifying before enrolling must be 409 not_enrolled (a 401 here says the code was \
         wrong, which is not what happened)"
    );

    // --- enrol ----------------------------------------------------------------------
    let (_, enrol) = gate.json("POST", "/api/mfa/enroll", Some(&t), None);
    assert!(!enrol.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    let d = parse(&enrol);
    let secret = d["secret"].as_str().unwrap_or_default().to_string();
    assert!(secret.len() >= 16, "a TOTP secret is base32 and not short: {d}");
    let uri = d["uri"].as_str().unwrap_or_default();
    assert!(uri.starts_with("otpauth://"), "the uri must be the otpauth:// one an authenticator app can read: {uri:?}");
    assert!(uri.contains("docsearch"), "the issuer belongs in the uri: {uri:?}");

    let s = mfa();
    assert_eq!(s["enrolled"], true, "enrolled must be true after enrolling: {s}");
    assert_eq!(s["verified"], false, "enrolling is not verifying: {s}");

    // --- a wrong code: refused, and it changes nothing ------------------------------
    let (c, _) = verify("000000");
    assert_eq!(c, 401, "a wrong code must be 401 bad_code");
    assert_eq!(mfa()["verified"], false, "a wrong code verified the session");

    // --- the real thing -------------------------------------------------------------
    let code = totp_now(&secret);
    let (_, ok) = verify(&code);
    assert!(!ok.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    assert_eq!(
        parse(&ok)["verified"], true,
        "a correct TOTP code was refused — the code was computed from the secret this part \
         provisioned: {ok}"
    );
    assert_eq!(mfa()["verified"], true, "after a correct code the part must report the session verified");

    // A wrong code AFTER verifying must not undo it: that would be a logout anyone can
    // cause.
    verify("000000");
    assert_eq!(
        mfa()["verified"], true,
        "a failed attempt cleared a verified step-up — anyone who knows a subject can log them out"
    );

    // --- re-enrolling is a new authenticator, and it has not been used --------------
    gate.json("POST", "/api/mfa/enroll", Some(&t), None);
    let s = mfa();
    assert!(
        s["enrolled"] == true && s["verified"] == false,
        "a new secret has not been used yet, so verified must go back to false: {s}"
    );
}
