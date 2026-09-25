//! Symbol extraction end to end: real files from this repository → symbols →
//! the store → `snapshot-export` → the same bytes, and the same git tree id.
//! Then the reason extraction exists: two agents editing one real file.
//!
//! Always runs in memory. With `--features native` and
//! `HOLON_VCS_NATS_URL` + `HOLON_VCS_SURREAL_URL` set, the scenarios also run
//! against NATS JetStream + SurrealDB (see `tests/live.rs` for how to start them).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use holon_vcs::engine::Engine;
use holon_vcs::extract::{self, EditKind, ExtractedSymbol, IngestReport};
use holon_vcs::graph::Graph;
use holon_vcs::model::*;
use holon_vcs::oplog::OpLog;
use holon_vcs::store::{BlobStore, PointerStore};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// `(repo-relative path, bytes)` for every file under `dir` matching `ext`.
fn files_under(dir: &str, ext: &str) -> Vec<(String, Vec<u8>)> {
    let root = repo();
    let abs = root.join(dir);
    if !abs.exists() {
        return vec![];
    }
    extract::read_tree(&abs, |p| p.ends_with(ext) && !p.contains("target/"))
        .unwrap()
        .into_iter()
        .map(|(p, b)| (format!("{dir}/{p}"), b))
        .collect()
}

/// The corpus the round trip is measured on (the brief's list, plus the tests).
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    out.extend(files_under("crates/holon-vcs/src", ".rs"));
    out.extend(files_under("crates/holon-vcs/tests", ".rs"));
    out.extend(files_under("components/record-store/src", ".rs"));
    out.extend(files_under("reconciler/src/bin", "media.rs"));
    out.extend(files_under("wit", ".wit"));
    let comps = repo().join("components");
    let mut names: Vec<String> = std::fs::read_dir(&comps)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("wit").is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        out.extend(files_under(&format!("components/{n}/wit"), ".wit"));
    }
    out
}

async fn file_bytes<B, P, G, L>(
    e: &Engine<B, P, G, L>,
    snap: &Snapshot,
) -> BTreeMap<String, Vec<u8>>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let mut out = BTreeMap::new();
    for t in &snap.entries {
        out.insert(t.path.clone(), e.blobs().get(&t.blob).await.unwrap().unwrap());
    }
    out
}

fn git_write_tree(files: &[(String, Vec<u8>)], tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("holon-vcs-extract-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (p, b) in files {
        let f = dir.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b).unwrap();
    }
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    let tree = git(&["write-tree"]);
    std::fs::remove_dir_all(&dir).unwrap();
    tree
}

fn agent(id: &str) -> Agent {
    Agent::named(id)
}

fn assert_clean(reports: &[IngestReport]) {
    for r in reports {
        assert!(r.is_clean(), "{}: {:?}", r.path, r.patches);
    }
}

// ---- the corpus ------------------------------------------------------------------

/// Every corpus file: extract → ingest → snapshot-export → identical bytes, and
/// the snapshot's git tree is `git write-tree` of the originals. Re-ingesting
/// the unchanged tree writes nothing.
pub async fn corpus_round_trip<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let files = corpus();
    assert!(files.len() > 250, "corpus has {} files", files.len());
    let (mut symbols, mut whole) = (0, 0);
    for (p, b) in &files {
        let s = extract::extract("corpus", p, b).unwrap();
        let joined: Vec<u8> = s.iter().flat_map(|s| s.content.bytes()).collect();
        assert_eq!(&joined, b, "{p} does not round-trip through extract");
        symbols += s.len();
        if s.len() == 1 && s[0].id.kind == SymbolKind::File {
            whole += 1;
        }
    }
    let reports = extract::ingest_tree(e, ws, "corpus", &files, &agent("importer"), None, false)
        .await
        .unwrap();
    assert_clean(&reports);
    let patches: usize = reports.iter().map(|r| r.patches.len()).sum();
    assert_eq!(patches, symbols, "one create per symbol");
    let snap = e.snapshot_export(ws, "corpus").await.unwrap();
    let got = file_bytes(e, &snap).await;
    let mut diffs = Vec::new();
    for (p, b) in &files {
        if got.get(p) != Some(b) {
            diffs.push(p.clone());
        }
    }
    assert!(diffs.is_empty(), "{} files differ: {diffs:?}", diffs.len());
    assert_eq!(got.len(), files.len());
    assert_eq!(snap.git_tree.as_deref(), Some(git_write_tree(&files, ws_tag(ws)).as_str()));
    eprintln!(
        "corpus: {} files, {} bytes, {symbols} symbols, {whole} kept whole; 0 byte diffs; git tree {}",
        files.len(),
        files.iter().map(|f| f.1.len()).sum::<usize>(),
        snap.git_tree.as_deref().unwrap_or("-")
    );

    // Again, unchanged: no patches at all.
    let at = extract::read_point(e, ws).await.unwrap();
    let again = extract::ingest_tree(e, ws, "corpus", &files, &agent("importer"), Some(at), true)
        .await
        .unwrap();
    let n: usize = again.iter().map(|r| r.patches.len()).sum();
    assert_eq!(n, 0, "re-ingest of an unchanged tree wrote {n} patches");
}

