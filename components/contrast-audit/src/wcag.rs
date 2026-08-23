//! WCAG 2.1 contrast, computed here rather than believed.
//!
//! No WASI, so it is host-testable — the same split `anthropic-provider`'s
//! `codec` uses, and for the same reason: this is the part that can be wrong in a
//! way no integration test would notice.
//!
//! ## Why the component recomputes what the browser already sent
//!
//! The page measures pixels and computes ratios to draw its own swatches, and
//! those numbers arrive in the request body. They are NOT what gets audited. A
//! ratio is a claim from outside the trust boundary, and a model handed
//! `"ratio": 21` for two greys would dutifully explain why that pair is fine.
//! The hex pair is the only thing taken on trust here, and even that is parsed
//! strictly; every number in the prompt is derived from it by this module.

/// One colour pair and what the maths says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub fg: String,
    pub bg: String,
    /// WCAG contrast ratio, 1.0 ..= 21.0.
    pub ratio: f64,
    /// Share of sampled pixels this pair covers, as the page reported it. Kept
    /// because "this combination is everywhere" is a priority signal the maths
    /// cannot supply — and it is only ever a hint, never a threshold.
    pub share: f64,
}

impl Pair {
    /// 4.5:1 — normal body text.
    pub fn passes_aa(&self) -> bool {
        self.ratio >= 4.5
    }
    /// 3:1 — text at 18pt+, or 14pt+ bold, and UI component boundaries.
    pub fn passes_aa_large(&self) -> bool {
        self.ratio >= 3.0
    }
    /// 7:1 — the stricter grade.
    pub fn passes_aaa(&self) -> bool {
        self.ratio >= 7.0
    }
    /// What a report should call it.
    pub fn verdict(&self) -> &'static str {
        if self.passes_aaa() {
            "AAA"
        } else if self.passes_aa() {
            "AA"
        } else if self.passes_aa_large() {
            "AA large text only"
        } else {
            "fails"
        }
    }
}

/// `#rgb` or `#rrggbb`, case-insensitive, `#` optional. Anything else is None.
///
/// Strict on purpose: a colour that half-parses becomes a ratio that is wrong
/// rather than absent, and a wrong ratio is what this whole module exists to
/// prevent. Three-digit form expands by DOUBLING each nibble (`#abc` -> `#aabbcc`)
/// — the CSS rule. Scaling by 17 is the same thing; multiplying by 16 is the
/// off-by-a-shade version of it.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().trim_start_matches('#');
    let d: Vec<u8> =
        h.bytes().map(|b| (b as char).to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    match d.len() {
        3 => Some((d[0] * 17, d[1] * 17, d[2] * 17)),
        6 => Some((d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5])),
        _ => None,
    }
}

/// Canonical `#rrggbb`, so the prompt and the report agree on how a colour is
/// spelled even when the page sent `#ABC`.
pub fn to_hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

