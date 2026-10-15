//! The pure half of `concept2-logbook`: turn a Concept2 logbook page into the short text a small
//! model reads whole. No I/O and no bindings, so it is unit-tested on the host.
//!
//! It is a hand port of the regular expressions in the Python connector this replaces, and the
//! output is meant to be IDENTICAL to what that script printed. The page is untrusted data from
//! another site, so every function here is total: nothing indexes past a length it did not check,
//! and a page that does not look like a logbook yields a plain "nothing found" rather than a panic.

/// The one host this component talks to.
pub const HOST: &str = "log.concept2.com";

/// `profile` is the number in the logbook URL: 1 to 12 digits.
pub fn valid_profile(s: &str) -> bool {
    (1..=12).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// A workout id, as `list` prints it: 4 to 12 digits.
pub fn valid_id(s: &str) -> bool {
    (4..=12).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// `limit`, clamped to 1..=30 like the script did; `None` when it is not a number.
pub fn clamp_limit(s: Option<&str>) -> Option<usize> {
    match s {
        None => Some(10),
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
            Some(v.parse::<usize>().unwrap_or(30).clamp(1, 30))
        }
        Some(_) => None,
    }
}

/// `page`, at least 1; `None` when it is not a number.
pub fn clamp_page(s: Option<&str>) -> Option<usize> {
    match s {
        None => Some(1),
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => {
            Some(v.parse::<usize>().unwrap_or(usize::MAX).max(1))
        }
        Some(_) => None,
    }
}

pub fn list_path(profile: &str, page: usize) -> String {
    if page > 1 {
        format!("/profile/{profile}/log?page={page}")
    } else {
        format!("/profile/{profile}/log")
    }
}

pub fn workout_path(profile: &str, id: &str) -> String {
    format!("/profile/{profile}/log/{id}")
}

/// What to do with a request, decided before anything is fetched.
#[derive(Debug, PartialEq)]
pub enum Plan {
    List {
        profile: String,
        limit: usize,
        page: usize,
    },
    Workout {
        profile: String,
        id: String,
    },
    /// A 400, with the reason.
    Bad(String),
    NotFound,
    MethodNotAllowed,
}

fn param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v)
}

/// `GET /list?profile=…[&limit=…][&page=…]` and `GET /workout/<id>?profile=…`; anything else is
/// refused. Nothing but digits ever reaches the path that is fetched, so the request cannot steer
/// the fetch anywhere but a logbook page.
pub fn plan(is_get: bool, route: &str, query: &str) -> Plan {
    let route = route.trim_end_matches('/');
    let wants_workout = route.strip_prefix("/workout/");
    if route != "/list" && wants_workout.is_none() {
        return Plan::NotFound;
    }
    if !is_get {
        return Plan::MethodNotAllowed;
    }
    let Some(profile) = param(query, "profile").filter(|p| valid_profile(p)) else {
        return Plan::Bad("`profile` must be the digits in your logbook URL (1 to 12)".into());
    };
    let profile = profile.to_string();
    match wants_workout {
        Some(id) if valid_id(id) => Plan::Workout { profile, id: id.to_string() },
        Some(_) => Plan::Bad("the workout id must be the 4 to 12 digits `list` prints".into()),
        None => match (clamp_limit(param(query, "limit")), clamp_page(param(query, "page"))) {
            (Some(limit), Some(page)) => Plan::List { profile, limit, page },
            _ => Plan::Bad("`limit` and `page` must be whole numbers".into()),
        },
    }
}

/// Where a redirect may go: a path on the same host, written relative or in full. Anything else
/// (another host, a scheme-relative `//x`, plain http) is refused, so a redirect cannot be used to
/// reach a host this component was never meant to.
pub fn redirect_target(location: &str) -> Option<String> {
    if location.starts_with('/') && !location.starts_with("//") {
        return Some(location.to_string());
    }
    let rest = location.strip_prefix("https://")?.strip_prefix(HOST)?;
    match rest.chars().next() {
        None => Some("/".to_string()),
        Some('/') => Some(rest.to_string()),
        _ => None, // `log.concept2.com.evil.example`, `log.concept2.com:444`
    }
}