fn ws_tag(ws: &str) -> &str {
    ws.rsplit('/').next().unwrap_or(ws)
}

// ---- edge cases through the store -------------------------------------------------

pub async fn edge_cases<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let base = "//! Header.\nuse std::fmt;\n\n/// Doc.\n#[derive(Debug)]\nstruct A;\n\nimpl fmt::Display for A {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, \"a\") }\n}\n\nmacro_rules! m { () => {} }\nm!();\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() {}\n    mod inner {\n        fn deep() {}\n    }\n}\n";
    let files: Vec<(String, Vec<u8>)> = vec![
        ("crlf.rs".into(), base.replace('\n', "\r\n").into_bytes()),
        ("no_newline.rs".into(), base.trim_end().as_bytes().to_vec()),
        ("comments.rs".into(), b"// only\n/* comments */\n".to_vec()),
        ("broken.rs".into(), b"fn broken( {\n  nope\n".to_vec()),
        ("bom.rs".into(), format!("\u{feff}{base}").into_bytes()),
        ("nested.rs".into(), base.as_bytes().to_vec()),
        ("bom.wit".into(), b"\xef\xbb\xbfpackage a:b;\ninterface i {}\n".to_vec()),
        ("empty.txt".into(), vec![]),
        ("blob.bin".into(), vec![0, 159, 146, 150, 255, b'\n']),
        ("notes.md".into(), b"# notes\r\n".to_vec()),
    ];
    let r = extract::ingest_tree(e, ws, "edge", &files, &agent("a"), None, false).await.unwrap();
    assert_clean(&r);
    let snap = e.snapshot_export(ws, "edge").await.unwrap();
    let got = file_bytes(e, &snap).await;
    for (p, b) in &files {
        assert_eq!(got.get(p), Some(b), "{p}");
    }
    assert_eq!(
        snap.git_tree.as_deref(),
        Some(git_write_tree(&files, &format!("edge-{}", ws_tag(ws))).as_str())
    );
    // The nested modules really were split.
    let names: Vec<String> = e
        .query_symbol(ws, SymbolQuery::Component("edge".into()))
        .await
        .unwrap()
        .into_iter()
        .filter(|v| v.id.path == "nested.rs")
        .map(|v| v.id.name)
        .collect();
    for want in ["tests", "tests::t", "tests::inner::deep", "tests::inner::(end)", "tests::(end)"] {
        assert!(names.iter().any(|n| n == want), "{want} missing from {names:?}");
    }
}

// ---- the agent scenario, on real code ----------------------------------------------

/// The real file the agents edit.
const REAL: &str = "components/record-store/src/idlist.rs";

fn real_file() -> String {
    std::fs::read_to_string(repo().join(REAL)).unwrap()
}

/// The file with `edit` applied to the function symbol `name`: a comment line
/// after the first line of its body.
fn edit_fn(text: &str, edits: &[(&str, &str)]) -> String {
    let syms = extract::extract_str("orders", REAL, text);
    let mut out = String::new();
    for s in &syms {
        match edits.iter().find(|(n, _)| s.id.name == *n && s.id.kind == SymbolKind::Function) {
            Some((_, line)) => out.push_str(&insert_after_open(&s.content, line)),
            None => out.push_str(&s.content),
        }
    }
    assert_eq!(edits.is_empty(), out == text, "the edit must change something");
    out
}

