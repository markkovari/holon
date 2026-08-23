//! Git's object serialisation, as pure functions.
//!
//! Separated from the component so it can be tested natively against the real
//! `git` binary — which is the only ground truth worth having here. Every id this
//! produces is checked against `git hash-object` and `git mktree` rather than
//! against what we expected, because "our tree hashes consistently" and "our tree
//! hashes the way git does" are different claims and only the second one matters.
//!
//! No WASI, no storage, no bindings. Just bytes in, bytes and ids out.

use sha1::{Digest, Sha1};

/// `<type> <len>\0<payload>` — the bytes git hashes, and the bytes stored.
///
/// Stored WITH the header rather than without: it makes an object
/// self-describing, so a read knows what it got, and it makes the id
/// re-checkable against the content at any time.
pub fn frame(kind: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 32);
    out.extend_from_slice(kind.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(0);
    out.extend_from_slice(payload);
    out
}

/// The git object id of an already-framed object.
pub fn id_of(framed: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(framed);
    let d = h.finalize();
    let mut s = String::with_capacity(40);
    for b in d {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// Split a stored object back into (type, payload).
pub fn unframe(bytes: &[u8]) -> Result<(String, &[u8]), String> {
    let nul = bytes.iter().position(|b| *b == 0).ok_or("no NUL in the object header")?;
    let head = core::str::from_utf8(&bytes[..nul]).map_err(|_| "header is not utf-8")?;
    let (kind, len) = head.split_once(' ').ok_or("header has no space")?;
    let len: usize = len.parse().map_err(|_| format!("header length {len:?} is not a number"))?;
    let payload = &bytes[nul + 1..];
    if payload.len() != len {
        // A truncated object that still parses is the worst kind, because it
        // reads as valid and produces a wrong tree.
        return Err(format!("object says {len} bytes and carries {}", payload.len()));
    }
    Ok((kind.to_string(), payload))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub name: String,
    pub id: String,
}

/// Git's tree ordering.
///
/// Entries sort by name, except that a SUBTREE sorts as though its name ended in
/// `/`. So `foo` the file comes before `foo.txt`, and `foo` the directory comes
/// after it — because `foo/` > `foo.`. Get this wrong and the tree still
/// serialises, still hashes, and hashes to something git does not agree with.
fn sort_key(e: &Entry) -> Vec<u8> {
    let mut k = e.name.as_bytes().to_vec();
    if e.mode == "40000" {
        k.push(b'/');
    }
    k
}

/// `<mode> <name>\0<20 raw bytes>` per entry, sorted.
pub fn tree_payload(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|e| sort_key(e));
    // Two entries with the same name would produce a tree git refuses to read,
    // and the failure would land somewhere far away from the cause.
    for w in sorted.windows(2) {
        if w[0].name == w[1].name {
            return Err(format!("two entries named {:?} in one tree", w[0].name));
        }
    }
    let mut out = Vec::new();
    for e in sorted {
        if !matches!(e.mode.as_str(), "100644" | "100755" | "40000" | "120000" | "160000") {
            return Err(format!("{:?} is not a git mode", e.mode));
        }
        if e.name.is_empty() || e.name.contains('/') || e.name.contains('\0') {
            return Err(format!("{:?} is not a usable tree entry name", e.name));
        }
        out.extend_from_slice(e.mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&hex_to_raw(&e.id)?);
    }
    Ok(out)
}

pub fn parse_tree(payload: &[u8]) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        let sp = payload[i..].iter().position(|b| *b == b' ').ok_or("tree entry has no space")? + i;
        let nul = payload[i..].iter().position(|b| *b == 0).ok_or("tree entry has no NUL")? + i;
        if nul + 21 > payload.len() {
            return Err("tree entry is truncated before its id".into());
        }
        let mode = core::str::from_utf8(&payload[i..sp]).map_err(|_| "mode is not utf-8")?;
        let name = core::str::from_utf8(&payload[sp + 1..nul]).map_err(|_| "name is not utf-8")?;
        out.push(Entry {
            mode: mode.to_string(),
            name: name.to_string(),
            id: raw_to_hex(&payload[nul + 1..nul + 21]),
        });
        i = nul + 21;
    }
    Ok(out)
}

