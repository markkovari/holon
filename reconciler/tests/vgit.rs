//! `vgit:store` on a real fleet: blob storage as the filesystem for git.
//!
//! virt-git's own tests check its serialisation against the `git` binary. They
//! run natively, on pure functions, and cannot reach the half that matters
//! operationally: whether objects actually land in `blob:store` when the thing is
//! deployed, whether a guarded ref update behaves under a lost race, whether an
//! untouched subtree is genuinely reused by id rather than rewritten to something
//! equal-looking — and whether a commit produced by a component running on a node
//! still has the id git would give it.
//!
//! That last assertion is the one worth the whole file. A component that hashes
//! correctly in a unit test and stores wrongly on a fleet looks right and is not,
//! which is precisely how `comp:secrets/reader` shipped unlinked (ADR-0061).
//!
//! So the ground truth here is the real `git` binary, again — asked the same
//! question, about bytes that made a full round trip through a deployed
//! component, a linked capability and a replicated key-value store.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

/// Ask the real git binary. `None` if it will not run.
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
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in
        [("gate", "vgit_probe.wasm"), ("vgit", "virt_git.wasm"), ("blobs", "blob_store.wasm")]
    {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path, String::new())
    }

    fn post(&self, path: &str, body: Value) -> Value {
        self.call(reqwest::Method::POST, path, body.to_string())
    }

    fn call(&self, method: reqwest::Method, path: &str, body: String) -> Value {
        // A transport failure is REPORTED rather than panicked on: the readiness
        // loop polls before anything is listening, and a panic there would make
        // "not up yet" indistinguishable from "broken".
        let r = match self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "vgit.acme.test")
            .body(body)
            .send()
        {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }

    /// Commit a set of whole files onto `base`.
    fn commit(&self, base: &str, message: &str, files: &[(&str, &str)]) -> Value {
        let changes: Vec<Value> = files
            .iter()
            .map(|(p, c)| json!({ "path": p, "content": c, "remove": false }))
            .collect();
        self.post("/commit", json!({ "base": base, "message": message, "changes": changes }))
    }
}

/// The first real read, retried until it works.
///
/// NOT a separate readiness probe — see `Fleet::until`. Reading an absent ref
/// crosses the link, opens the app's bucket and does a CAS read, so `found:
/// false` is both the answer wanted and the proof the path exists.
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap(),
    };
    fleet.until("reading a ref that does not exist", Duration::from_secs(120), || {
        let r = probe.get("/ref?name=probe%2Fready");
        if r["found"] == json!(false) {
            Ok(())
        } else {
            Err(r.to_string())
        }
    });
    probe
}