/// One sRGB channel, linearised. The threshold is WCAG's 0.03928.
fn linearise(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Relative luminance, 0.0 (black) ..= 1.0 (white).
pub fn luminance(rgb: (u8, u8, u8)) -> f64 {
    0.2126 * linearise(rgb.0) + 0.7152 * linearise(rgb.1) + 0.0722 * linearise(rgb.2)
}

/// The contrast ratio between two colours, 1.0 ..= 21.0.
///
/// Symmetric: which one is the text does not change the number, which is why the
/// lighter of the two goes on top rather than the first argument.
pub fn ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Build the audited pairs from what the page claimed, dropping what cannot be
/// parsed and recomputing every ratio.
///
/// `claims` is `(fg, bg, share)` as received. The share is carried through
/// unchanged but clamped: it is a hint for ordering, and a negative or >1 share
/// would sort a made-up pair to the top of the report.
pub fn audit(claims: &[(String, String, f64)]) -> Vec<Pair> {
    let mut out = Vec::new();
    for (fg, bg, share) in claims {
        let (Some(f), Some(b)) = (parse_hex(fg), parse_hex(bg)) else { continue };
        // A colour against itself is not a finding, it is a sampling artefact:
        // ratio 1.0, and reporting it as a failure buries the real ones.
        if f == b {
            continue;
        }
        out.push(Pair {
            fg: to_hex(f),
            bg: to_hex(b),
            ratio: ratio(f, b),
            share: share.clamp(0.0, 1.0),
        });
    }
    // Worst contrast first: the report's order IS its priority, and a caller that
    // truncates the list must truncate the least important end of it.
    out.sort_by(|a, b| a.ratio.partial_cmp(&b.ratio).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two anchors of the scale. If either of these drifts, every ratio in
    /// every report is wrong by the same factor and nothing else would say so.
    #[test]
    fn black_on_white_is_twenty_one_to_one() {
        let r = ratio((0, 0, 0), (255, 255, 255));
        assert!((r - 21.0).abs() < 0.01, "black on white is 21:1, got {r}");
        // A colour against itself is 1:1, and the formula must not need a special
        // case to say so.
        assert!((ratio((18, 52, 86), (18, 52, 86)) - 1.0).abs() < 1e-12);
    }

    /// Symmetry. The caller decides which colour is the text; the maths must not.
    #[test]
    fn the_ratio_does_not_care_which_colour_is_the_text() {
        let (a, b) = ((0x11, 0x22, 0x33), (0xdd, 0xee, 0xff));
        assert!((ratio(a, b) - ratio(b, a)).abs() < 1e-12);
    }

    /// The threshold cases, because "4.5" is the whole point of the report and an
    /// off-by-one-shade parse is how a failing pair reads as passing.
    #[test]
    fn the_grades_land_on_the_right_side_of_the_thresholds() {
        // #767676 on white is the canonical "just passes AA" grey (4.54:1).
        let grey = Pair {
            fg: "#767676".into(),
            bg: "#ffffff".into(),
            ratio: ratio((0x76, 0x76, 0x76), (255, 255, 255)),
            share: 0.1,
        };
        assert!(grey.passes_aa(), "#767676 on white passes AA, got {}", grey.ratio);
        assert!(!grey.passes_aaa());
        assert_eq!(grey.verdict(), "AA");
        // #949494 on white is the canonical "large text only" grey (3.03:1).
        let light = Pair {
            fg: "#949494".into(),
            bg: "#ffffff".into(),
            ratio: ratio((0x94, 0x94, 0x94), (255, 255, 255)),
            share: 0.1,
        };
        assert!(!light.passes_aa(), "ratio {}", light.ratio);
        assert!(light.passes_aa_large());
        assert_eq!(light.verdict(), "AA large text only");
    }

    /// `#abc` expands by doubling the nibble, not by shifting it.
    #[test]
    fn short_hex_expands_the_way_css_says() {
        assert_eq!(parse_hex("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("ABC"), parse_hex("#aabbcc"));
        assert_eq!(parse_hex("#123456"), Some((0x12, 0x34, 0x56)));
    }

    /// Anything that is not a colour is not a colour. A half-parsed hex is the
    /// failure this guards: it yields a plausible ratio for a colour nobody sent.
    #[test]
    fn a_colour_that_is_not_one_is_rejected_rather_than_guessed() {
        for bad in ["", "#", "#12", "#12345", "#1234567", "#gggggg", "rebeccapurple", "#12 34 56"] {
            assert_eq!(parse_hex(bad), None, "{bad:?} is not a hex colour");
        }
    }

    /// The audit ignores the ratio it was handed.
    ///
    /// This is the trust boundary. The page says these two greys are 21:1; they
    /// are not, and the report must say what the maths says.
    #[test]
    fn a_ratio_from_the_client_is_recomputed_not_trusted() {
        let audited = audit(&[("#777777".into(), "#808080".into(), 0.5)]);
        assert_eq!(audited.len(), 1);
        assert!(
            audited[0].ratio < 1.2,
            "two near-identical greys cannot be 21:1, got {}",
            audited[0].ratio
        );
        assert_eq!(audited[0].verdict(), "fails");
    }

    /// Unparseable pairs are dropped, identical pairs are dropped, and what
    /// survives is ordered worst-first so a truncated report keeps the findings.
    #[test]
    fn the_audit_drops_noise_and_reports_the_worst_first() {
        let audited = audit(&[
            ("#000000".into(), "#ffffff".into(), 0.4), // 21:1, best
            ("#ff0000".into(), "not-a-colour".into(), 0.3), // dropped
            ("#333333".into(), "#333333".into(), 0.2), // same colour, dropped
            ("#888888".into(), "#999999".into(), 9.0), // awful, and a bogus share
        ]);
        assert_eq!(audited.len(), 2, "two of the four are noise: {audited:?}");
        assert_eq!(audited[0].fg, "#888888", "worst contrast comes first");
        assert_eq!(audited[0].share, 1.0, "a share above 1 is clamped, not believed");
        assert_eq!(audited[1].verdict(), "AAA");
    }
}