fn insert_after_open(fn_text: &str, line: &str) -> String {
    let at = fn_text.find("{\n").expect("a multi-line body") + 2;
    format!("{}    {line}\n{}", &fn_text[..at], &fn_text[at..])
}

fn sym(syms: &[ExtractedSymbol], name: &str) -> SymbolId {
    syms.iter().find(|s| s.id.name == name).unwrap().id.clone()
}

async fn file_of<B, P, G, L>(
    e: &Engine<B, P, G, L>,
    ws: &str,
    component: &str,
    path: &str,
) -> String
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let snap = e.snapshot_export(ws, component).await.unwrap();
    String::from_utf8(file_bytes(e, &snap).await.remove(path).unwrap()).unwrap()
}

/// Two agents, one read point, two different functions of a real file, at the
/// same time: both land and the file has both edits.
pub async fn two_agents_two_functions<B, P, G, L>(e: Arc<Engine<B, P, G, L>>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let original = real_file();
    let r =
        extract::ingest_file(&*e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
            .await
            .unwrap();
    assert!(r.is_clean());
    let read = extract::read_point(&*e, ws).await.unwrap();

    let a_copy = edit_fn(&original, &[("insert", "// A: validate the id first")]);
    let b_copy = edit_fn(&original, &[("remove", "// B: removing is idempotent")]);
    let spawn = |copy: String, who: &'static str| {
        let e = e.clone();
        let ws = ws.to_string();
        tokio::spawn(async move {
            extract::ingest_file(&*e, &ws, "orders", REAL, copy.as_bytes(), &agent(who), Some(read))
                .await
                .unwrap()
        })
    };
    let (ra, rb) = tokio::join!(spawn(a_copy, "agent-a"), spawn(b_copy, "agent-b"));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    for r in [&ra, &rb] {
        assert!(r.is_clean(), "{:?}", r.patches);
        assert_eq!(r.patches.len(), 1, "only the edited function is a patch: {:?}", r.patches);
        assert_eq!(r.patches[0].edit, EditKind::Replace);
    }
    let outcomes = [
        ra.patches[0].result.as_ref().unwrap().outcome,
        rb.patches[0].result.as_ref().unwrap().outcome,
    ];
    assert!(
        outcomes.contains(&PatchOutcome::Commuted),
        "one commuted past the other: {outcomes:?}"
    );
    let both = edit_fn(
        &original,
        &[("insert", "// A: validate the id first"), ("remove", "// B: removing is idempotent")],
    );
    assert_eq!(file_of(&*e, ws, "orders", REAL).await, both);
}

