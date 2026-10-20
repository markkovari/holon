//! Git blob/tree ids exactly as `git` computes them (checked against the binary in tests/).
//! The ids depend on git's quirks: dir mode `40000` has no leading zero; dirs sort as if named `name/`.

use std::collections::BTreeMap;

use sha1::{Digest, Sha1};

use crate::error::{Result, VcsError};

pub fn blob_id(bytes: &[u8]) -> [u8; 20] {
    // SHA-1 is git's object id format, not a security hash.
    let mut h = Sha1::new(); // nosemgrep: rust.lang.security.insecure-hashes.insecure-hashes
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    h.finalize().into()
}

enum Node {
    File { id: [u8; 20], executable: bool },
    Dir(BTreeMap<String, Node>),
}

pub fn validate_path(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.contains('\0')
        || path.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err(VcsError::Invalid(format!("path {path:?} is not a relative file path")));
    }
    Ok(())
}

pub fn tree_id(files: &[(String, Vec<u8>, bool)]) -> Result<String> {
    let mut root = BTreeMap::new();
    for (path, bytes, executable) in files {
        validate_path(path)?;
        let segs: Vec<&str> = path.split('/').collect();
        let mut dir = &mut root;
        for (i, seg) in segs.iter().enumerate() {
            if i + 1 == segs.len() {
                if dir.contains_key(*seg) {
                    return Err(VcsError::Invalid(format!(
                        "{path} is both a file and a directory"
                    )));
                }
                dir.insert(
                    seg.to_string(),
                    Node::File { id: blob_id(bytes), executable: *executable },
                );
            } else {
                let next = dir.entry(seg.to_string()).or_insert_with(|| Node::Dir(BTreeMap::new()));
                dir = match next {
                    Node::Dir(d) => d,
                    Node::File { .. } => {
                        return Err(VcsError::Invalid(format!(
                            "{path} is both a file and a directory"
                        )));
                    }
                };
            }
        }
    }
    Ok(hex::encode(write_tree(&root)))
}

fn write_tree(dir: &BTreeMap<String, Node>) -> [u8; 20] {
    let mut entries: Vec<(Vec<u8>, &'static str, &String, [u8; 20])> = dir
        .iter()
        .map(|(name, node)| match node {
            Node::File { id, executable } => {
                (name.as_bytes().to_vec(), if *executable { "100755" } else { "100644" }, name, *id)
            }
            Node::Dir(d) => {
                let mut sort = name.as_bytes().to_vec();
                sort.push(b'/');
                (sort, "40000", name, write_tree(d))
            }
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut body = Vec::new();
    for (_, mode, name, id) in entries {
        body.extend_from_slice(mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(&id);
    }
    // SHA-1 is git's object id format, not a security hash.
    let mut h = Sha1::new(); // nosemgrep: rust.lang.security.insecure-hashes.insecure-hashes
    h.update(format!("tree {}\0", body.len()).as_bytes());
    h.update(&body);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids() {
        // `git hash-object /dev/null` and the empty tree.
        assert_eq!(hex::encode(blob_id(b"")), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(tree_id(&[]).unwrap(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    }

    #[test]
    fn rejects_bad_paths_and_file_dir_clashes() {
        assert!(validate_path("/abs").is_err());
        assert!(validate_path("a/../b").is_err());
        assert!(validate_path("a//b").is_err());
        let clash = vec![("a".to_string(), vec![], false), ("a/b".to_string(), vec![], false)];
        assert!(tree_id(&clash).is_err());
    }
}
