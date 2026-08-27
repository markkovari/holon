//! Agreement with the reference implementation, over its own test data.
//!
//! The held-out specification in `validate.rs` says what this component should do.
//! This says something narrower and harder to fake: that it does not DISAGREE with
//! cucumber about whether a file is valid Gherkin.
//!
//! It is the check that corrected six mistakes, listed in `corpus/README.md`. Every
//! one was mine; the corpus was right each time. Three of them were assertions in
//! the held-out spec, written from a reading of the grammar rather than from the
//! implementation that defines it.

use gherkin_validate::{validate, Severity};

fn features(which: &str) -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus").join(which);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "feature"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            // Lossy on purpose: one of these files is deliberately full of emoji.
            let bytes = std::fs::read(&p).unwrap();
            (name, String::from_utf8_lossy(&bytes).to_string())
        })
        .collect()
}

fn errors(source: &str) -> Vec<String> {
    validate(source)
        .into_iter()
        .filter(|p| p.severity() == Severity::Error)
        .map(|p| format!("{}:{:?}", p.line, p.kind))
        .collect()
}

/// Nothing cucumber accepts may be called broken here.
///
/// A `warning` is fine — those are the files that parse and mean nothing. So is a
/// `declined`, which is the five files written in a language this component does not
/// read. An `error` is the claim "cucumber cannot parse this", and on these files
/// that claim is false.
#[test]
fn no_file_the_reference_accepts_is_reported_as_broken() {
    let files = features("good");
    assert!(files.len() >= 49, "the corpus is missing: only {} files", files.len());
    let disagreements: Vec<String> = files
        .iter()
        .filter_map(|(name, src)| {
            let e = errors(src);
            (!e.is_empty()).then(|| format!("  {name}: {}", e.join(" | ")))
        })
        .collect();
    assert!(
        disagreements.is_empty(),
        "{} of {} files cucumber parses are reported as errors:\n{}",
        disagreements.len(),
        files.len(),
        disagreements.join("\n")
    );
}

/// And everything it refuses has to be caught.
///
/// A warning is not enough here. `whitespace_in_tags.feature` used to produce only
/// an "empty scenario" warning, which would have let a genuinely broken file through
/// a gate that acts on errors.
#[test]
fn every_file_the_reference_refuses_is_caught() {
    let files = features("bad");
    assert!(files.len() >= 12, "the corpus is missing: only {} files", files.len());
    let missed: Vec<String> = files
        .iter()
        .filter_map(|(name, src)| {
            errors(src).is_empty().then(|| {
                let others: Vec<String> = validate(src)
                    .into_iter()
                    .map(|p| format!("{:?}({:?})", p.kind, p.severity()))
                    .collect();
                format!("  {name}: no error, only [{}]", others.join(" | "))
            })
        })
        .collect();
    assert!(
        missed.is_empty(),
        "{} of {} files cucumber refuses are not caught:\n{}",
        missed.len(),
        files.len(),
        missed.join("\n")
    );
}