/// Two agents, one read point, the SAME function edited two ways: a conflict
/// with both sides verbatim; a resolver merges; the file is the merge.
pub async fn two_agents_one_function<B, P, G, L>(e: Arc<Engine<B, P, G, L>>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let original = real_file();
    extract::ingest_file(&*e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let read = extract::read_point(&*e, ws).await.unwrap();
    let a_copy = edit_fn(&original, &[("insert", "// A was here")]);
    let b_copy = edit_fn(&original, &[("insert", "// B was here")]);
    let ra = extract::ingest_file(
        &*e,
        ws,
        "orders",
        REAL,
        a_copy.as_bytes(),
        &agent("agent-a"),
        Some(read),
    )
    .await
    .unwrap();
    let rb = extract::ingest_file(
        &*e,
        ws,
        "orders",
        REAL,
        b_copy.as_bytes(),
        &agent("agent-b"),
        Some(read),
    )
    .await
    .unwrap();
    assert!(ra.is_clean());
    let conflicts = rb.conflicts();
    assert_eq!(conflicts.len(), 1, "{:?}", rb.patches);

    let syms = extract::extract_str("orders", REAL, &original);
    let target = sym(&syms, "insert");
    let open = e.list_conflicts(ws, Some(ConflictState::Open)).await.unwrap();
    assert_eq!(open.len(), 1);
    let c = &open[0];
    assert_eq!(c.symbol, target);
    let side = |s: &ConflictSide| match &s.content {
        Some(Content::Inline(t)) => t.clone(),
        other => panic!("{other:?}"),
    };
    assert!(side(&c.left).contains("// A was here") && !side(&c.left).contains("// B"));
    assert!(side(&c.right).contains("// B was here") && !side(&c.right).contains("// A"));
    assert_eq!(c.left.agent.id, "agent-a");
    assert_eq!(c.right.agent.id, "agent-b");
    // The file cannot be exported while the function is disputed.
    assert!(matches!(
        e.snapshot_export(ws, "orders").await,
        Err(holon_vcs::VcsError::UnresolvedConflict(_))
    ));

    // The resolver keeps both lines.
    let merged_fn = insert_after_open(&side(&c.left), "// B was here");
    let res = e
        .resolve_conflict(ResolutionRequest {
            workspace: ws.to_string(),
            conflict: c.id.clone(),
            resolution: Transformation::Replace(Content::Inline(merged_fn)),
            agent: agent("resolver"),
            message: None,
        })
        .await
        .unwrap();
    assert_eq!(res.outcome, PatchOutcome::Applied);
    let want = {
        let a = edit_fn(&original, &[("insert", "// A was here")]);
        // A's line is first in the body; B's goes right after the opening line too,
        // so it lands above A's.
        edit_fn(&a, &[("insert", "// B was here")])
    };
    assert_eq!(file_of(&*e, ws, "orders", REAL).await, want);

    // An agent that reads now and re-ingests the exported file changes nothing.
    let now = extract::read_point(&*e, ws).await.unwrap();
    let r = extract::ingest_file(&*e, ws, "orders", REAL, want.as_bytes(), &agent("c"), Some(now))
        .await
        .unwrap();
    assert!(r.patches.is_empty(), "{:?}", r.patches);
}

/// Structural edits on the real file: a function inserted mid-file (a placed
/// create), one appended, one renamed in place, one deleted — and the bytes are
/// still exact.
pub async fn structural_edits<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let original = real_file();
    extract::ingest_file(e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let syms = extract::extract_str("orders", REAL, &original);
    let mut text = String::new();
    for s in &syms {
        match s.id.name.as_str() {
            "is_zero" => {
                text.push_str(&s.content);
                text.push_str("\nfn inserted_mid_file() -> u32 {\n    7\n}\n");
            }
            "chunk_key" => text.push_str(&s.content.replace("fn chunk_key(", "fn chunk_key_v2(")),
            "is_chunk_key" => {} // deleted
            _ => text.push_str(&s.content),
        }
    }
    text.push_str("\nfn appended_at_end() {}\n");
    let read = extract::read_point(e, ws).await.unwrap();
    let r =
        extract::ingest_file(e, ws, "orders", REAL, text.as_bytes(), &agent("editor"), Some(read))
            .await
            .unwrap();
    assert!(r.is_clean(), "{:?}", r.patches);
    let kinds: Vec<(EditKind, &str)> =
        r.patches.iter().map(|p| (p.edit, p.symbol.name.as_str())).collect();
    assert!(kinds.contains(&(EditKind::Delete, "is_chunk_key")), "{kinds:?}");
    assert!(kinds.contains(&(EditKind::Rename, "chunk_key")), "{kinds:?}");
    assert!(kinds.contains(&(EditKind::Replace, "chunk_key_v2")), "{kinds:?}");
    assert!(kinds.contains(&(EditKind::Create, "appended_at_end")), "{kinds:?}");
    // The mid-file insert is its own symbol, placed after its neighbour.
    assert!(kinds.contains(&(EditKind::Create, "inserted_mid_file")), "{kinds:?}");
    assert!(!kinds.iter().any(|k| k.0 == EditKind::Move), "{kinds:?}");
    assert_eq!(file_of(e, ws, "orders", REAL).await, text);
    let names: Vec<String> = e
        .query_symbol(ws, SymbolQuery::Component("orders".into()))
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.id.name)
        .collect();
    let at = names.iter().position(|n| n == "inserted_mid_file").expect("its own symbol");
    assert_eq!(names[at - 1], "is_zero", "{names:?}");

    // And back to the original: exact again.
    let read = extract::read_point(e, ws).await.unwrap();
    let r = extract::ingest_file(
        e,
        ws,
        "orders",
        REAL,
        original.as_bytes(),
        &agent("editor"),
        Some(read),
    )
    .await
    .unwrap();
    assert!(r.is_clean(), "{:?}", r.patches);
    assert_eq!(file_of(e, ws, "orders", REAL).await, original);
}