pub struct Commit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub when: u64,
    pub message: String,
}

/// The commit payload, in the order git writes it.
///
/// The timezone is fixed at `+0000` and the time comes from the caller, so the
/// same inputs always produce the same commit id. A commit whose id moves with
/// the wall clock cannot be deduplicated, compared between generations, or
/// re-derived to check anything.
pub fn commit_payload(c: &Commit) -> Result<Vec<u8>, String> {
    if c.tree.len() != 40 {
        return Err(format!("tree id {:?} is not a sha", c.tree));
    }
    let mut s = format!("tree {}\n", c.tree);
    for p in &c.parents {
        if p.len() != 40 {
            return Err(format!("parent id {p:?} is not a sha"));
        }
        s.push_str(&format!("parent {p}\n"));
    }
    let who = if c.author.is_empty() { "comp <comp@invalid>" } else { &c.author };
    s.push_str(&format!("author {who} {} +0000\n", c.when));
    s.push_str(&format!("committer {who} {} +0000\n", c.when));
    s.push('\n');
    s.push_str(&c.message);
    // git's own commits end with a newline; without one, `git log` runs the
    // next thing onto the same line.
    if !c.message.ends_with('\n') {
        s.push('\n');
    }
    Ok(s.into_bytes())
}

pub fn parse_commit(payload: &[u8]) -> Result<Commit, String> {
    let text = core::str::from_utf8(payload).map_err(|_| "commit is not utf-8")?;
    let (head, message) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut c = Commit {
        tree: String::new(),
        parents: Vec::new(),
        author: String::new(),
        when: 0,
        message: message.to_string(),
    };
    for line in head.lines() {
        if let Some(v) = line.strip_prefix("tree ") {
            c.tree = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("parent ") {
            c.parents.push(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("author ") {
            // `Name <email> <seconds> <tz>` — take the identity and the seconds,
            // and drop the offset, which we always write as +0000 anyway.
            let mut parts: Vec<&str> = v.rsplitn(3, ' ').collect();
            parts.reverse();
            if parts.len() == 3 {
                c.author = parts[0].to_string();
                c.when = parts[1].parse().unwrap_or(0);
            } else {
                c.author = v.to_string();
            }
        }
    }
    if c.tree.is_empty() {
        return Err("commit has no tree".into());
    }
    Ok(c)
}

fn hex_to_raw(hex: &str) -> Result<[u8; 20], String> {
    if hex.len() != 40 {
        return Err(format!("{hex:?} is not a 40-character sha"));
    }
    let b = hex.as_bytes();
    let mut out = [0u8; 20];
    for i in 0..20 {
        let hi = (b[i * 2] as char).to_digit(16).ok_or_else(|| format!("{hex:?} is not hex"))?;
        let lo =
            (b[i * 2 + 1] as char).to_digit(16).ok_or_else(|| format!("{hex:?} is not hex"))?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Ok(out)
}

fn raw_to_hex(raw: &[u8]) -> String {
    let mut s = String::with_capacity(raw.len() * 2);
    for b in raw {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// A path split into its segments, refusing anything that leaves the tree.
pub fn split_path(path: &str) -> Result<Vec<String>, String> {
    if path.is_empty() || path.starts_with('/') {
        return Err(format!("{path:?} is not a path inside the tree"));
    }
    let segs: Vec<String> = path.split('/').map(str::to_string).collect();
    if segs.iter().any(|s| s.is_empty() || s == "." || s == ".." || s.contains('\0')) {
        return Err(format!("{path:?} escapes the tree or is malformed"));
    }
    Ok(segs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Ask the real git binary. `None` when git is not installed, so the
    /// assertions can skip rather than fail for the wrong reason.
    fn git(args: &[&str], stdin: Option<&[u8]>) -> Option<String> {
        let mut c = Command::new("git");
        c.args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().ok()?;
        if let Some(b) = stdin {
            child.stdin.take()?.write_all(b).ok()?;
        }
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The claim is not "we hash consistently", it is "we hash the way git does".
    /// Only the real binary can settle that.
    #[test]
    fn a_blob_id_is_the_id_git_gives_it() {
        for content in [
            &b"hello\n"[..],
            &b""[..],
            &b"fn main() {}\n"[..],
            // Not UTF-8, because a repository is full of things that are not.
            &[0xff, 0xfe, 0x00, 0x01][..],
        ] {
            let ours = id_of(&frame("blob", content));
            let Some(theirs) = git(&["hash-object", "-t", "blob", "--stdin"], Some(content)) else {
                eprintln!("SKIPPED: no usable `git` binary");
                return;
            };
            assert_eq!(ours, theirs, "blob id disagrees with git for {content:?}");
        }
    }

    /// Tree ordering is the classic way to get a subtly wrong id: a subtree sorts
    /// as though its name ended in `/`, so `foo` the DIRECTORY comes after
    /// `foo.txt` while `foo` the FILE comes before it.
    #[test]
    fn a_tree_id_is_the_id_git_gives_it_including_the_ordering_trap() {
        let empty = id_of(&frame("blob", b""));
        let entries = vec![
            Entry { mode: "100644".into(), name: "foo.txt".into(), id: empty.clone() },
            Entry { mode: "40000".into(), name: "foo".into(), id: String::new() },
            Entry { mode: "100755".into(), name: "run.sh".into(), id: empty.clone() },
            Entry { mode: "100644".into(), name: "a.txt".into(), id: empty.clone() },
        ];

        // Build the subtree first so it has a real id.
        let sub_payload = tree_payload(&[Entry {
            mode: "100644".into(),
            name: "inner".into(),
            id: empty.clone(),
        }])
        .unwrap();
        let sub_id = id_of(&frame("tree", &sub_payload));

        let mut entries = entries;
        entries[1].id = sub_id.clone();
        let ours = id_of(&frame("tree", &tree_payload(&entries).unwrap()));

        // `git mktree` sorts nothing — it takes what it is given in order and
        // trusts it — so feeding it the same entries proves OUR ordering matches.
        let mut spec = String::new();
        for e in &entries {
            let kind = if e.mode == "40000" { "tree" } else { "blob" };
            spec.push_str(&format!("{} {} {}\t{}\n", e.mode, kind, e.id, e.name));
        }
        // Sorted the way we sort, then handed over verbatim.
        let mut lines: Vec<&str> = spec.lines().collect();
        lines.sort_by_key(|l| {
            let name = l.split('\t').nth(1).unwrap_or("");
            let is_tree = l.contains(" tree ");
            let mut k = name.as_bytes().to_vec();
            if is_tree {
                k.push(b'/');
            }
            k
        });
        let ordered = lines.join("\n") + "\n";

        // The blobs have to exist in a real repository for mktree to accept them.
        let dir = std::env::temp_dir().join(format!("comp-vgit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        if git(&["-C", &d, "init", "-q"], None).is_none() {
            eprintln!("SKIPPED: no usable `git` binary");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let hashed = git(&["-C", &d, "hash-object", "-w", "-t", "blob", "--stdin"], Some(b""));
        assert_eq!(hashed.as_deref(), Some(empty.as_str()), "the empty blob should round-trip");
        // Write the subtree into the repo too.
        let _ =
            git(&["-C", &d, "mktree"], Some(format!("100644 blob {empty}\tinner\n").as_bytes()));

        let theirs = git(&["-C", &d, "mktree"], Some(ordered.as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(theirs) = theirs else {
            eprintln!("SKIPPED: `git mktree` would not run");
            return;
        };
        assert_eq!(
            ours, theirs,
            "tree id disagrees with git — the ordering rule is the usual cause"
        );
    }

    #[test]
    fn a_tree_round_trips_through_its_own_serialisation() {
        let empty = id_of(&frame("blob", b""));
        let entries = vec![
            Entry { mode: "100644".into(), name: "b.txt".into(), id: empty.clone() },
            Entry { mode: "100644".into(), name: "a.txt".into(), id: empty.clone() },
        ];
        let payload = tree_payload(&entries).unwrap();
        let back = parse_tree(&payload).unwrap();
        // Sorted, so `a` comes first regardless of what the caller passed.
        assert_eq!(back[0].name, "a.txt");
        assert_eq!(back[1].name, "b.txt");
        assert_eq!(back[0].id, empty);
    }

    #[test]
    fn a_duplicate_name_is_refused_rather_than_written() {
        let empty = id_of(&frame("blob", b""));
        let dup = vec![
            Entry { mode: "100644".into(), name: "a".into(), id: empty.clone() },
            Entry { mode: "100644".into(), name: "a".into(), id: empty },
        ];
        assert!(tree_payload(&dup).is_err(), "git cannot read a tree with a duplicate name");
    }

    #[test]
    fn a_mode_git_does_not_have_is_refused() {
        let empty = id_of(&frame("blob", b""));
        // 040000 is the one people write, and it hashes differently from 40000.
        let bad = vec![Entry { mode: "040000".into(), name: "d".into(), id: empty }];
        assert!(tree_payload(&bad).is_err(), "040000 is not how git writes a subtree");
    }

    /// The commit id must be a function of the inputs alone.
    #[test]
    fn a_commit_id_does_not_move_with_the_clock() {
        let c = Commit {
            tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
            parents: vec![],
            author: "Ada <ada@example.com>".into(),
            when: 1_700_000_000,
            message: "first".into(),
        };
        let a = id_of(&frame("commit", &commit_payload(&c).unwrap()));
        let b = id_of(&frame("commit", &commit_payload(&c).unwrap()));
        assert_eq!(a, b, "the same commit must hash the same twice");

        let back = parse_commit(&commit_payload(&c).unwrap()).unwrap();
        assert_eq!(back.tree, c.tree);
        assert_eq!(back.author, c.author);
        assert_eq!(back.when, c.when);
        assert_eq!(back.message.trim_end(), "first");
    }

    /// And the commit git would make from the same inputs.
    #[test]
    fn a_commit_id_is_the_id_git_gives_it() {
        let dir = std::env::temp_dir().join(format!("comp-vgit-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        if git(&["-C", &d, "init", "-q"], None).is_none() {
            eprintln!("SKIPPED: no usable `git` binary");
            return;
        }
        // The empty tree, which git knows without being told.
        let tree = git(&["-C", &d, "mktree"], Some(b"")).unwrap_or_default();
        let c = Commit {
            tree: tree.clone(),
            parents: vec![],
            author: "Ada <ada@example.com>".into(),
            when: 1_700_000_000,
            message: "first".into(),
        };
        let ours = id_of(&frame("commit", &commit_payload(&c).unwrap()));

        let theirs = {
            let mut cmd = Command::new("git");
            cmd.args(["-C", &d, "commit-tree", &tree, "-m", "first"])
                .env("GIT_AUTHOR_NAME", "Ada")
                .env("GIT_AUTHOR_EMAIL", "ada@example.com")
                .env("GIT_AUTHOR_DATE", "1700000000 +0000")
                .env("GIT_COMMITTER_NAME", "Ada")
                .env("GIT_COMMITTER_EMAIL", "ada@example.com")
                .env("GIT_COMMITTER_DATE", "1700000000 +0000")
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            cmd.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        let _ = std::fs::remove_dir_all(&dir);
        let Some(theirs) = theirs.filter(|t| t.len() == 40) else {
            eprintln!("SKIPPED: `git commit-tree` would not run");
            return;
        };
        assert_eq!(ours, theirs, "commit id disagrees with git");
    }

    #[test]
    fn a_truncated_object_is_corrupt_rather_than_short() {
        let framed = frame("blob", b"hello");
        let (kind, payload) = unframe(&framed).unwrap();
        assert_eq!(kind, "blob");
        assert_eq!(payload, b"hello");
        // Chop a byte: it still parses as a header, and must not read as valid.
        assert!(unframe(&framed[..framed.len() - 1]).is_err(), "a short object must be refused");
    }

    #[test]
    fn a_path_cannot_leave_the_tree() {
        assert_eq!(split_path("a/b/c.rs").unwrap(), vec!["a", "b", "c.rs"]);
        for bad in ["", "/abs", "a//b", "../x", "a/../b", "a/./b"] {
            assert!(split_path(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
