//! One org stores a secret, reads what it may, updates it — and a second org cannot
//! see it by any route.
//!
//! ADR-0049 proved org visibility for the CATALOGUE. Secrets are the case where being
//! wrong is expensive, so the boundary is asserted directly rather than inferred from
//! sharing a policy engine.
//!
//! The harness lives in this crate because this is where the process-spawning helpers
//! already are; the thing under test is `platform-domain`.

mod harness;
use harness::Platform;

use serde_json::{json, Value};

/// Each test gets its own control plane on its own port.
const PORT_ISOLATION: u16 = 8401;
const PORT_NO_READBACK: u16 = 8402;

#[test]
fn an_org_keeps_its_secrets_from_another_org() {
    let p = Platform::start(PORT_ISOLATION);
    let (ada, zed) = (p.user("ada"), p.user("zed"));
    assert_eq!(p.post(&ada, "/api/orgs", json!({ "name": "acme" })).0, 201);
    assert_eq!(p.post(&zed, "/api/orgs", json!({ "name": "globex" })).0, 201);

    // --- store ---
    let (code, stored) =
        p.post(&ada, "/api/secrets?org=acme", json!({ "name": "stripe", "value": "sk_v1" }));
    assert_eq!(code, 201, "{stored}");
    assert_eq!(stored["ref"], json!("vault://acme/stripe"));
    assert_eq!(stored["version"], json!(1));

    // --- read what an owner MAY read: the name, never the value ---
    let (_, listed) = p.get(&ada, "/api/secrets?org=acme");
    assert_eq!(listed["count"], json!(1));
    assert_eq!(listed["secrets"][0]["name"], json!("stripe"));
    let text = listed.to_string();
    assert!(!text.contains("sk_v1"), "a listing must never carry a value: {text}");

    // --- update: the same name again is a new version, not a second secret ---
    let (code, updated) =
        p.post(&ada, "/api/secrets?org=acme", json!({ "name": "stripe", "value": "sk_v2" }));
    assert_eq!(code, 201);
    assert_eq!(updated["version"], json!(2), "an overwrite bumps the version");
    let (_, after) = p.get(&ada, "/api/secrets?org=acme");
    assert_eq!(after["count"], json!(1), "updating must not create a second entry");

    // --- the other org, by every route it has ---

    // 1. it cannot list acme's secrets, because it is not in acme.
    let (code, denied) = p.get(&zed, "/api/secrets?org=acme");
    assert_eq!(code, 404, "membership is checked before anything else: {denied}");

    // 2. its OWN listing is empty — no bleed from a shared vault.
    let (_, mine) = p.get(&zed, "/api/secrets?org=globex");
    assert_eq!(mine["count"], json!(0), "a shared vault must not leak across orgs: {mine}");

    // 3. it cannot store INTO acme to overwrite the value.
    let (code, _) =
        p.post(&zed, "/api/secrets?org=acme", json!({ "name": "stripe", "value": "mine-now" }));
    assert_eq!(code, 404, "writing into another org must be refused");

    // 4. and acme's secret is untouched by the attempt.
    let (_, still) = p.get(&ada, "/api/secrets?org=acme");
    assert_eq!(still["count"], json!(1));

    // 5. a fetch token minted for globex cannot read an acme reference. This is the
    //    runtime path (ADR-0051) — the one a compromised node would take.
    let mint = p
        .http
        .post(p.url("/api/internal/fetch-token"))
        .header("x-platform-secret", "s3cret")
        .json(&json!({ "instance": "zed/app/gate@n1", "refs": ["vault://globex/anything"] }))
        .send()
        .unwrap();
    let token = mint.json::<Value>().unwrap()["token"].as_str().unwrap().to_string();
    let stolen = p
        .http
        .get(p.url("/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe"))
        .header("x-fetch-token", &token)
        .header("x-fetch-ts", now_secs().to_string())
        .header("x-fetch-nonce", "stolen-attempt-1")
        .send()
        .unwrap();
    assert_eq!(stolen.status().as_u16(), 403, "a token may only fetch what it was granted");
    let body = stolen.text().unwrap_or_default();
    assert!(!body.contains("sk_v"), "a refusal must not leak the value: {body}");

    // 6. `?probe=1` — the start-time existence check (ADR-0051). Same authorisation,
    //    answered from `describe`, so a host can fail closed on a broken reference at
    //    START without pulling a plaintext it may never need.
    // Every fetch carries a fresh nonce and a timestamp, exactly as a host does
    // (ADR-0071) — the platform refuses a request without them, and refuses the
    // same one twice.
    let nonce = std::cell::Cell::new(0u32);
    let probe = |token: &str, reference: &str| {
        nonce.set(nonce.get() + 1);
        let r = p
            .http
            .get(p.url(&format!("/api/internal/secret?probe=1&ref={reference}")))
            .header("x-fetch-token", token)
            .header("x-fetch-ts", now_secs().to_string())
            .header("x-fetch-nonce", format!("probe-{}", nonce.get()))
            .send()
            .unwrap();
        (r.status().as_u16(), r.text().unwrap_or_default())
    };
    let mint = p
        .http
        .post(p.url("/api/internal/fetch-token"))
        .header("x-platform-secret", "s3cret")
        .json(&json!({
            "instance": "ada/app/gate@n1",
            "refs": ["vault://acme/stripe", "vault://acme/gone"],
        }))
        .send()
        .unwrap();
    let mine = mint.json::<Value>().unwrap()["token"].as_str().unwrap().to_string();

    let (code, body) = probe(&mine, "vault%3A%2F%2Facme%2Fstripe");
    assert_eq!(code, 200, "a granted reference that exists must resolve: {body}");
    assert!(!body.contains("sk_v"), "A PROBE RETURNED THE VALUE: {body}");

    // The case the whole check exists for: granted, well-formed, and not there.
    let (code, body) = probe(&mine, "vault%3A%2F%2Facme%2Fgone");
    assert_eq!(code, 404, "a reference to nothing must not resolve: {body}");

    // And a probe is not a way around the token — same 403 as a fetch.
    let (code, _) = probe(&token, "vault%3A%2F%2Facme%2Fstripe");
    assert_eq!(code, 403, "a probe must be scoped by the same token as a fetch");

    // 7. the same request twice is refused the second time (ADR-0071). Until this
    //    existed, anyone who captured one fetch could repeat it for the rest of
    //    the token's life.
    let replay = |nonce: &str| {
        p.http
            .get(p.url("/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe"))
            .header("x-fetch-token", &mine)
            .header("x-fetch-ts", now_secs().to_string())
            .header("x-fetch-nonce", nonce)
            .send()
            .unwrap()
            .status()
            .as_u16()
    };
    assert_eq!(replay("once-only"), 200, "the first use of a nonce must work");
    assert_eq!(replay("once-only"), 409, "THE SAME REQUEST WAS ACCEPTED TWICE");
    assert_eq!(replay("fresh-one"), 200, "a fresh nonce must still work");

    // 8. and a request with no nonce at all is refused rather than waved through:
    //    an old host is one whose requests can be replayed.
    let bare = p
        .http
        .get(p.url("/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe"))
        .header("x-fetch-token", &mine)
        .send()
        .unwrap();
    assert_eq!(bare.status().as_u16(), 409, "a fetch with no nonce must be refused");

    // 9. a timestamp far outside the window is refused, which is what keeps the
    //    remembered-nonce set small.
    let stale = p
        .http
        .get(p.url("/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe"))
        .header("x-fetch-token", &mine)
        .header("x-fetch-ts", (now_secs() - 3600).to_string())
        .header("x-fetch-nonce", "an-hour-late")
        .send()
        .unwrap();
    assert_eq!(stale.status().as_u16(), 409, "an hour-old request must be refused");
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[test]
fn there_is_no_route_that_returns_a_secret_value_to_a_user() {
    // The property that makes "by reference" true rather than aspirational: a user
    // with every permission still cannot read a value back through the API. Only a
    // host holding an instance-scoped token can, and only for its own references.
    let p = Platform::start(PORT_NO_READBACK);
    let ada = p.user("ada");
    p.post(&ada, "/api/orgs", json!({ "name": "acme" }));
    p.post(&ada, "/api/secrets?org=acme", json!({ "name": "stripe", "value": "sk_secret_value" }));

    for path in ["/api/secrets?org=acme", "/api/secrets/stripe?org=acme", "/api/market?q=stripe"] {
        let (_, v) = p.get(&ada, path);
        assert!(
            !v.to_string().contains("sk_secret_value"),
            "{path} returned the value: {v}"
        );
    }
    // Even the internal route refuses a user's bearer token — it wants an
    // instance-scoped fetch token, which a user has no way to mint.
    let (code, v) = p.get(&ada, "/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe");
    assert_eq!(code, 401, "a user session must not be a fetch token: {v}");
}
