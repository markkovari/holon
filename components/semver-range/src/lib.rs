//! `semver-range` — does a version satisfy a range?
//!
//! `tests/ranges.rs` is the specification and is not writable from here. Every
//! case in it is a rule people get wrong, and most of them are the pre-release
//! rules — a matcher that handles `^1.2.3` and nothing else looks finished and
//! is not.
//!
//! ## The pre-release rule, which is the whole difficulty
//!
//! A version carrying a pre-release is only considered at all when some
//! comparator in the range shares its `[major, minor, patch]`. Otherwise the
//! answer is `false` before any ordering is done.
//!
//! That one sentence is what makes `^1.0.0` refuse `1.1.0-alpha`. Expanded,
//! `^1.0.0` is `>=1.0.0, <2.0.0`, and `1.1.0-alpha` sits inside that interval —
//! so an implementation that only compares would admit it. It must not: nobody
//! asking for `^1.0.0` is asking to be handed an unreleased 1.1.0. The range has
//! to have NAMED that triple somewhere before its pre-releases are in scope.
//!
//! And it is the same sentence that lets `<1.0.0` accept `1.0.0-alpha`, which
//! looks like the opposite behaviour and is not: `<1.0.0` names the triple
//! `1.0.0`, so `1.0.0-alpha` is in scope, and ordinary ordering then puts it
//! below its own release. Both cases fall out of one rule rather than two, which
//! is why it is written as one.
//!
//! ## Build metadata
//!
//! Dropped on sight, from both sides. SemVer §10: build metadata is not part of
//! precedence, so `1.0.0+build.5` IS `1.0.0` for every question asked here.

/// One dot-separated piece of a pre-release tag.
///
/// The distinction is load-bearing twice over: numeric identifiers compare as
/// NUMBERS (so `alpha.2` is below `alpha.10`, which a string comparison gets
/// backwards), and when the kinds differ the alphanumeric one is the greater
/// (SemVer §11.4.3).
#[derive(Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Ident {
    fn parse(s: &str) -> Self {
        // Leading zeroes make it a string, not a number: `01` is not `1`, and
        // treating it as one would make two distinct versions compare equal.
        if !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && (s.len() == 1 || !s.starts_with('0'))
        {
            s.parse().map(Ident::Num).unwrap_or_else(|_| Ident::Text(s.to_string()))
        } else {
            Ident::Text(s.to_string())
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Ident::Num(_) => 0,
            Ident::Text(_) => 1,
        }
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
            // Different kinds: numeric is always the lower one.
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Ident>,
}

impl Version {
    fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    /// `major.minor.patch` with an optional `-pre`, tolerating a missing minor
    /// or patch so `~1.2` can be written as a version.
    ///
    /// Build metadata is discarded here rather than stored, because nothing
    /// downstream is allowed to look at it.
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let core = s.split('+').next().unwrap_or(s);
        let (core, pre) = match core.split_once('-') {
            Some((c, p)) => (c, p.split('.').map(Ident::parse).collect()),
            None => (core, Vec::new()),
        };

        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        // An absent minor or patch is zero — `~1.2` means `1.2.0` — but a
        // PRESENT one that is not a number is a parse failure, not a zero.
        let minor = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Version { major, minor, patch, pre })
    }

    /// How many of `major.minor.patch` were actually written.
    ///
    /// `~1.2` and `~1.2.0` expand to different upper bounds, so the shape of what
    /// was typed survives parsing.
    fn written_parts(s: &str) -> usize {
        s.trim().split('+').next().unwrap_or("").split('-').next().unwrap_or("").split('.').count()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.triple().cmp(&other.triple()).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release outranks any pre-release of the same triple.
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                // Field by field, and a shorter set is lower once every shared
                // identifier is equal (SemVer §11.4.4).
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
}

struct Comparator {
    op: Op,
    version: Version,
}

impl Comparator {
    fn admits(&self, v: &Version) -> bool {
        let ord = v.cmp(&self.version);
        match self.op {
            Op::Lt => ord.is_lt(),
            Op::Lte => ord.is_le(),
            Op::Gt => ord.is_gt(),
            Op::Gte => ord.is_ge(),
            Op::Eq => ord.is_eq(),
        }
    }
}

/// `^1.2.3` — compatible within the leftmost NONZERO part.
///
/// Which is why `^0.2.3` is minor-locked and `^0.0.3` is patch-locked: with a
/// zero major the minor is carrying the compatibility promise, and with a zero
/// minor the patch is. Treating `^` as "same major" is the common bug, and on a
/// 0.x dependency it is the expensive one.
fn caret(v: Version) -> Vec<Comparator> {
    let upper = if v.major > 0 {
        Version { major: v.major + 1, minor: 0, patch: 0, pre: vec![] }
    } else if v.minor > 0 {
        Version { major: 0, minor: v.minor + 1, patch: 0, pre: vec![] }
    } else {
        Version { major: 0, minor: 0, patch: v.patch + 1, pre: vec![] }
    };
    vec![Comparator { op: Op::Gte, version: v }, Comparator { op: Op::Lt, version: upper }]
}