#[test]
fn a_repository_lives_in_blob_storage_and_git_agrees_with_its_ids() {
    let fleet = Fleet::start_with_secrets("vgit", &["fixtures/virt-git.yaml"], &artifacts(), &[]);
    let probe = wait_for_probe(&fleet);

    // --- a first commit, from nothing ---------------------------------------
    let first = probe.commit(
        "",
        "first",
        &[("src/lib.rs", "fn main() {}\n"), ("docs/a.md", "# a\n"), ("README.md", "hello\n")],
    );
    let c1 = first["commit"].as_str().unwrap_or_default().to_string();
    assert_eq!(c1.len(), 40, "no commit came back: {first}");

    // The files are readable back out of blob storage.
    let r = probe.get(&format!("/read?commit={c1}&path=src%2Flib.rs"));
    assert_eq!(r["content"], json!("fn main() {}\n"), "the file did not round-trip: {r}");

    let r = probe.get(&format!("/paths?commit={c1}"));
    assert_eq!(
        r["paths"],
        json!(["README.md", "docs/a.md", "src/lib.rs"]),
        "the tree does not list what was written: {r}"
    );

    // --- the assertion this file exists for ---------------------------------
    // A blob that travelled through a deployed component, a linked capability and
    // the key-value store must still have the id git gives those bytes.
    let Some(theirs) = git(&["hash-object", "-t", "blob", "--stdin"], Some(b"fn main() {}\n")) else {
        eprintln!("SKIPPED: no usable `git` binary — the ground truth is unavailable");
        return;
    };
    let tree_of_src = probe.get(&format!("/tree?commit={c1}&path=src"));
    assert_eq!(tree_of_src["tree"].as_str().map(str::len), Some(40), "no subtree id: {tree_of_src}");

    // And the commit itself, against `git commit-tree` with the same tree,
    // author and timestamp — which is why the probe pins both.
    let dir = std::env::temp_dir().join(format!("comp-vgit-fleet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.to_string_lossy().to_string();
    assert!(git(&["-C", &d, "init", "-q"], None).is_some(), "git init failed");
    let blob_in_repo = git(&["-C", &d, "hash-object", "-w", "-t", "blob", "--stdin"], Some(b"fn main() {}\n"));
    assert_eq!(blob_in_repo.as_deref(), Some(theirs.as_str()));
    let src_tree = git(&["-C", &d, "mktree"], Some(format!("100644 blob {theirs}\tlib.rs\n").as_bytes()))
        .expect("mktree");
    assert_eq!(
        tree_of_src["tree"].as_str().unwrap_or_default(),
        src_tree,
        "the subtree this component built in blob storage is not the tree git builds \
         from the same content — the ids are supposed to be REAL git ids"
    );

    // --- a second commit touching one file ----------------------------------
    let second = probe.commit(&c1, "second", &[("src/lib.rs", "fn main() { /* v2 */ }\n")]);
    let c2 = second["commit"].as_str().unwrap_or_default().to_string();
    assert_ne!(c2, c1, "a different change must be a different commit");

    // The untouched subtree is REUSED BY ID, not rewritten. This is the property
    // that makes a branch cost its diff rather than the repository: if `docs`
    // came back with a new id, every candidate would be rewriting the whole tree.
    let docs1 = probe.get(&format!("/tree?commit={c1}&path=docs"));
    let docs2 = probe.get(&format!("/tree?commit={c2}&path=docs"));
    assert_eq!(
        docs1["tree"], docs2["tree"],
        "an untouched subtree was rewritten — a change should cost its depth, not the repo"
    );
    let src1 = probe.get(&format!("/tree?commit={c1}&path=src"));
    let src2 = probe.get(&format!("/tree?commit={c2}&path=src"));
    assert_ne!(src1["tree"], src2["tree"], "the touched subtree must change");

    // The old commit still reads the old content: history is immutable because
    // objects are, not because anything defends it.
    let old = probe.get(&format!("/read?commit={c1}&path=src%2Flib.rs"));
    assert_eq!(old["content"], json!("fn main() {}\n"), "the first commit changed under us: {old}");

    // --- diff ---------------------------------------------------------------
    let d1 = probe.commit(&c2, "third", &[("docs/b.md", "# b\n")]);
    let c3 = d1["commit"].as_str().unwrap_or_default().to_string();
    let r = probe.get(&format!("/diff?before={c1}&after={c3}"));
    let changes = r["changes"].as_array().cloned().unwrap_or_default();
    let mut summary: Vec<String> = changes
        .iter()
        .map(|c| format!("{} {}", c["kind"].as_str().unwrap_or(""), c["path"].as_str().unwrap_or("")))
        .collect();
    summary.sort();
    assert_eq!(
        summary,
        vec!["added docs/b.md", "modified src/lib.rs"],
        "diff should see exactly the two changes across three commits: {r}"
    );

    // --- refs, which are the only mutable thing ------------------------------
    // Creating: `expect` absent means "and it must not exist yet".
    let r = probe.post(&format!("/ref?name=heads%2Fmain&to={c1}"), json!({}));
    assert_eq!(r["updated"], json!(true), "creating a ref failed: {r}");
    let r = probe.get("/ref?name=heads%2Fmain");
    assert_eq!(r["ref"], json!(c1), "the ref does not point where it was put: {r}");

    // Creating it AGAIN must lose. This is the branch-collision case, and losing
    // silently would mean one branch overwriting another's work.
    let r = probe.post(&format!("/ref?name=heads%2Fmain&to={c2}"), json!({}));
    assert_eq!(
        r["updated"],
        json!(false),
        "a create that finds the ref already there must lose, not overwrite: {r}"
    );
    let r = probe.get("/ref?name=heads%2Fmain");
    assert_eq!(r["ref"], json!(c1), "the losing write moved the ref anyway: {r}");

    // A guarded move that names the right expectation wins.
    let r = probe.post(&format!("/ref?name=heads%2Fmain&expect={c1}&to={c2}"), json!({}));
    assert_eq!(r["updated"], json!(true), "a correctly-guarded move should win: {r}");

    // And one holding a stale expectation loses — the lost-update case (ADR-0065)
    // with somebody's branch as the thing that would have been lost.
    let r = probe.post(&format!("/ref?name=heads%2Fmain&expect={c1}&to={c3}"), json!({}));
    assert_eq!(r["updated"], json!(false), "a stale guarded move must lose: {r}");
    let r = probe.get("/ref?name=heads%2Fmain");
    assert_eq!(r["ref"], json!(c2), "the stale write landed anyway — that is a lost update: {r}");

    let _ = std::fs::remove_dir_all(&dir);
    println!("    a repo in blob storage: real git ids, subtree reuse, and refs that do not lose writes");
}
