//! `meal-domain` — post a photo of a meal, get back what is on the plate.
//!
//! The HTTP front of the photo-to-nutrition pipeline, and deliberately thin: it neither looks nor
//! reads. `vision:describe` is a provider's (which model looks is a deployment choice; this app
//! names no vendor), and `nutrition:meal` owns the prompt and the parser. What is left here is the
//! HTTP shape, and everything it decides lives in a pure core that is unit-tested on the host.
//!
//! ```text
//! POST /estimate?note=post-row%20lunch[&format=json][&media-type=image/jpeg]
//!      body: the image bytes (Content-Type: image/jpeg | image/png | image/webp | image/gif)
//! 200  text/plain: the items, a total, a confidence and what to double-check
//!      (or application/json with ?format=json)
//! GET  /health  -> 200 ok
//! ```
//!
//! Plain text is the default because the first caller is an agent that reads it whole.

// `guestio`'s macros name `crate::bindings`, so it sits at the crate root. wasm32 only: the
// bindings import WASI and export the component, neither of which a host test binary has.
#[allow(warnings)]
#[cfg(target_arch = "wasm32")]
mod bindings;

/// The most image this takes. A phone photo is a few MB; the provider base64s it, which grows it
/// by a third, so this is a ceiling on what is held, not a quality policy.
pub const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;

pub const MEDIA_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/webp", "image/gif"];

// ----------------------------------------------------------------------------- the pure core

#[derive(Debug, PartialEq)]
pub enum Route {
    Health,
    Estimate { note: String, media_type: Option<String>, json: bool },
    NotFound,
    MethodNotAllowed,
}

/// `%XX` and `+` decoded; bytes that are not valid UTF-8 after decoding become U+FFFD.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

/// Which handler a request is for.
pub fn route(method: &str, path_and_query: &str) -> Route {
    let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
    match (method, path.trim_end_matches('/')) {
        ("GET", "/health") => Route::Health,
        ("POST", "/estimate") => Route::Estimate {
            note: query_param(query, "note").unwrap_or_default(),
            media_type: query_param(query, "media-type").filter(|m| !m.is_empty()),
            json: query_param(query, "format").as_deref() == Some("json"),
        },
        (_, "/estimate" | "/health") => Route::MethodNotAllowed,
        _ => Route::NotFound,
    }
}

/// The picture's own type from its first bytes: a `.jpg` that holds a webp fails far from where
/// it was named, so what the file IS wins over what the caller called it.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// The media type to send: what the bytes say, else what the caller said (header, then query).
/// A caller's claim that the bytes contradict is an error, not a guess.
pub fn media_type(
    header: Option<&str>,
    query: Option<&str>,
    bytes: &[u8],
) -> Result<&'static str, String> {
    let claimed = header
        .map(|h| h.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
        .filter(|h| h.starts_with("image/"))
        .or_else(|| query.map(|q| q.trim().to_ascii_lowercase()));
    let canon = |m: &str| {
        MEDIA_TYPES.iter().copied().find(|t| *t == if m == "image/jpg" { "image/jpeg" } else { m })
    };
    match (sniff(bytes), claimed.as_deref().map(canon)) {
        (Some(real), Some(Some(said))) if real != said => {
            Err(format!("the body is {real} but was sent as {said}"))
        }
        (Some(real), _) => Ok(real),
        (None, Some(Some(said))) => Ok(said),
        (None, Some(None)) => Err(format!(
            "unsupported media type {}: use one of {}",
            claimed.unwrap_or_default(),
            MEDIA_TYPES.join(", ")
        )),
        (None, None) => Err(format!(
            "send the image with a Content-Type of {} (heic is not supported: convert it first)",
            MEDIA_TYPES.join(", ")
        )),
    }
}

/// What went wrong, before it is an HTTP status.
#[derive(Debug, PartialEq)]
pub enum Why {
    /// The caller's request: not an image, empty, refused by the provider as malformed.
    Invalid(String),
    /// The deployment is missing a key or its quota is spent.
    Denied(String),
    /// The provider could not be reached or is busy; worth retrying.
    Unavailable(String),
    /// The provider answered and it could not be understood.
    Bad(String),
    /// It looked and said nothing.
    NoContent,
    /// It said something that is not an estimate.
    Unreadable(String),
}

