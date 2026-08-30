//! The `docsearch:agent` library gate, ported from
//! `components/doc-search-domain/e2e-library.sh`.
//!
//! `e2e-answer.sh` wants a model on :8787. `e2e-stepup.sh` needs a TOTP code computed
//! from the secret the part provisions, which the shell gate does with `python3`'s
//! `hmac`+`hashlib.sha1`; the reconciler has `sha2` and not `sha1`, so that one stays
//! a shell gate until somebody decides whether an HMAC-SHA1 dependency is worth it.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const CRATE: &str = "doc-search-domain";

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut b = json!({ "subject": subject });
    if let Some(s) = scopes {
        b["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, b).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}

#[test]
fn a_document_is_findable_by_its_body_and_the_tag_filter_is_the_index() {
    let Some(gate) = Gate::compose_and_start("docsearch", CRATE, &[]) else { return };
    let w = token(&gate, "ada", None);
    let doc = json!({
        "title":"Rotating the signing key",
        "text":"The webhook signer keeps two keys so an in-flight request signed with the old one still verifies during the overlap window.",
        "tag":"security"});

    // --- the refusals ---------------------------------------------------------------
    let (c, _) = gate.post("/api/docs", None, doc.clone());
    assert_eq!(c, 401, "filing a document with no bearer must be 401");
    let ro = token(&gate, "reader", Some(json!(["docs:read"])));
    let (c, _) = gate.post("/api/docs", Some(&ro), doc.clone());
    assert_eq!(c, 403, "a token with only docs:read must be 403 on a write");
    let (c, _) = gate.post("/api/docs", Some(&w), json!({"title":"","text":"x","tag":"y"}));
    assert_eq!(c, 400, "an empty title must be 400 invalid_doc");

    // --- a document goes in, and is findable by its BODY ---------------------------
    let (_, created) = gate.post("/api/docs", Some(&w), doc);
    let id = field(&created, "id");
    assert!(!id.is_empty(), "POST /api/docs returned no id");

    let raw = gate.stored("doc", &id);
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    assert_eq!(
        d["title"], "Rotating the signing key",
        "the stored document is not what was filed: {d}"
    );
    assert_eq!(d["tag"], "security", "{d}");
    assert!(d["text"].as_str().unwrap_or_default().contains("overlap window"), "{d}");

    // "overlap" appears only in the body. A title match cannot find this.
    let (_, raw) = gate.get("/api/search?q=overlap", Some(&w));
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    let hits = d["hits"].as_array().cloned().unwrap_or_default();
    assert!(!hits.is_empty(), "no hits for a word that is in the indexed text: {d}");
    let ids: Vec<&str> = hits.iter().filter_map(|h| h["id"].as_str()).collect();
    assert!(ids.contains(&id.as_str()), "the document just filed is not among the hits: {ids:?}");
    let h = hits
        .iter()
        .find(|h| h["id"] == id.as_str())
        .expect("the hit that was just asserted present");
    assert_eq!(
        h["title"], "Rotating the signing key",
        "a hit must carry the title from the store — a caller cannot use a list of ULIDs: {h}"
    );
    assert!(h["score"].is_number(), "a hit must carry the index's score: {h}");
    let scores: Vec<f64> = hits.iter().filter_map(|x| x["score"].as_f64()).collect();
    let mut desc = scores.clone();
    desc.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(scores, desc, "hits must be ordered by descending score: {scores:?}");

    // The tag filter is the index's, not a filter applied afterwards to everything.
    let (_, raw) = gate.get("/api/search?q=overlap&tag=ops", Some(&w));
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    assert_eq!(parse(&raw)["hits"], json!([]), "tag=ops must not match a security document: {raw}");

    // A question the library cannot answer is an empty list, not an error: an empty
    // library and a bad question are the same shape to a caller.
    let (_, raw) = gate.get("/api/search?q=sourdough", Some(&w));
    assert_eq!(parse(&raw)["hits"], json!([]), "a query matching nothing answered hits");

    let (c, _) = gate.get("/api/docs/nope", Some(&w));
    assert_eq!(c, 404, "an unknown document id must be 404");
}
