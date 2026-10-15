//! Files that come with a run: a photo Mark sent, read from the agent's own inbox and put in
//! front of its model with the message.
//!
//! The inbox is `workspaces/<agent>/inbox/`, where the chat bridge puts what it was sent. A run
//! names the files (`Cause::attachments`) and nothing else: a name is checked to be a plain file
//! name, so an attachment cannot reach outside the inbox, and another agent's inbox is not
//! reachable at all. A file that cannot be read does not fail the run; the model is told, so it
//! can say so instead of answering as if it had looked.

use std::io::Write;
use std::process::Command;

use crate::model::Image;
use crate::store::Store;

/// More than this is a mistake or an abuse: every picture is sent again with every step of a run.
pub const MAX_ATTACHMENTS: usize = 4;
/// A photo bigger than this is refused outright.
const MAX_BYTES: u64 = 20 << 20;
/// Above this a photo is shrunk first: the model does not need a phone's full resolution, and it
/// is sent again on every step of the run.
const SHRINK_ABOVE: usize = 1_500_000;

/// The media type of a photo by its file name; `None` for anything that is not one.
pub fn media_type_of(file: &str) -> Option<&'static str> {
    match file.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "heic" => Some("image/heic"),
        _ => None,
    }
}

/// A plain file name: letters, digits, dots, dashes and underscores, not starting with a dot.
pub fn valid_name(file: &str) -> bool {
    !file.is_empty()
        && file.len() <= 120
        && !file.starts_with('.')
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn inbox_file(store: &Store, agent: &str, file: &str) -> Option<std::path::PathBuf> {
    if !valid_name(file) {
        return None;
    }
    let path = store.workspace(agent).ok()?.join("inbox").join(file);
    path.is_file().then_some(path)
}

/// Whether `file` is in `agent`'s inbox (and is a name that could be).
pub fn exists(store: &Store, agent: &str, file: &str) -> bool {
    inbox_file(store, agent, file).is_some()
}

/// An iPhone's HEIC converted to JPEG, and a large photo shrunk, with the system `sips` (macOS;
/// nowhere else). `None` when it could not: a HEIC is then unusable, a large photo goes as it is.
fn sips(bytes: &[u8], longest: u32) -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().ok()?;
    let (src, out) = (dir.path().join("in"), dir.path().join("out.jpg"));
    std::fs::File::create(&src).ok()?.write_all(bytes).ok()?;
    let done = Command::new("/usr/bin/sips")
        .args(["-Z", &longest.to_string(), "-s", "format", "jpeg"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .output()
        .ok()?
        .status
        .success();
    done.then(|| std::fs::read(&out).ok()).flatten()
}

/// A photo from the inbox as a JPEG whose longest side is at most `longest` pixels, ready for a
/// service with strict picture rules (Jev takes only PNG and JPEG, 256 to 1568 px a side). Off a
/// Mac, with no `sips`, a PNG or JPEG goes as it is and anything else is refused.
pub fn jpeg_of(store: &Store, agent: &str, file: &str, longest: u32) -> Result<Image, String> {
    let path =
        inbox_file(store, agent, file).ok_or_else(|| format!("`{file}` is not in your inbox"))?;
    let media_type = media_type_of(file).ok_or_else(|| format!("`{file}` is not a photo"))?;
    if path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(format!("`{file}` is too large"));
    }
    let bytes = std::fs::read(&path).map_err(|_| format!("`{file}` could not be read"))?;
    shrunk(bytes, media_type, longest).ok_or_else(|| format!("`{file}` could not be converted"))
}

/// `bytes` as a JPEG of at most `longest` pixels, or as they are when already a PNG or JPEG and
/// `sips` is not there.
pub fn shrunk(bytes: Vec<u8>, media_type: &str, longest: u32) -> Option<Image> {
    match sips(&bytes, longest) {
        Some(data) => Some(Image { media_type: "image/jpeg".into(), data }),
        None if matches!(media_type, "image/jpeg" | "image/png") => {
            Some(Image { media_type: media_type.into(), data: bytes })
        }
        None => None,
    }
}

/// The pictures for these files, and a sentence for each one that could not be used.
pub fn load(store: &Store, agent: &str, files: &[String]) -> (Vec<Image>, Vec<String>) {
    let (mut images, mut problems) = (Vec::new(), Vec::new());
    for file in files.iter().take(MAX_ATTACHMENTS) {
        let Some(path) = inbox_file(store, agent, file) else {
            problems.push(format!("`{file}` is not in your inbox"));
            continue;
        };
        let Some(media_type) = media_type_of(file) else {
            problems.push(format!("`{file}` is not a photo"));
            continue;
        };
        if path.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            problems.push(format!("`{file}` is too large"));
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            problems.push(format!("`{file}` could not be read"));
            continue;
        };
        let image = if media_type == "image/heic" {
            match sips(&bytes, 1600) {
                Some(jpeg) => Image { media_type: "image/jpeg".into(), data: jpeg },
                None => {
                    problems.push(format!("`{file}` is a HEIC photo and could not be converted"));
                    continue;
                }
            }
        } else if bytes.len() > SHRINK_ABOVE {
            match sips(&bytes, 1600) {
                Some(jpeg) => Image { media_type: "image/jpeg".into(), data: jpeg },
                None => Image { media_type: media_type.into(), data: bytes },
            }
        } else {
            Image { media_type: media_type.into(), data: bytes }
        };
        images.push(image);
    }
    if files.len() > MAX_ATTACHMENTS {
        problems.push(format!("only the first {MAX_ATTACHMENTS} attachments were sent"));
    }
    (images, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(agent: &str, files: &[(&str, Vec<u8>)]) -> (Store, std::path::PathBuf) {
        let d = crate::testutil::dir("attach");
        let store = Store::open(&d).unwrap();
        let inbox = store.workspace(agent).unwrap().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        for (name, bytes) in files {
            std::fs::write(inbox.join(name), bytes).unwrap();
        }
        (store, inbox)
    }

    #[test]
    fn only_plain_file_names_and_photo_extensions_are_accepted() {
        for ok in ["1-lunch.jpg", "a_b.PNG", "x.webp", "y.gif", "z.heic", "17.0-meal.jpeg"] {
            assert!(valid_name(ok) && media_type_of(ok).is_some(), "{ok}");
        }
        for bad in
            ["", ".hidden.jpg", "../a.jpg", "a/b.jpg", "a\\b.jpg", "a b.jpg", "caf\u{e9}.jpg"]
        {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(media_type_of("notes.pdf"), None);
        assert_eq!(media_type_of("noext"), None);
        assert_eq!(media_type_of("A.JPG"), Some("image/jpeg"));
    }

    #[test]
    fn a_photo_in_the_inbox_is_loaded_with_its_type() {
        let (store, _) = store_with("chef", &[("1-a.png", vec![1, 2, 3])]);
        let (images, problems) = load(&store, "chef", &["1-a.png".to_string()]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(images, vec![Image { media_type: "image/png".into(), data: vec![1, 2, 3] }]);
        assert!(exists(&store, "chef", "1-a.png") && !exists(&store, "chef", "nope.png"));
    }

    #[test]
    fn what_cannot_be_used_is_a_sentence_for_the_model_not_a_failed_run() {
        let (store, _) = store_with("chef", &[("notes.pdf", vec![1]), ("ok.jpg", vec![9])]);
        let files: Vec<String> = ["missing.jpg", "notes.pdf", "../x.jpg", "ok.jpg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (images, problems) = load(&store, "chef", &files);
        assert_eq!(images.len(), 1, "the one that could be used still goes");
        assert!(
            problems.iter().any(|p| p.contains("missing.jpg") && p.contains("not in your inbox")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("notes.pdf") && p.contains("not a photo")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("../x.jpg")),
            "a name that leaves the inbox is refused"
        );
    }

    #[test]
    fn one_agent_cannot_read_another_agents_inbox() {
        let (store, _) = store_with("chef", &[("secret.jpg", vec![7])]);
        assert!(exists(&store, "chef", "secret.jpg"));
        assert!(!exists(&store, "coach", "secret.jpg"), "coach's inbox is a different folder");
        assert_eq!(load(&store, "coach", &["secret.jpg".to_string()]).0.len(), 0);
    }

    #[test]
    fn more_than_the_limit_sends_the_first_few_and_says_so() {
        let names: Vec<String> = (0..6).map(|i| format!("{i}.png")).collect();
        let files: Vec<(&str, Vec<u8>)> = names.iter().map(|n| (n.as_str(), vec![1])).collect();
        let (store, _) = store_with("chef", &files);
        let (images, problems) = load(&store, "chef", &names);
        assert_eq!(images.len(), MAX_ATTACHMENTS);
        assert!(problems.iter().any(|p| p.contains("only the first 4")), "{problems:?}");
    }
}