/// Python's `str.isspace`, which is what `\s` and `.strip()` use on text.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Everything between the first `open` at or after `from` and the next `close`, removed. `None`
/// when there is no such pair (a lone `<script` with no end is left as it is, like the regex).
fn cut_block(s: &str, from: usize, open: &str, close: &str) -> Option<(usize, usize)> {
    let start = from + s[from..].find(open)?;
    let end = start + s[start..].find(close)? + close.len();
    Some((start, end))
}

/// `<script>…</script>` and `<style>…</style>` blocks replaced by a space.
fn drop_blocks(h: &str) -> String {
    let mut out = String::with_capacity(h.len());
    let mut pos = 0;
    loop {
        let script = cut_block(h, pos, "<script", "</script>");
        let style = cut_block(h, pos, "<style", "</style>");
        let next = match (script, style) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        match next {
            Some((start, end)) => {
                out.push_str(&h[pos..start]);
                out.push(' ');
                pos = end;
            }
            None => {
                out.push_str(&h[pos..]);
                return out;
            }
        }
    }
}

/// Every `<…>` replaced by a space. A `<` with no `>` after it, or `<>`, is left as text.
fn strip_tags(h: &str) -> String {
    let mut out = String::with_capacity(h.len());
    let mut rest = h;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('>') {
            Some(j) if j > 0 => {
                out.push(' ');
                rest = &after[j + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Windows-1252 for the C1 range, which is what a browser (and Python's `html.unescape`) makes of
/// `&#150;` and friends.
fn cp1252(n: u32) -> Option<char> {
    const TABLE: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    TABLE.get(n.checked_sub(0x80)? as usize).and_then(|c| char::from_u32(*c))
}

fn numeric_entity(body: &str) -> Option<char> {
    let n = match body.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => body.parse::<u32>().ok()?,
    };
    Some(match n {
        0 => '\u{FFFD}',
        0x80..=0x9F => cp1252(n)?,
        _ => char::from_u32(n).unwrap_or('\u{FFFD}'),
    })
}

/// The named entities a logbook page uses, with their terminating `;`. A subset of HTML5, on
/// purpose: the page is ASCII apart from these.
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "hellip" => '\u{2026}',
        "middot" => '\u{b7}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "deg" => '\u{b0}',
        _ => return None,
    })
}

/// `&amp;`, `&#38;`, `&#x26;` and the legacy names that browsers also accept without the `;`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end =
            after.find(|c: char| !c.is_ascii_alphanumeric() && c != '#').unwrap_or(after.len());
        let (name, tail) = after.split_at(end);
        let terminated = tail.starts_with(';');
        let decoded = if let Some(n) = name.strip_prefix('#') {
            numeric_entity(n)
        } else if terminated {
            named_entity(name)
        } else {
            // Without the `;`, only the five names the HTML spec keeps for old pages.
            matches!(name, "amp" | "lt" | "gt" | "quot" | "nbsp")
                .then(|| named_entity(name))
                .flatten()
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = if terminated { &tail[1..] } else { tail };
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The visible text of a page: scripts and styles dropped, tags removed, entities decoded, runs of
/// whitespace made one space.
pub fn text(h: &str) -> String {
    let flat = unescape(&strip_tags(&drop_blocks(h)));
    let mut out = String::with_capacity(flat.len());
    let mut gap = false;
    for c in flat.chars() {
        if is_space(c) {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out
}

/// Each `<tr …>…</tr>`: `<tr` then a space or `>` (not a newline, as in the script), up to the
/// first `</tr>`.
fn rows(h: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = h[pos..].find("<tr") {
        let start = pos + i;
        let after = start + 3;
        let ok = matches!(h.as_bytes().get(after), Some(b' ') | Some(b'>'));
        if !ok {
            pos = after;
            continue;
        }
        let Some(j) = h[after + 1..].find("</tr>") else { break };
        let end = after + 1 + j + 5;
        out.push(&h[start..end]);
        pos = end;
    }
    out
}

/// The text of each `<td …>…</td>` in a row.
fn cells(row: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = row[pos..].find("<td") {
        let tag = pos + i + 3;
        let Some(gt) = row[tag..].find('>') else { break };
        let body = tag + gt + 1;
        let Some(j) = row[body..].find("</td>") else { break };
        out.push(text(&row[body..body + j]));
        pos = body + j + 5;
    }
    out
}

/// The first `/log/<digits>` in a row.
fn log_id(row: &str) -> Option<&str> {
    let mut pos = 0;
    while let Some(i) = row[pos..].find("/log/") {
        let start = pos + i + 5;
        let digits = row[start..].bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            return Some(&row[start..start + digits]);
        }
        pos = start;
    }
    None
}

/// `MM/DD/YY`, as the logbook prints a date.
fn short_date(s: &str) -> Option<(&str, &str, &str)> {
    let b = s.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    (b.len() == 8 && b[2] == b'/' && b[5] == b'/' && [0, 1, 3, 4, 6, 7].iter().all(|&i| digit(i)))
        .then(|| (&s[0..2], &s[3..5], &s[6..8]))
}

/// The newest `limit` workouts, one line each, with the id `workout` takes.
pub fn listing(html: &str, limit: usize) -> String {
    let mut out = Vec::new();
    for r in rows(html) {
        let c = cells(r);
        let (Some(id), true) = (log_id(r), c.len() >= 5) else { continue };
        let Some((m, d, y)) = short_date(&c[0]) else { continue };
        let note = match c.get(5) {
            Some(n) if !n.is_empty() => format!(" | {n}"),
            _ => String::new(),
        };
        out.push(format!(
            "20{y}-{m}-{d} | {} | {} | {}/500m | {} | id {id}{note}",
            c[1], c[2], c[3], c[4]
        ));
    }
    if out.is_empty() {
        return "no workouts found on that page".to_string();
    }
    out.truncate(limit);
    out.join("\n")
}

/// The maximal run of `set` characters that is followed by `suffix`: `(\d+) Heart Rate`,
/// `([\d:.]+) Pace` and friends. The first run that is followed by the suffix wins.
fn run_before<'a>(t: &'a str, set: fn(char) -> bool, suffix: &str) -> Option<&'a str> {
    let mut start = None;
    for (i, c) in t.char_indices().chain(std::iter::once((t.len(), ' '))) {
        match (set(c) && i < t.len(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if t[i..].starts_with(suffix) {
                    return Some(&t[s..i]);
                }
                start = None;
            }
            _ => {}
        }
    }
    None
}

/// The digits right after `prefix`: `Watts (\d+)`, `Drag Factor (\d+)`.
fn digits_after<'a>(t: &'a str, prefix: &str) -> Option<&'a str> {
    let mut pos = 0;
    while let Some(i) = t[pos..].find(prefix) {
        let start = pos + i + prefix.len();
        let n = t[start..].bytes().take_while(u8::is_ascii_digit).count();
        if n > 0 {
            return Some(&t[start..start + n]);
        }
        pos = start;
    }
    None
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}
fn is_digit_comma(c: char) -> bool {
    c.is_ascii_digit() || c == ','
}
fn is_clock(c: char) -> bool {
    c.is_ascii_digit() || c == ':' || c == '.'
}
fn is_number_part(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ':' | '.' | ',')
}
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `^(.*?) \| Concept2 Logbook`: everything before the first ` | Concept2 Logbook`.
fn title(t: &str) -> Option<&str> {
    t.find(" | Concept2 Logbook").map(|i| &t[..i])
}