/// Agent B read before agent A landed an edit; B's copy has A's function as it
/// was. B's ingest must not undo A's edit: it diffs against B's read point.
pub async fn stale_copy_keeps_others_edits<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let original = real_file();
    extract::ingest_file(e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let read = extract::read_point(e, ws).await.unwrap();
    let a = edit_fn(&original, &[("heal", "// A")]);
    extract::ingest_file(e, ws, "orders", REAL, a.as_bytes(), &agent("a"), Some(read))
        .await
        .unwrap();
    let b = edit_fn(&original, &[("count", "// B")]);
    let rb = extract::ingest_file(e, ws, "orders", REAL, b.as_bytes(), &agent("b"), Some(read))
        .await
        .unwrap();
    assert!(rb.is_clean() && rb.patches.len() == 1, "{:?}", rb.patches);
    let both = edit_fn(&original, &[("heal", "// A"), ("count", "// B")]);
    assert_eq!(file_of(e, ws, "orders", REAL).await, both);
}

/// The file with each `(after, text)` inserted right after the symbol `after`
/// (texts for the same symbol in the order given).
fn insert_after(text: &str, inserts: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for s in extract::extract_str("orders", REAL, text) {
        out.push_str(&s.content);
        for (_, t) in inserts.iter().filter(|(n, _)| s.id.name == *n) {
            out.push_str(t);
        }
    }
    out
}

const BY_A: &str = "\n/// Agent A's helper.\nfn inserted_by_a() -> u32 {\n    1\n}\n";
const BY_B: &str = "\n/// Agent B's helper.\nfn inserted_by_b() -> u32 {\n    2\n}\n";

/// Two agents, one read point, each inserting a DIFFERENT new function at the
/// SAME spot of a real file (right after `insert`). Both are their own symbols,
/// neither conflicts, and the file has both. Landing one after the other, the
/// later one's `after(insert)` resolves among the file as it is by then, so it
/// goes right after `insert`, ahead of the earlier: B's, then A's.
pub async fn two_agents_insert_at_one_spot<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let original = real_file();
    extract::ingest_file(e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let read = extract::read_point(e, ws).await.unwrap();
    let a = insert_after(&original, &[("insert", BY_A)]);
    let b = insert_after(&original, &[("insert", BY_B)]);
    let ra = extract::ingest_file(e, ws, "orders", REAL, a.as_bytes(), &agent("a"), Some(read))
        .await
        .unwrap();
    let rb = extract::ingest_file(e, ws, "orders", REAL, b.as_bytes(), &agent("b"), Some(read))
        .await
        .unwrap();
    for (r, name) in [(&ra, "inserted_by_a"), (&rb, "inserted_by_b")] {
        assert!(r.is_clean() && r.conflicts().is_empty(), "{:?}", r.patches);
        assert_eq!(r.patches.len(), 1, "{:?}", r.patches);
        assert_eq!(
            (r.patches[0].edit, r.patches[0].symbol.name.as_str()),
            (EditKind::Create, name)
        );
        assert_eq!(r.read_at, read);
    }
    // B read before A landed: its create reordered past A's.
    assert_eq!(rb.patches[0].result.as_ref().unwrap().outcome, PatchOutcome::Commuted);
    let want = insert_after(&original, &[("insert", BY_B), ("insert", BY_A)]);
    assert_eq!(file_of(e, ws, "orders", REAL).await, want);
    // Each is its own symbol, and re-ingesting the result changes nothing.
    let now = extract::read_point(e, ws).await.unwrap();
    let r = extract::ingest_file(e, ws, "orders", REAL, want.as_bytes(), &agent("c"), Some(now))
        .await
        .unwrap();
    assert!(r.patches.is_empty(), "{:?}", r.patches);
    assert!(r.unchanged.iter().any(|s| s.name == "inserted_by_a"));
    assert!(r.unchanged.iter().any(|s| s.name == "inserted_by_b"));
}

