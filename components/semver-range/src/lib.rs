//! `semver-range` — does a version satisfy a range?
//! `1.1.0-alpha`, `<1.0.0` accepts `1.0.0-alpha`. Build metadata is dropped (SemVer §10).

/// Numeric identifiers compare as NUMBERS (`alpha.2` < `alpha.10`), and alphanumeric beats
/// numeric (SemVer §11.4.3).
#[derive(Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Ident {
    fn parse(s: &str) -> Self {
        // Leading zeroes make it text: `01` as 1 would make two distinct versions compare equal.
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

    /// Tolerates a missing minor or patch so `~1.2` parses; build metadata is discarded.
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
        // Absent is zero (`~1.2` is `1.2.0`); present but not a number is a failure.
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

    /// `~1.2` and `~1.2.0` have different upper bounds, so what was typed survives parsing.
    fn written_parts(s: &str) -> usize {
        s.trim().split('+').next().unwrap_or("").split('-').next().unwrap_or("").split('.').count()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.triple().cmp(&other.triple()).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                // A shorter set is lower once every shared identifier is equal (SemVer §11.4.4).
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

/// Locked at the leftmost NONZERO part: treating `^` as 'same major' is the costly bug on 0.x.
fn caret(v: Version) -> Vec<Comparator> {
    // Saturates: input has no upper bound, and 'nothing is above this' is right at `u64::MAX`.
    let upper = if v.major > 0 {
        Version { major: v.major.saturating_add(1), minor: 0, patch: 0, pre: vec![] }
    } else if v.minor > 0 {
        Version { major: 0, minor: v.minor.saturating_add(1), patch: 0, pre: vec![] }
    } else {
        Version { major: 0, minor: 0, patch: v.patch.saturating_add(1), pre: vec![] }
    };
    vec![Comparator { op: Op::Gte, version: v }, Comparator { op: Op::Lt, version: upper }]
}

/// `~1.2`: the minor was never written, so the minor may move.
fn tilde(v: Version, written: usize) -> Vec<Comparator> {
    let upper = if written >= 2 {
        Version { major: v.major, minor: v.minor.saturating_add(1), patch: 0, pre: vec![] }
    } else {
        Version { major: v.major.saturating_add(1), minor: 0, patch: 0, pre: vec![] }
    };
    vec![Comparator { op: Op::Gte, version: v }, Comparator { op: Op::Lt, version: upper }]
}

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
    Some(vec![Comparator { op: Op::Eq, version: Version::parse(term)? }])
}

/// `*`, exact, `^`, `~`, `>=`, `<`, and comma-separated conjunctions of those.
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
        // `*` names no triple, so it admits no pre-release.
        return saw_star && v.pre.is_empty();
    }

    if !v.pre.is_empty() && !comparators.iter().any(|c| c.version.triple() == v.triple()) {
        return false;
    }

    comparators.iter().all(|c| c.admits(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

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