/// `(\w+ \d\d, \d{4}) \d\d:\d\d:\d\d Workout`: the date written before the time and "Workout".
fn workout_date(t: &str) -> Option<&str> {
    let b = t.as_bytes();
    let dd = |i: usize| b.get(i..i + 2).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    let mut prev_word = false;
    for (i, c) in t.char_indices() {
        let word = is_word(c);
        if word && !prev_word {
            // the maximal run of word characters starting here
            let run_end = t[i..].find(|ch: char| !is_word(ch)).map_or(t.len(), |k| i + k);
            let j = run_end;
            // " dd, dddd dd:dd:dd Workout"
            let ok = b.get(j) == Some(&b' ')
                && dd(j + 1)
                && b.get(j + 3) == Some(&b',')
                && b.get(j + 4) == Some(&b' ')
                && b.get(j + 5..j + 9).is_some_and(|x| x.iter().all(u8::is_ascii_digit))
                && b.get(j + 9) == Some(&b' ')
                && dd(j + 10)
                && b.get(j + 12) == Some(&b':')
                && dd(j + 13)
                && b.get(j + 15) == Some(&b':')
                && dd(j + 16)
                && t[j + 18..].starts_with(" Workout");
            if ok {
                return Some(&t[i..j + 9]);
            }
        }
        prev_word = word;
    }
    None
}