/// The same two inserts racing for real. Both land, no conflict; the file is
/// one of the two orders — the one every node computes from the log (a tie,
/// when both resolved against the same file, is broken by symbol key).
pub async fn two_agents_insert_at_one_spot_racing<B, P, G, L>(e: Arc<Engine<B, P, G, L>>, ws: &str)
where
    B: BlobStore + 'static,
    P: PointerStore + 'static,
    G: Graph + 'static,
    L: OpLog + 'static,
{
    let original = real_file();
    extract::ingest_file(&*e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let read = extract::read_point(&*e, ws).await.unwrap();
    let spawn = |copy: String, who: &'static str| {
        let e = e.clone();
        let ws = ws.to_string();
        tokio::spawn(async move {
            extract::ingest_file(&*e, &ws, "orders", REAL, copy.as_bytes(), &agent(who), Some(read))
                .await
                .unwrap()
        })
    };
    let (ra, rb) = tokio::join!(
        spawn(insert_after(&original, &[("insert", BY_A)]), "a"),
        spawn(insert_after(&original, &[("insert", BY_B)]), "b")
    );
    for r in [ra.unwrap(), rb.unwrap()] {
        assert!(r.is_clean() && r.conflicts().is_empty(), "{:?}", r.patches);
        assert_eq!(r.patches.len(), 1, "{:?}", r.patches);
    }
    let got = file_of(&*e, ws, "orders", REAL).await;
    let ab = insert_after(&original, &[("insert", BY_A), ("insert", BY_B)]);
    let ba = insert_after(&original, &[("insert", BY_B), ("insert", BY_A)]);
    assert!(got == ab || got == ba, "neither order:\n{got}");
    // What the snapshot says is what the query says.
    let names: Vec<String> = e
        .query_symbol(ws, SymbolQuery::Component("orders".into()))
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.id.name)
        .collect();
    let at = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert_eq!(at("inserted_by_a") < at("inserted_by_b"), got == ab);
    assert_eq!(at("insert") + 1, at("inserted_by_a").min(at("inserted_by_b")));
}

/// Functions reordered in a real file: `heal` moved up to just after
/// `is_zero`, and `count` (the last function) up to just after `chunk_key`.
/// Exactly those two are `move`s, the export is the edited file byte for byte,
/// and moving them back restores it.
pub async fn moved_functions<B, P, G, L>(e: &Engine<B, P, G, L>, ws: &str)
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let original = real_file();
    extract::ingest_file(e, ws, "orders", REAL, original.as_bytes(), &agent("setup"), None)
        .await
        .unwrap();
    let syms = extract::extract_str("orders", REAL, &original);
    let body = |n: &str| syms.iter().find(|s| s.id.name == n).unwrap().content.clone();
    let mut text = String::new();
    for s in &syms {
        match s.id.name.as_str() {
            "heal" | "count" => {}
            "is_zero" => {
                text.push_str(&s.content);
                text.push_str(&body("heal"));
            }
            "chunk_key" => {
                text.push_str(&s.content);
                text.push_str(&body("count"));
            }
            _ => text.push_str(&s.content),
        }
    }
    assert_ne!(text, original);
    for to in [&text, &original] {
        let read = extract::read_point(e, ws).await.unwrap();
        let r = extract::ingest_file(e, ws, "orders", REAL, to.as_bytes(), &agent("m"), Some(read))
            .await
            .unwrap();
        assert!(r.is_clean(), "{:?}", r.patches);
        let mut moved: Vec<(EditKind, &str)> =
            r.patches.iter().map(|p| (p.edit, p.symbol.name.as_str())).collect();
        moved.sort_by_key(|m| m.1);
        assert_eq!(moved, vec![(EditKind::Move, "count"), (EditKind::Move, "heal")]);
        assert_eq!(&file_of(e, ws, "orders", REAL).await, to);
    }
}

// ---- in memory ----------------------------------------------------------------------