pub fn http_for(w: &Why) -> (u16, String) {
    match w {
        Why::Invalid(m) => (400, m.clone()),
        Why::Denied(m) => (502, format!("the vision provider refused this deployment: {m}")),
        Why::Unavailable(m) => (503, format!("the vision provider is unavailable: {m}")),
        Why::Bad(m) => (502, format!("the vision provider's answer could not be read: {m}")),
        Why::NoContent => {
            (422, "the model looked at the picture and said nothing: try another photo".into())
        }
        Why::Unreadable(m) => (502, format!("the model's answer was not an estimate: {m}")),
    }
}

pub struct ItemView {
    pub name: String,
    pub grams: u32,
    pub kcal: u32,
    pub protein_g: u32,
    pub carbs_g: u32,
    pub fat_g: u32,
}

pub struct MealView {
    pub items: Vec<ItemView>,
    pub total_kcal: u32,
    pub total_protein_g: u32,
    pub total_carbs_g: u32,
    pub total_fat_g: u32,
    pub confidence: &'static str,
    pub unsure: Vec<String>,
    pub notes: String,
}

/// The estimate as a small model reads it whole: one line per item, a total, and what to check.
pub fn render_text(m: &MealView) -> String {
    let mut out = format!("confidence: {}\n", m.confidence);
    for i in &m.items {
        out.push_str(&format!(
            "- {}: ~{} g | {} kcal | P {} | C {} | F {}\n",
            i.name, i.grams, i.kcal, i.protein_g, i.carbs_g, i.fat_g
        ));
    }
    out.push_str(&format!(
        "total: ~{} kcal | P {} g | C {} g | F {} g\n",
        m.total_kcal, m.total_protein_g, m.total_carbs_g, m.total_fat_g
    ));
    if !m.unsure.is_empty() {
        out.push_str(&format!("check: {}\n", m.unsure.join("; ")));
    }
    if !m.notes.is_empty() {
        out.push_str(&format!("note: {}\n", m.notes));
    }
    out.push_str(
        "(an estimate from a photo: portions are guesses. Ask for grams if it matters.)\n",
    );
    out
}

pub fn render_json(m: &MealView) -> String {
    serde_json::json!({
        "items": m.items.iter().map(|i| serde_json::json!({
            "name": i.name, "grams": i.grams, "kcal": i.kcal,
            "protein_g": i.protein_g, "carbs_g": i.carbs_g, "fat_g": i.fat_g,
        })).collect::<Vec<_>>(),
        "total": {
            "kcal": m.total_kcal, "protein_g": m.total_protein_g,
            "carbs_g": m.total_carbs_g, "fat_g": m.total_fat_g,
        },
        "confidence": m.confidence,
        "unsure": m.unsure,
        "notes": m.notes,
    })
    .to_string()
}

// -------------------------------------------------------------------------------- the guest

#[cfg(target_arch = "wasm32")]
mod guest {
    use super::*;
    use crate::bindings::exports::wasi::http::incoming_handler::Guest;
    use crate::bindings::nutrition::meal::estimator as meal;
    use crate::bindings::vision::describe::describer as vision;
    use crate::bindings::wasi::http::types::{
        Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
    };

    guestio::guest_write_all!();
    guestio::guest_read_body!(MAX_IMAGE_BYTES);

    struct Component;

    fn respond(out: ResponseOutparam, status: u16, content_type: &str, body: &str) {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
        let response = OutgoingResponse::new(headers);
        let _ = response.set_status_code(status);
        let out_body = response.body().expect("outgoing body");
        ResponseOutparam::set(out, Ok(response));
        let stream = out_body.write().expect("write stream");
        assert!(write_all(&stream, body.as_bytes()), "write body");
        drop(stream);
        OutgoingBody::finish(out_body, None).expect("finish body");
    }

    fn text(out: ResponseOutparam, status: u16, body: &str) {
        respond(out, status, "text/plain; charset=utf-8", &format!("{body}\n"));
    }

