//! The capability manifest — the source of truth for what the engine can do and
//! at what version. The self-improvement loop edits `capabilities.txt`; this
//! validates it. A deployed version advertises exactly these, over the lattice,
//! and the self-improve gate promotes a candidate only if it advanced one.
//!
//! A version is not just a number: `conforms` says what BEHAVIOR each version of
//! each capability must exhibit, and the gate refuses a manifest that claims a
//! version the code does not actually implement. So bumping `slug` to `1.1.0` in
//! the manifest without making `slugify` do the 1.1.0 thing fails the gate.

pub mod slug;

pub const MANIFEST: &str = include_str!("../capabilities.txt");

/// Parse `name:semver` lines, ignoring blanks and `#` comments.
pub fn parse(s: &str) -> Vec<(String, String)> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_string(), v.trim().to_string())))
        .collect()
}

/// The manifest as the comma-separated `name:semver` list the probe bakes in.
pub fn as_caps(s: &str) -> String {
    parse(s).into_iter().map(|(n, v)| format!("{n}:{v}")).collect::<Vec<_>>().join(",")
}

/// A `major.minor.patch` as a comparable tuple (missing parts are 0).
fn triple(v: &str) -> (u64, u64, u64) {
    let p: Vec<u64> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    (p.first().copied().unwrap_or(0), p.get(1).copied().unwrap_or(0), p.get(2).copied().unwrap_or(0))
}

/// The behavior a capability must exhibit AT LEAST at `version`. This is what
/// makes a version more than a number. A capability with no entry here is a
/// declared capability with no conformance yet — advertised as-is.
pub fn conforms(name: &str, version: &str) -> Result<(), String> {
    let ver = triple(version);
    match name {
        "slug" => {
            // 1.0.0 — basic slugging.
            if slug::slugify("Hello, World!") != "hello-world" {
                return Err("slug 1.0.0: lowercase, collapse punctuation to hyphens".into());
            }
            // 1.1.0 — fold accents to ASCII.
            if ver >= (1, 1, 0) && slug::slugify("Café Déjà Señor") != "cafe-deja-senor" {
                return Err("slug 1.1.0: fold accented letters to ASCII".into());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_semver(v: &str) -> bool {
        let p: Vec<&str> = v.split('.').collect();
        p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.parse::<u64>().is_ok())
    }

    /// The manifest still parses, names no capability twice, and versions
    /// everything with a real semver.
    #[test]
    fn the_manifest_is_wellformed() {
        let caps = parse(MANIFEST);
        assert!(!caps.is_empty(), "the engine must advertise at least one capability");
        for (n, v) in &caps {
            assert!(!n.is_empty(), "a capability needs a name");
            assert!(is_semver(v), "capability {n:?} has a non-semver version {v:?}");
        }
        let mut names: Vec<&String> = caps.iter().map(|(n, _)| n).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), caps.len(), "a capability is named twice");
    }

    /// THE TIE. Every version the manifest claims must be backed by the behavior
    /// for that version — a number the code cannot deliver is refused here.
    #[test]
    fn every_manifest_version_is_backed_by_its_behavior() {
        for (name, version) in parse(MANIFEST) {
            conforms(&name, &version)
                .unwrap_or_else(|e| panic!("manifest claims {name}:{version}, but behavior fails: {e}"));
        }
    }
}