fn mem() -> Arc<holon_vcs::MemEngine> {
    Arc::new(holon_vcs::mem_engine())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_corpus_round_trip() {
    corpus_round_trip(&*mem(), "corpus/1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_edge_cases() {
    edge_cases(&*mem(), "edge/1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_two_agents_two_functions() {
    for i in 0..10 {
        two_agents_two_functions(mem(), &format!("two/{i}")).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_two_agents_one_function() {
    two_agents_one_function(mem(), "one/1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_structural_edits() {
    structural_edits(&*mem(), "struct/1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_two_agents_insert_at_one_spot() {
    two_agents_insert_at_one_spot(&*mem(), "spot/1").await;
    for i in 0..10 {
        two_agents_insert_at_one_spot_racing(mem(), &format!("spot-race/{i}")).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_moved_functions() {
    moved_functions(&*mem(), "moved/1").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mem_stale_copy_keeps_others_edits() {
    stale_copy_keeps_others_edits(&*mem(), "stale/1").await;
}

/// Extraction alone, over every Rust file of the repository's workspaces: lossless
/// always, and how many `syn` could not split.
#[test]
fn every_rust_file_in_the_repo_round_trips() {
    let mut files = Vec::new();
    for dir in ["crates", "components", "host", "reconciler", "cli", "lattice", "xtask"] {
        files.extend(files_under(dir, ".rs"));
    }
    assert!(files.len() > 300, "{}", files.len());
    let mut whole = Vec::new();
    let mut symbols = 0;
    for (p, b) in &files {
        let Ok(s) = extract::extract("repo", p, b) else { continue };
        let joined: Vec<u8> = s.iter().flat_map(|s| s.content.bytes()).collect();
        assert_eq!(&joined, b, "{p}");
        symbols += s.len();
        if s.len() == 1 && s[0].id.kind == SymbolKind::File && !b.is_empty() {
            whole.push(p.clone());
        }
    }
    eprintln!("repo: {} .rs files, {symbols} symbols, kept whole: {whole:?}", files.len());
}

// ---- live ---------------------------------------------------------------------------

#[cfg(feature = "native")]
mod live {
    use super::*;
    use holon_vcs::nats::{self, NatsBlobs, NatsConfig, NatsKv};
    use holon_vcs::oplog::KvOpLog;
    use holon_vcs::store::KvPointers;
    use holon_vcs::surreal::{SurrealConfig, SurrealGraph};

    type Live = Engine<NatsBlobs, KvPointers<NatsKv>, SurrealGraph, KvOpLog<NatsKv>>;

    async fn make(name: &str) -> Option<(Arc<Live>, String)> {
        let (Ok(nats_url), Ok(surreal_url)) =
            (std::env::var("HOLON_VCS_NATS_URL"), std::env::var("HOLON_VCS_SURREAL_URL"))
        else {
            eprintln!("SKIPPED live {name}: set HOLON_VCS_NATS_URL and HOLON_VCS_SURREAL_URL");
            return None;
        };
        let prefix =
            std::env::var("HOLON_VCS_BUCKET_PREFIX").unwrap_or_else(|_| "holon-vcs-test".into());
        let (blobs, pointers, log) =
            nats::connect(&nats_url, &NatsConfig::prefixed(&prefix)).await.expect("nats");
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let mut cfg = SurrealConfig::new(&surreal_url);
        cfg.namespace = "holon_vcs_test".into();
        // One database per run, as in `tests/live.rs`.
        cfg.database = format!("extract_{}_{nanos}", std::process::id());
        let graph = SurrealGraph::connect(&cfg).await.expect("surreal");
        let ws = format!("{name}/{}-{nanos}", std::process::id());
        Some((Arc::new(Engine::new(blobs, pointers, graph, log)), ws))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_scenarios() {
        let Some((e, ws)) = make("extract").await else { return };
        edge_cases(&*e, &format!("{ws}-edge")).await;
        for i in 0..3 {
            two_agents_two_functions(e.clone(), &format!("{ws}-two-{i}")).await;
        }
        two_agents_one_function(e.clone(), &format!("{ws}-one")).await;
        structural_edits(&*e, &format!("{ws}-struct")).await;
        stale_copy_keeps_others_edits(&*e, &format!("{ws}-stale")).await;
        two_agents_insert_at_one_spot(&*e, &format!("{ws}-spot")).await;
        for i in 0..3 {
            two_agents_insert_at_one_spot_racing(e.clone(), &format!("{ws}-spot-race-{i}")).await;
        }
        moved_functions(&*e, &format!("{ws}-moved")).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_corpus_round_trip() {
        let Some((e, ws)) = make("extract-corpus").await else { return };
        corpus_round_trip(&*e, &ws).await;
    }
}
