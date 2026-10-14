//! A photo sent to an agent, read by a service: the plain bridge between the inbox and an
//! analyzer such as the `meal` app's `POST /estimate` (a wasm component, ADR-0095).
//!
//! The runtime does not know what the service reads in the picture. It posts the image with its
//! media type and the person's words (`?note=`), and hands the text reply back to be put in front
//! of the agent. A failure is the agent's to tell the person, so it comes back as text, not as a
//! reason to drop the photo.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

/// Above this a phone photo is shrunk first: the model does not need it and the service has a
/// ceiling.
const SHRINK_ABOVE: usize = 1_500_000;

/// The media type a file name says. `heic` is converted before it is sent (see `prepare`).
pub fn media_type_of(file: &str) -> &'static str {
    match file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("heic") => "image/heic",
        _ => "image/jpeg",
    }
}

/// The picture as the service takes it: an iPhone's HEIC converted to JPEG and a large photo
/// shrunk, both with the system `sips`, which is there on macOS and nowhere else. Without it a
/// HEIC cannot be sent (and the service says so), and a large photo goes as it is.
pub fn prepare(bytes: Vec<u8>, media_type: &'static str) -> (Vec<u8>, &'static str) {
    let heic = media_type == "image/heic";
    if !heic && bytes.len() <= SHRINK_ABOVE {
        return (bytes, media_type);
    }
    let converted = (|| {
        let dir = tempfile::tempdir().ok()?;
        let (src, out) = (dir.path().join("in"), dir.path().join("out.jpg"));
        std::fs::File::create(&src).ok()?.write_all(&bytes).ok()?;
        let ok = Command::new("/usr/bin/sips")
            .args(["-Z", "1600", "-s", "format", "jpeg"])
            .arg(&src)
            .arg("--out")
            .arg(&out)
            .output()
            .ok()?
            .status
            .success();
        ok.then(|| std::fs::read(&out).ok()).flatten()
    })();
    match converted {
        Some(jpeg) => (jpeg, "image/jpeg"),
        None => (bytes, media_type),
    }
}

/// Posts the photo to `url` and returns the service's text. `Err` carries a sentence the agent can
/// pass on.
pub fn analyze(url: &str, bytes: Vec<u8>, media_type: &str, note: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        // a redirect could leave the host the agent was allowed to reach
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("holon-agent/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(url).header("content-type", media_type).body(bytes);
    if !note.trim().is_empty() {
        req = req.query(&[("note", note.trim())]);
    }
    let r = req.send().map_err(|e| format!("could not reach the photo service: {e}"))?;
    let status = r.status();
    let text = r.text().unwrap_or_default();
    if status.is_success() {
        Ok(text.trim().to_string())
    } else {
        let why: String = text.trim().chars().take(300).collect();
        Err(format!("the photo service answered {status}: {why}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-request HTTP server on an ephemeral port: what it was sent, and what it answers.
    fn serve(
        status: u16,
        reply: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<(String, String, Vec<u8>)>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/estimate", server.server_addr().to_ip().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok(mut req) = server.recv() {
                let ct = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("content-type"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let mut body = Vec::new();
                std::io::Read::read_to_end(req.as_reader(), &mut body).unwrap();
                let _ = tx.send((req.url().to_string(), ct, body));
                let _ =
                    req.respond(tiny_http::Response::from_string(reply).with_status_code(status));
            }
        });
        (url, rx)
    }

    #[test]
    fn the_photo_goes_with_its_type_and_the_persons_words_and_the_text_comes_back() {
        let (url, rx) = serve(200, "confidence: medium\n- rice\n");
        let got = analyze(&url, vec![1, 2, 3], "image/png", "  post-row lunch ").unwrap();
        assert_eq!(got, "confidence: medium\n- rice");
        let (target, content_type, body) = rx.recv().unwrap();
        assert_eq!(target, "/estimate?note=post-row+lunch");
        assert_eq!((content_type.as_str(), body), ("image/png", vec![1, 2, 3]));
    }

    #[test]
    fn no_words_means_no_note_parameter() {
        let (url, rx) = serve(200, "ok");
        analyze(&url, vec![9], "image/jpeg", "   ").unwrap();
        assert_eq!(rx.recv().unwrap().0, "/estimate");
    }

    #[test]
    fn a_refusal_or_an_unreachable_service_is_a_sentence_for_the_agent() {
        let (url, _rx) = serve(415, "the body is image/jpeg but was sent as image/png");
        let e = analyze(&url, vec![1], "image/png", "").unwrap_err();
        assert!(e.contains("415") && e.contains("sent as image/png"), "{e}");
        let gone = analyze("http://127.0.0.1:1/estimate", vec![1], "image/png", "").unwrap_err();
        assert!(gone.starts_with("could not reach the photo service"), "{gone}");
    }

    #[test]
    fn a_file_name_gives_its_media_type_and_a_small_photo_is_left_alone() {
        assert_eq!(media_type_of("1-lunch.JPG"), "image/jpeg");
        assert_eq!(media_type_of("a.png"), "image/png");
        assert_eq!(media_type_of("a.webp"), "image/webp");
        assert_eq!(media_type_of("a.heic"), "image/heic");
        assert_eq!(prepare(vec![5; 10], "image/png"), (vec![5; 10], "image/png"));
    }
}