    fn header(request: &IncomingRequest, name: &str) -> Option<String> {
        request
            .headers()
            .get(name)
            .into_iter()
            .next()
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    fn view(e: meal::Estimate) -> MealView {
        MealView {
            items: e
                .items
                .into_iter()
                .map(|i| ItemView {
                    name: i.name,
                    grams: i.grams,
                    kcal: i.kcal,
                    protein_g: i.protein_g,
                    carbs_g: i.carbs_g,
                    fat_g: i.fat_g,
                })
                .collect(),
            total_kcal: e.total_kcal,
            total_protein_g: e.total_protein_g,
            total_carbs_g: e.total_carbs_g,
            total_fat_g: e.total_fat_g,
            confidence: match e.confidence {
                meal::Confidence::Low => "low",
                meal::Confidence::Medium => "medium",
                meal::Confidence::High => "high",
            },
            unsure: e.unsure,
            notes: e.notes,
        }
    }

    fn describe(image: &[u8], media_type: &str, prompt: &str) -> Result<String, Why> {
        vision::describe(image, media_type, prompt).map_err(|e| match e {
            vision::DescribeError::InvalidRequest(m) => Why::Invalid(m),
            vision::DescribeError::ProviderDenied(m) => Why::Denied(m),
            vision::DescribeError::ProviderUnavailable(m) => Why::Unavailable(m),
            vision::DescribeError::BadResponse(m) => Why::Bad(m),
            vision::DescribeError::NoContent => Why::NoContent,
        })
    }

    fn estimate(image: &[u8], media_type: &str, note: &str) -> Result<MealView, Why> {
        let answer = describe(image, media_type, &meal::prompt(note))?;
        meal::parse(&answer).map(view).map_err(|e| {
            Why::Unreadable(match e {
                meal::ParseError::NotJson(m) => format!("it was not JSON ({m})"),
                meal::ParseError::NoItems => "it listed nothing to eat".to_string(),
                meal::ParseError::Malformed(m) => m,
            })
        })
    }

    impl Guest for Component {
        fn handle(request: IncomingRequest, out: ResponseOutparam) {
            let method = match request.method() {
                Method::Get => "GET",
                Method::Post => "POST",
                _ => "OTHER",
            };
            let target = request.path_with_query().unwrap_or_default();
            match route(method, &target) {
                Route::Health => text(out, 200, "ok"),
                Route::NotFound => text(out, 404, "not found: POST /estimate"),
                Route::MethodNotAllowed => {
                    text(out, 405, "use POST /estimate with the image as the body")
                }
                Route::Estimate { note, media_type: claimed, json } => {
                    let Ok(bytes) = read_body(&request) else {
                        return text(
                            out,
                            413,
                            &format!("the image is larger than {} MiB", MAX_IMAGE_BYTES >> 20),
                        );
                    };
                    if bytes.is_empty() {
                        return text(out, 400, "the body is empty: send the image bytes");
                    }
                    let mt = match media_type(
                        header(&request, "content-type").as_deref(),
                        claimed.as_deref(),
                        &bytes,
                    ) {
                        Ok(mt) => mt,
                        Err(m) => return text(out, 415, &m),
                    };
                    match estimate(&bytes, mt, note.trim()) {
                        Ok(m) if json => respond(out, 200, "application/json", &render_json(&m)),
                        Ok(m) => text(out, 200, render_text(&m).trim_end()),
                        Err(w) => {
                            let (status, msg) = http_for(&w);
                            text(out, status, &msg)
                        }
                    }
                }
            }
        }
    }

    crate::bindings::export!(Component with_types_in crate::bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: [u8; 4] = [0xff, 0xd8, 0xff, 0xe0];
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    #[test]
    fn requests_are_routed_and_the_query_is_decoded() {
        assert_eq!(route("GET", "/health"), Route::Health);
        assert_eq!(route("GET", "/health/"), Route::Health);
        assert_eq!(
            route("POST", "/estimate?note=post-row%20lunch+x&format=json&media-type=image%2Fpng"),
            Route::Estimate {
                note: "post-row lunch x".into(),
                media_type: Some("image/png".into()),
                json: true
            }
        );
        assert_eq!(
            route("POST", "/estimate"),
            Route::Estimate { note: String::new(), media_type: None, json: false }
        );
        assert_eq!(route("GET", "/estimate"), Route::MethodNotAllowed);
        assert_eq!(route("POST", "/health"), Route::MethodNotAllowed);
        assert_eq!(route("GET", "/nope"), Route::NotFound);
    }

    #[test]
    fn percent_decoding_survives_a_broken_escape_and_a_non_utf8_byte() {
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
    }

    #[test]
    fn what_the_bytes_are_beats_what_the_caller_called_them() {
        assert_eq!(sniff(&JPEG), Some("image/jpeg"));
        assert_eq!(sniff(&PNG), Some("image/png"));
        assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff(b"hello"), None);
        // the header and the bytes agree, or only one is given
        assert_eq!(media_type(Some("image/jpeg; charset=binary"), None, &JPEG), Ok("image/jpeg"));
        assert_eq!(media_type(None, None, &PNG), Ok("image/png"));
        assert_eq!(
            media_type(Some("image/jpg"), None, &JPEG),
            Ok("image/jpeg"),
            "the common misspelling"
        );
        // a contradiction is an error, not a guess
        assert!(media_type(Some("image/png"), None, &JPEG)
            .unwrap_err()
            .contains("image/jpeg but was sent as image/png"));
        // unknown bytes: trust a supported claim, refuse an unsupported or missing one
        assert_eq!(media_type(None, Some("image/webp"), b"???"), Ok("image/webp"));
        assert!(media_type(None, Some("image/heic"), b"???").unwrap_err().contains("unsupported"));
        assert!(media_type(Some("application/octet-stream"), None, b"???")
            .unwrap_err()
            .contains("Content-Type"));
    }

    #[test]
    fn each_failure_is_the_status_a_caller_can_act_on() {
        assert_eq!(http_for(&Why::Invalid("x".into())).0, 400);
        assert_eq!(
            http_for(&Why::Denied("no key".into())),
            (502, "the vision provider refused this deployment: no key".into())
        );
        assert_eq!(http_for(&Why::Unavailable("slow".into())).0, 503);
        assert_eq!(http_for(&Why::Bad("x".into())).0, 502);
        assert_eq!(http_for(&Why::NoContent).0, 422);
        assert_eq!(http_for(&Why::Unreadable("not JSON".into())).0, 502);
    }

    fn meal() -> MealView {
        MealView {
            items: vec![
                ItemView {
                    name: "chicken".into(),
                    grams: 150,
                    kcal: 320,
                    protein_g: 30,
                    carbs_g: 0,
                    fat_g: 21,
                },
                ItemView {
                    name: "rice".into(),
                    grams: 120,
                    kcal: 155,
                    protein_g: 3,
                    carbs_g: 34,
                    fat_g: 0,
                },
            ],
            total_kcal: 475,
            total_protein_g: 33,
            total_carbs_g: 34,
            total_fat_g: 21,
            confidence: "medium",
            unsure: vec!["cooking oil".into(), "rice was cooked in broth?".into()],
            notes: "rough".into(),
        }
    }

    #[test]
    fn the_text_form_is_what_an_agent_reads_whole() {
        let t = render_text(&meal());
        assert!(
            t.starts_with("confidence: medium\n- chicken: ~150 g | 320 kcal | P 30 | C 0 | F 21\n"),
            "{t}"
        );
        assert!(t.contains("total: ~475 kcal | P 33 g | C 34 g | F 21 g\n"));
        assert!(t.contains("check: cooking oil; rice was cooked in broth?\n"));
        assert!(t.contains("note: rough\n") && t.ends_with("Ask for grams if it matters.)\n"));
        let mut bare = meal();
        bare.unsure.clear();
        bare.notes.clear();
        let t = render_text(&bare);
        assert!(!t.contains("check:") && !t.contains("note:"));
    }

    #[test]
    fn the_json_form_is_valid_and_carries_the_same_numbers() {
        let v: serde_json::Value = serde_json::from_str(&render_json(&meal())).unwrap();
        assert_eq!(v["total"]["kcal"], 475);
        assert_eq!(v["items"][1]["name"], "rice");
        assert_eq!(v["confidence"], "medium");
        assert_eq!(v["unsure"][0], "cooking oil");
    }
}
