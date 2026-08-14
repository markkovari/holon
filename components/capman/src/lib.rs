//! The capability manifest — the source of truth for what the engine can do and
//! at what version. The self-improvement loop edits `capabilities.txt`; this
//! validates it. A deployed version advertises exactly these, over the lattice,
//! and the self-improve gate promotes a candidate only if it advanced one.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn is_semver(v: &str) -> bool {
        let p: Vec<&str> = v.split('.').collect();
        p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.parse::<u64>().is_ok())
    }

    /// The gate the loop's edit must pass: a manifest that still parses, names no
    /// capability twice, and versions everything with a real semver. It does NOT
    /// check that anything improved — the lattice is the judge of that.
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
}