/// `~1.2.3` — patch-level changes. `~1.2` — the minor was never written, so the
/// minor is what may move.
fn tilde(v: Version, written: usize) -> Vec<Comparator> {
    let upper = if written >= 2 {
        Version { major: v.major, minor: v.minor + 1, patch: 0, pre: vec![] }
    } else {
        Version { major: v.major + 1, minor: 0, patch: 0, pre: vec![] }
    };
    vec![Comparator { op: Op::Gte, version: v }, Comparator { op: Op::Lt, version: upper }]
}

/// One comma-separated term into the comparators it stands for.
///
/// Returns `None` for anything unparseable, and the caller turns that into a
/// plain `false` — a range nobody can read matches nothing, rather than panics.
fn parse_term(term: &str) -> Option<Vec<Comparator>> {
    let term = term.trim();
    if term.is_empty() {
        return None;
    }
    if term == "*" {
        return Some(Vec::new());
    }

    // Longest operator first: `>=` must be tried before `>`.
    for (prefix, op) in
        [(">=", Op::Gte), ("<=", Op::Lte), (">", Op::Gt), ("<", Op::Lt), ("=", Op::Eq)]
    {
        if let Some(rest) = term.strip_prefix(prefix) {
            let v = Version::parse(rest)?;
            return Some(vec![Comparator { op, version: v }]);
        }
    }
    if let Some(rest) = term.strip_prefix('^') {
        return Some(caret(Version::parse(rest)?));
    }
    if let Some(rest) = term.strip_prefix('~') {
        return Some(tilde(Version::parse(rest)?, Version::written_parts(rest)));
    }
    // A bare version is an exact match.
    Some(vec![Comparator { op: Op::Eq, version: Version::parse(term)? }])
}

/// Does `version` satisfy `range`?
///
/// Ranges: `*`, `1.2.3`, `^1.2.3`, `~1.2.3`, `~1.2`, `>=1.0.0`, `<2.0.0`, and
/// comma-separated conjunctions of those.
pub fn matches(range: &str, version: &str) -> bool {
    let Some(v) = Version::parse(version) else { return false };

    let mut comparators = Vec::new();
    let mut saw_star = false;
    for term in range.split(',') {
        match parse_term(term) {
            Some(cs) if cs.is_empty() => saw_star = true,
            Some(cs) => comparators.extend(cs),
            None => return false,
        }
    }
    if comparators.is_empty() {
        // `*` admits any release. A pre-release still needs a range that named
        // its triple, and `*` names nothing.
        return saw_star && v.pre.is_empty();
    }

    // THE pre-release gate. Before any comparison: a pre-release is only in
    // scope when the range itself mentioned this exact triple.
    if !v.pre.is_empty() && !comparators.iter().any(|c| c.version.triple() == v.triple()) {
        return false;
    }

    comparators.iter().all(|c| c.admits(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering rules, tested directly rather than through `matches`.
    ///
    /// `tests/ranges.rs` reaches these through ranges, which is the contract;
    /// this reaches them head-on, so a failure says "comparison is wrong" rather
    /// than "some range is wrong".
    #[test]
    fn precedence_follows_semver_11() {
        let v = |s: &str| Version::parse(s).expect(s);
        assert!(v("1.0.0-alpha") < v("1.0.0"), "a release outranks its pre-release");
        assert!(v("1.0.0-alpha.2") < v("1.0.0-alpha.10"), "numeric identifiers compare as numbers");
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"), "alphanumeric outranks numeric");
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"), "a shorter identifier set is lower");
        assert_eq!(v("1.0.0+build.5"), v("1.0.0"), "build metadata is not precedence");
        assert!(v("1.0.0") < v("1.0.1") && v("1.0.1") < v("1.1.0") && v("1.1.0") < v("2.0.0"));
    }

    #[test]
    fn a_leading_zero_makes_an_identifier_textual() {
        // `01` is not the number 1: treating it as one would make two distinct
        // pre-releases compare equal.
        assert_eq!(Ident::parse("01"), Ident::Text("01".into()));
        assert_eq!(Ident::parse("1"), Ident::Num(1));
        assert_eq!(Ident::parse("0"), Ident::Num(0));
    }

    #[test]
    fn a_version_with_too_many_parts_is_not_a_version() {
        assert!(Version::parse("1.2.3.4").is_none());
        assert!(Version::parse("1.x.3").is_none());
        assert!(Version::parse("1.2").is_some(), "a missing patch is zero");
    }
}
