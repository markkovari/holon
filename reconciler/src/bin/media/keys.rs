//! Keys and ids this daemon accepts: a `photo_id`/`job_id`'s character
//! class, an upload's file extension, and which bucket (and object inside
//! it) a key names — the only three questions "is this ours" ever needs.

use std::path::Path;

/// S3's minimum part is 5 MiB; 16 MiB keeps a 129 MB raw file at 8 parts,
/// few enough that a browser's retry of one is cheap.
pub(crate) const PART_SIZE: u64 = 16 * 1024 * 1024;
pub(crate) const ALLOWED_EXTS: &[&str] = &["arw", "jpg", "jpeg"];

// ---- validation --------------------------------------------------------------

/// `[A-Za-z0-9_-]{1,64}`. A `photo_id` becomes part of an object key and a
/// `job_id` a directory name, so neither may carry a `/` or a `..`.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The lower-cased extension of `filename`, if it is one this accepts.
pub(crate) fn allowed_ext(filename: &str) -> Option<String> {
    let ext = Path::new(filename).extension()?.to_str()?.to_ascii_lowercase();
    ALLOWED_EXTS.contains(&ext.as_str()).then_some(ext)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    Originals,
    Renditions,
}

/// Parse a key this daemon could have made, and nothing else: `originals/<id>.<ext>`
/// or `renditions/<id>/{thumb,share,ai}.jpg`. Returns the bucket and the object
/// name inside it.
pub(crate) fn parse_key(key: &str) -> Option<(Which, &str)> {
    if let Some(obj) = key.strip_prefix("originals/") {
        let (id, ext) = obj.rsplit_once('.')?;
        return (valid_id(id) && ALLOWED_EXTS.contains(&ext)).then_some((Which::Originals, obj));
    }
    if let Some(obj) = key.strip_prefix("renditions/") {
        let (id, file) = obj.split_once('/')?;
        return (valid_id(id) && ["thumb.jpg", "share.jpg", "ai.jpg"].contains(&file))
            .then_some((Which::Renditions, obj));
    }
    None
}

/// `(number, length)` for every part of a `size`-byte upload. 1-based, as S3
/// numbers them; every part is `part_size` except a shorter last one.
pub(crate) fn plan_parts(size: u64, part_size: u64) -> Vec<(u16, u64)> {
    let n = size.div_ceil(part_size);
    (0..n)
        .map(|i| {
            let len = if i + 1 == n { size - i * part_size } else { part_size };
            (i as u16 + 1, len)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_the_contracts_character_class_and_nothing_else() {
        assert!(valid_id("photo_01-A"));
        assert!(valid_id(&"x".repeat(64)));
        assert!(!valid_id(&"x".repeat(65)));
        assert!(!valid_id(""));
        for bad in ["../etc", "a/b", "a.b", "a b", "é", "a\0b"] {
            assert!(!valid_id(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn only_keys_this_daemon_could_have_made_parse() {
        assert_eq!(parse_key("originals/p1.arw"), Some((Which::Originals, "p1.arw")));
        assert_eq!(parse_key("renditions/p1/share.jpg"), Some((Which::Renditions, "p1/share.jpg")));
        for bad in [
            "originals/../x.arw",
            "originals/p1.exe",
            "originals/p1",
            "originals/a/b.arw",
            "renditions/p1/other.jpg",
            "renditions/../p1/thumb.jpg",
            "renditions/p1/thumb.jpg/x",
            "elsewhere/p1.arw",
            "p1.arw",
        ] {
            assert_eq!(parse_key(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_extension_is_lower_cased_and_allow_listed() {
        assert_eq!(allowed_ext("SZI02833.ARW").as_deref(), Some("arw"));
        assert_eq!(allowed_ext("a.JPEG").as_deref(), Some("jpeg"));
        assert_eq!(allowed_ext("a.png"), None);
        assert_eq!(allowed_ext("noext"), None);
    }

    /// The real file: 129,581,056 bytes is seven full 16 MiB parts and a
    /// shorter eighth, and the lengths add back up to the size.
    #[test]
    fn parts_are_planned_with_a_short_last_one() {
        let parts = plan_parts(129_581_056, PART_SIZE);
        assert_eq!(parts.len(), 8);
        assert_eq!(parts[0], (1, PART_SIZE));
        assert_eq!(parts[7].0, 8);
        assert_eq!(parts.iter().map(|p| p.1).sum::<u64>(), 129_581_056);
        assert_eq!(plan_parts(PART_SIZE, PART_SIZE), vec![(1, PART_SIZE)]);
        assert_eq!(plan_parts(1, PART_SIZE), vec![(1, 1)]);
        assert_eq!(plan_parts(PART_SIZE + 1, PART_SIZE), vec![(1, PART_SIZE), (2, 1)]);
    }
}