/// The table body after the split headers: `Splits Time Meters Pace Watts Cal/Hr Cal S/M( HR)? (.*?) Click and drag`.
fn splits_body(t: &str) -> Option<&str> {
    const HEAD: &str = "Splits Time Meters Pace Watts Cal/Hr Cal S/M";
    const TAIL: &str = " Click and drag";
    let mut pos = 0;
    while let Some(i) = t[pos..].find(HEAD) {
        let after = &t[pos + i + HEAD.len()..];
        for lead in [" HR ", " "] {
            if let Some(body) = after.strip_prefix(lead) {
                if let Some(k) = body.find(TAIL) {
                    return Some(&body[..k]);
                }
            }
        }
        pos += i + 1;
    }
    None
}

/// Maximal runs of digits, colons, dots and commas: the cells of the splits table.
fn number_runs(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        match (is_number_part(c) && i < s.len(), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push(&s[st..i]);
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// One session: totals, then its splits (time | metres | pace | watts | spm).
pub fn workout(html: &str, id: &str) -> String {
    let t = text(html);
    let fields: [(&str, Option<&str>, &str); 9] = [
        ("distance", run_before(&t, is_digit_comma, " Meters"), "m"),
        ("time", run_before(&t, is_clock, " Time"), ""),
        ("pace", run_before(&t, is_clock, " Pace"), "/500m"),
        ("heart rate avg", run_before(&t, is_digit, " Heart Rate"), " bpm"),
        ("avg watts", digits_after(&t, "Watts "), " W"),
        ("stroke rate", digits_after(&t, "Stroke Rate "), " spm"),
        ("stroke count", digits_after(&t, "Stroke Count "), ""),
        ("drag factor", digits_after(&t, "Drag Factor "), ""),
        ("calories", run_before(&t, is_digit, " Calories "), ""),
    ];
    let mut lines = vec![format!(
        "{} ({}, id {id})",
        title(&t).unwrap_or("workout"),
        workout_date(&t).unwrap_or("?")
    )];
    lines.push(
        fields
            .iter()
            .filter_map(|(k, v, u)| v.map(|v| format!("{k} {v}{u}")))
            .collect::<Vec<_>>()
            .join("; "),
    );
    if let Some(body) = splits_body(&t) {
        let nums = number_runs(body);
        // Whole rows of 8 cells (with the HR column); only when there are fewer than 8 numbers
        // does the script try rows of 7, which is a single row at most. Kept as it was.
        let width = if nums.len() >= 8 { 8 } else { 7 };
        let rows: Vec<&[&str]> =
            (0..nums.len() / width).map(|i| &nums[i * width..(i + 1) * width]).collect();
        if rows.len() > 1 {
            lines.push("splits (time | metres | pace | watts | spm):".to_string());
            for r in &rows[1..] {
                lines.push(format!("  {} | {}m | {} | {}W | {}", r[0], r[1], r[2], r[3], r[6]));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        date: &str,
        time: &str,
        m: &str,
        pace: &str,
        kind: &str,
        id: Option<&str>,
        note: &str,
    ) -> String {
        // the link is on the row itself, as the logbook does it, not inside a cell
        let href = id.map(|i| format!(" data-href=\"/profile/1/log/{i}\"")).unwrap_or_default();
        format!(
            "<tr class=\"x\"{href}><td>{date}</td><td>{time}</td><td>{m}</td><td>{pace}</td><td>{kind}</td><td>{note}</td></tr>"
        )
    }

    #[test]
    fn profile_id_limit_and_page_are_checked() {
        assert!(valid_profile("2586473") && valid_profile("1") && valid_profile("123456789012"));
        assert!(
            !valid_profile("")
                && !valid_profile("1234567890123")
                && !valid_profile("12a")
                && !valid_profile("../1")
        );
        assert!(
            valid_id("1234")
                && valid_id("121366059")
                && !valid_id("123")
                && !valid_id("1234567890123")
                && !valid_id("12 4")
        );
        assert_eq!(clamp_limit(None), Some(10));
        assert_eq!(clamp_limit(Some("0")), Some(1));
        assert_eq!(clamp_limit(Some("500")), Some(30));
        assert_eq!(clamp_limit(Some("99999999999999999999999")), Some(30));
        assert_eq!(clamp_limit(Some("-3")), None);
        assert_eq!(clamp_limit(Some("x")), None);
        assert_eq!(clamp_page(None), Some(1));
        assert_eq!(clamp_page(Some("0")), Some(1));
        assert_eq!(clamp_page(Some("3")), Some(3));
        assert_eq!(clamp_page(Some("")), None);
        assert_eq!(list_path("5", 1), "/profile/5/log");
        assert_eq!(list_path("5", 2), "/profile/5/log?page=2");
        assert_eq!(workout_path("5", "1234"), "/profile/5/log/1234");
    }

    #[test]
    fn a_request_is_planned_before_anything_is_fetched() {
        let list = |l: Option<usize>, p: Option<usize>| Plan::List {
            profile: "2586473".into(),
            limit: l.unwrap_or(10),
            page: p.unwrap_or(1),
        };
        assert_eq!(plan(true, "/list", "profile=2586473"), list(None, None));
        assert_eq!(plan(true, "/list/", "limit=5&profile=2586473&page=2"), list(Some(5), Some(2)));
        assert_eq!(plan(true, "/list", "profile=2586473&limit=900"), list(Some(30), None));
        assert_eq!(
            plan(true, "/workout/121601263", "profile=2586473"),
            Plan::Workout { profile: "2586473".into(), id: "121601263".into() }
        );
        // refused: no / bad profile, bad id, bad numbers, another route, another method
        assert!(matches!(plan(true, "/list", ""), Plan::Bad(_)));
        assert!(matches!(plan(true, "/list", "profile=../../x"), Plan::Bad(_)));
        assert!(matches!(plan(true, "/list", "profile=1234567890123"), Plan::Bad(_)));
        assert!(matches!(plan(true, "/list", "profile=1&limit=ten"), Plan::Bad(_)));
        assert!(matches!(plan(true, "/workout/12", "profile=1"), Plan::Bad(_)));
        assert!(matches!(plan(true, "/workout/12a4", "profile=1"), Plan::Bad(_)));
        assert!(matches!(plan(true, "/workout/1234/../../x", "profile=1"), Plan::Bad(_)));
        assert_eq!(plan(true, "/", "profile=1"), Plan::NotFound);
        assert_eq!(plan(true, "/other", "profile=1"), Plan::NotFound);
        assert_eq!(plan(false, "/list", "profile=1"), Plan::MethodNotAllowed);
    }

    #[test]
    fn a_redirect_may_only_stay_on_the_logbook_host() {
        assert_eq!(
            redirect_target("/profile/1/log?page=2").as_deref(),
            Some("/profile/1/log?page=2")
        );
        assert_eq!(
            redirect_target("https://log.concept2.com/profile/1/log").as_deref(),
            Some("/profile/1/log")
        );
        assert_eq!(redirect_target("https://log.concept2.com").as_deref(), Some("/"));
        for refused in [
            "https://evil.example/x",
            "//evil.example/x",
            "http://log.concept2.com/x",
            "https://log.concept2.com.evil.example/x",
            "https://log.concept2.com:444/x",
            "https://log.concept2.comx/x",
            "ftp://log.concept2.com/x",
            "",
        ] {
            assert_eq!(redirect_target(refused), None, "{refused}");
        }
    }

    #[test]
    fn text_drops_scripts_and_styles_strips_tags_decodes_entities_and_collapses_space() {
        let h = "<style>p{x:y}</style><p>Tom &amp;  Jerry</p>\n<script>var a = '<b>';</script><b>2:22.0</b>&nbsp;/500m &#150; &#x41;&lt;&gt;";
        assert_eq!(text(h), "Tom & Jerry 2:22.0 /500m \u{2013} A<>");
        // a lone `<script` with no end stays, as the regex would leave it; `<>` and a bare `<` are text
        assert_eq!(text("a <script b"), "a <script b");
        assert_eq!(text("1 <> 2 < 3"), "1 <> 2 < 3");
        // unknown entities are left alone; the old `&amp` without `;` still decodes
        assert_eq!(text("&bogus; &amp x"), "&bogus; & x");
        assert_eq!(text("  \u{a0}x\u{a0}  y \n"), "x y");
    }

    #[test]
    fn the_list_keeps_rows_with_a_date_and_a_link_and_five_cells_and_adds_a_note() {
        let page = [
            "<table><tr><th>Date</th><th>Time</th></tr>".to_string(),
            row("10/13/26", "v500m/1:16r...7", "3,500", "1:58.4", "RowErg", Some("121619579"), ""),
            row(
                "10/12/26",
                "1:00:00",
                "12,671",
                "2:22.0",
                "RowErg",
                Some("121601263"),
                "felt good ",
            ),
            // no link: dropped. non-numeric date: dropped. too few cells: dropped.
            row("10/11/26", "1:00:00", "12,104", "2:28.7", "RowErg", None, ""),
            row("Oct 10", "30:00", "7,000", "2:08.0", "RowErg", Some("111111111"), ""),
            "<tr><td>10/09/26</td><td>x</td><td>y</td><a href=\"/log/222222222\"></a></tr>"
                .to_string(),
            "</table>".to_string(),
        ]
        .concat();
        assert_eq!(
            listing(&page, 10),
            "2026-10-13 | v500m/1:16r...7 | 3,500 | 1:58.4/500m | RowErg | id 121619579\n\
             2026-10-12 | 1:00:00 | 12,671 | 2:22.0/500m | RowErg | id 121601263 | felt good"
        );
        assert_eq!(
            listing(&page, 1),
            "2026-10-13 | v500m/1:16r...7 | 3,500 | 1:58.4/500m | RowErg | id 121619579"
        );
        assert_eq!(listing("<html>nothing here</html>", 10), "no workouts found on that page");
        assert_eq!(listing("", 10), "no workouts found on that page");
        // `<tr` must be followed by a space or `>`: a newline is not, exactly as in the script
        assert_eq!(
            listing("<tr\nclass=x><td>10/13/26</td></tr>", 5),
            "no workouts found on that page"
        );
    }

    #[test]
    fn a_page_cut_off_mid_row_or_mid_tag_is_survived() {
        assert_eq!(listing("<tr><td>10/13/26</td><td>1", 5), "no workouts found on that page");
        assert_eq!(listing("<tr><td", 5), "no workouts found on that page");
        assert_eq!(text("<p>abc <b"), "abc <b");
        assert_eq!(workout("", "1234"), "workout (?, id 1234)\n");
    }

    fn workout_page(splits_header: &str, split_cells: &[&str]) -> String {
        let body: String = split_cells.iter().map(|c| format!("<td>{c}</td>")).collect();
        format!(
            "<html><head><title>1:00:00 row | Concept2 Logbook</title></head><body>\
             <h1>October 12, 2026 18:00:00 Workout</h1>\
             <div>12,671 Meters</div><div>1:00:00.0 Time</div><div>2:22.0 Pace</div>\
             <div>128 Heart Rate</div><div>Watts 122</div><div>Stroke Rate 19</div>\
             <div>Stroke Count 1136</div><div>Drag Factor 128</div><div>734 Calories </div>\
             <table><tr><th>Splits</th><th>Time</th><th>Meters</th><th>Pace</th><th>Watts</th><th>Cal/Hr</th><th>Cal</th><th>S/M</th>{splits_header}</tr>\
             <tr>{body}</tr></table><p>Click and drag to zoom</p></body></html>"
        )
    }

    #[test]
    fn a_workout_with_splits_in_the_eight_column_shape() {
        let cells = [
            "1:00:00.0",
            "12,671",
            "2:22.0",
            "122",
            "700",
            "734",
            "19",
            "128", // the totals row
            "12:00.0",
            "2,655",
            "2:15.5",
            "140",
            "800",
            "160",
            "20",
            "130",
            "24:00.0",
            "2,538",
            "2:21.8",
            "123",
            "700",
            "140",
            "20",
            "127",
        ];
        let page = workout_page("<th>HR</th>", &cells);
        assert_eq!(
            workout(&page, "121601263"),
            "1:00:00 row (October 12, 2026, id 121601263)\n\
             distance 12,671m; time 1:00:00.0; pace 2:22.0/500m; heart rate avg 128 bpm; avg watts 122 W; \
             stroke rate 19 spm; stroke count 1136; drag factor 128; calories 734\n\
             splits (time | metres | pace | watts | spm):\n  12:00.0 | 2,655m | 2:15.5 | 140W | 20\n  24:00.0 | 2,538m | 2:21.8 | 123W | 20"
        );
    }

    #[test]
    fn a_seven_column_table_prints_no_splits_exactly_as_the_script_did() {
        // Without the HR column a table has 7 cells per row. The script's 7-column branch can
        // only run when there are fewer than 8 numbers, which is one row, which prints nothing:
        // so no splits section, and this port keeps that so its output stays identical.
        let cells = [
            "30:00.0", "7,000", "2:08.0", "190", "900", "450", "24", // totals
            "10:00.0", "2,350", "2:07.5", "195", "910", "150", "24",
        ];
        let page = workout_page("", &cells);
        let out = workout(&page, "5555");
        assert!(!out.contains("splits"), "{out}");
        assert!(
            out.starts_with("1:00:00 row (October 12, 2026, id 5555)\ndistance 12,671m;"),
            "{out}"
        );
        // and with a header that has no HR but the optional ` HR ` absent from the text:
        assert_eq!(
            splits_body("x Splits Time Meters Pace Watts Cal/Hr Cal S/M 1 2 Click and drag y"),
            Some("1 2")
        );
        assert_eq!(
            splits_body("Splits Time Meters Pace Watts Cal/Hr Cal S/M HR 1 2 Click and drag"),
            Some("1 2")
        );
        // `HR` straight before the end marker is a number-less body, not the optional header
        assert_eq!(
            splits_body("Splits Time Meters Pace Watts Cal/Hr Cal S/M HR Click and drag"),
            Some("HR")
        );
        assert_eq!(splits_body("Splits Time Meters Pace Watts Cal/Hr Cal S/M 1 2"), None);
    }

    #[test]
    fn a_workout_with_only_a_totals_row_has_no_splits_section_and_missing_fields_are_skipped() {
        let page = workout_page(
            "<th>HR</th>",
            &["1:00:00.0", "12,671", "2:22.0", "122", "700", "734", "19", "128"],
        );
        assert!(!workout(&page, "1234").contains("splits"));
        // no title, no date, only a pace and a drag factor
        let t = "<div>2:20.5 Pace</div><div>Drag Factor 130</div>";
        assert_eq!(workout(t, "9999"), "workout (?, id 9999)\npace 2:20.5/500m; drag factor 130");
        // an entirely different page
        assert_eq!(workout("<html>404 not found</html>", "1234"), "workout (?, id 1234)\n");
    }

    #[test]
    fn the_leftmost_matching_run_wins_like_the_regular_expressions() {
        // a number not followed by the label is skipped; the first one that is, is taken
        let t = "5 Stuff 1,234,567 Meters 9 Meters Watts x Watts 77 Watts 88";
        assert_eq!(run_before(t, is_digit_comma, " Meters"), Some("1,234,567"));
        assert_eq!(digits_after(t, "Watts "), Some("77"));
        assert_eq!(run_before("no numbers Meters", is_digit_comma, " Meters"), None);
        // a run at the very end of the text, with no suffix to follow, is no match
        assert_eq!(run_before("ends in 42", is_digit, " Heart Rate"), None);
        assert_eq!(workout_date("x October 12, 2026 18:00:00 Workout"), Some("October 12, 2026"));
        assert_eq!(workout_date("October 12, 2026 18:00:00 Session"), None);
        assert_eq!(title("Hello | Concept2 Logbook | x"), Some("Hello"));
    }
}
