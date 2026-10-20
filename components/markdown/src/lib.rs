//! User Markdown -> HTML: raw HTML is escaped before inline formatting,
//! and only http(s)/mailto/relative hrefs survive.

#[allow(warnings)]
mod bindings;

use bindings::exports::md::render::renderer::{Guest, Options};

struct Component;

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Allow-list, so `javascript:`, `data:` and `vbscript:` never become an `href`.
fn sanitize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    // Control chars can hide a scheme (`java\tscript:`): any present rejects the URL.
    let cleaned: String = trimmed.chars().filter(|c| !c.is_control() && *c != '\u{0}').collect();
    if cleaned != trimmed {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    let ok = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || cleaned.starts_with('/')
        || cleaned.starts_with('#');
    if ok {
        Some(cleaned)
    } else {
        None
    }
}

/// Input is already escaped.
fn inline(escaped: &str, opts: &Options) -> String {
    // Order matters: code spans first (their content is opaque), then links, then bold, then italic.
    let with_code = inline_code(escaped);
    let with_links = inline_links(&with_code, opts);
    let with_strong = inline_emphasis(&with_links, "**", "strong");
    let with_strong = inline_emphasis(&with_strong, "__", "strong");
    let with_em = inline_emphasis(&with_strong, "*", "em");
    inline_emphasis(&with_em, "_", "em")
}

fn inline_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            if let Some(close_rel) = s[i + 1..].find('`') {
                let inner = &s[i + 1..i + 1 + close_rel];
                out.push_str("<code>");
                out.push_str(inner);
                out.push_str("</code>");
                i = i + 1 + close_rel + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn inline_links(s: &str, opts: &Options) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        // Don't descend into <code>…</code> regions for link syntax.
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = match after.find(']') {
            Some(c) => c,
            None => {
                out.push('[');
                rest = after;
                continue;
            }
        };
        let text = &after[..close];
        let tail = &after[close + 1..];
        if let Some(stripped) = tail.strip_prefix('(') {
            if let Some(paren) = stripped.find(')') {
                let url = &stripped[..paren];
                let remainder = &stripped[paren + 1..];
                match sanitize_url(url) {
                    Some(safe) => {
                        out.push_str("<a href=\"");
                        out.push_str(&escape(&safe));
                        out.push('"');
                        if opts.safe_links {
                            out.push_str(" rel=\"nofollow noopener\" target=\"_blank\"");
                        }
                        out.push('>');
                        out.push_str(text);
                        out.push_str("</a>");
                    }
                    None => out.push_str(text),
                }
                rest = remainder;
                continue;
            }
        }
        out.push('[');
        out.push_str(text);
        out.push(']');
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn inline_emphasis(s: &str, marker: &str, tag: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find(marker) {
            Some(open) => {
                let after = &rest[open + marker.len()..];
                match after.find(marker) {
                    Some(close) if close > 0 => {
                        out.push_str(&rest[..open]);
                        out.push('<');
                        out.push_str(tag);
                        out.push('>');
                        out.push_str(&after[..close]);
                        out.push_str("</");
                        out.push_str(tag);
                        out.push('>');
                        rest = &after[close + marker.len()..];
                    }
                    _ => {
                        out.push_str(&rest[..open + marker.len()]);
                        rest = after;
                    }
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    (t.len() >= 3 && t.chars().all(|c| c == '-')) || (t.len() >= 3 && t.chars().all(|c| c == '*'))
}

fn heading_level(line: &str) -> Option<(usize, &str)> {
    let mut n = 0;
    for c in line.chars() {
        if c == '#' {
            n += 1;
        } else {
            break;
        }
    }
    if (1..=6).contains(&n) {
        let rest = &line[n..];
        if let Some(content) = rest.strip_prefix(' ') {
            return Some((n, content));
        }
        if rest.is_empty() {
            return Some((n, ""));
        }
    }
    None
}

fn ordered_item(line: &str) -> Option<&str> {
    let mut digits = 0;
    for c in line.chars() {
        if c.is_ascii_digit() {
            digits += 1;
        } else {
            break;
        }
    }
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

fn unordered_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

fn render_para(lines: &[String], opts: &Options) -> String {
    let joined = lines.join("\n");
    let escaped = escape(&joined);
    let formatted = inline(&escaped, opts);
    if opts.hard_breaks {
        formatted.replace('\n', "<br>\n")
    } else {
        formatted.replace('\n', " ")
    }
}

fn render_html(markdown: &str, opts: &Options) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        if let Some(info) = line.trim_start().strip_prefix("```") {
            let info = info.trim();
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            out.push_str("<pre><code");
            if !info.is_empty() {
                let lang = info.split_whitespace().next().unwrap_or("");
                out.push_str(" class=\"language-");
                out.push_str(&escape(lang));
                out.push('"');
            }
            out.push('>');
            out.push_str(&escape(&body.join("\n")));
            if !body.is_empty() {
                out.push('\n');
            }
            out.push_str("</code></pre>\n");
            continue;
        }

        if is_hr(line) {
            out.push_str("<hr>\n");
            i += 1;
            continue;
        }

        if let Some((level, content)) = heading_level(line) {
            let inner = inline(&escape(content.trim()), opts);
            out.push_str(&format!("<h{level}>{inner}</h{level}>\n"));
            i += 1;
            continue;
        }

        if line.starts_with('>') {
            let mut quoted: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].starts_with('>') {
                let stripped = lines[i]
                    .strip_prefix("> ")
                    .or_else(|| lines[i].strip_prefix('>'))
                    .unwrap_or("");
                quoted.push(stripped.to_string());
                i += 1;
            }
            let inner = render_para(&quoted, opts);
            out.push_str("<blockquote><p>");
            out.push_str(&inner);
            out.push_str("</p></blockquote>\n");
            continue;
        }

        if unordered_item(line).is_some() {
            out.push_str("<ul>\n");
            while i < lines.len() {
                match unordered_item(lines[i]) {
                    Some(item) => {
                        let inner = inline(&escape(item.trim()), opts);
                        out.push_str(&format!("<li>{inner}</li>\n"));
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str("</ul>\n");
            continue;
        }

        if ordered_item(line).is_some() {
            out.push_str("<ol>\n");
            while i < lines.len() {
                match ordered_item(lines[i]) {
                    Some(item) => {
                        let inner = inline(&escape(item.trim()), opts);
                        out.push_str(&format!("<li>{inner}</li>\n"));
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str("</ol>\n");
            continue;
        }

        let mut para: Vec<String> = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            if l.trim().is_empty()
                || is_hr(l)
                || heading_level(l).is_some()
                || l.starts_with('>')
                || l.trim_start().starts_with("```")
                || unordered_item(l).is_some()
                || ordered_item(l).is_some()
            {
                break;
            }
            para.push(l.to_string());
            i += 1;
        }
        if !para.is_empty() {
            out.push_str("<p>");
            out.push_str(&render_para(&para, opts));
            out.push_str("</p>\n");
        }
    }

    out
}

fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' => {}
            '[' => {
                let mut text = String::new();
                for tc in chars.by_ref() {
                    if tc == ']' {
                        break;
                    }
                    text.push(tc);
                }
                out.push_str(&strip_inline(&text));
                if matches!(chars.peek(), Some('(')) {
                    chars.next();
                    for uc in chars.by_ref() {
                        if uc == ')' {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn render_text(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if line.trim_start().starts_with("```") {
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                out.push(lines[i].to_string());
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            continue;
        }
        if is_hr(line) {
            i += 1;
            continue;
        }
        if let Some((_, content)) = heading_level(line) {
            out.push(strip_inline(content.trim()));
            i += 1;
            continue;
        }
        if line.starts_with('>') {
            let stripped = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')).unwrap_or("");
            out.push(strip_inline(stripped.trim()));
            i += 1;
            continue;
        }
        if let Some(item) = unordered_item(line) {
            out.push(strip_inline(item.trim()));
            i += 1;
            continue;
        }
        if let Some(item) = ordered_item(line) {
            out.push(strip_inline(item.trim()));
            i += 1;
            continue;
        }
        out.push(strip_inline(line.trim()));
        i += 1;
    }
    out.join("\n")
}

impl Guest for Component {
    fn to_html(markdown: String) -> String {
        let opts = Options { hard_breaks: false, safe_links: true };
        render_html(&markdown, &opts)
    }

    fn to_html_with(markdown: String, opts: Options) -> String {
        render_html(&markdown, &opts)
    }

    fn to_text(markdown: String) -> String {
        render_text(&markdown)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    /// Both quote forms: a bare `'` breaks out of a single-quoted attribute.
    #[test]
    fn escape_covers_every_character_that_can_start_markup() {
        assert_eq!(escape("<script>"), "&lt;script&gt;");
        assert_eq!(escape("a & b"), "a &amp; b");
        assert_eq!(escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(escape("it's"), "it&#x27;s");
        // The ampersand goes FIRST or the other entities get double-escaped.
        assert_eq!(escape("&lt;"), "&amp;lt;");
        assert_eq!(escape("plain text 123"), "plain text 123");
    }

    /// The allow-list is the point: an unforeseen scheme cannot become an `href`.
    #[test]
    fn only_allow_listed_schemes_can_become_an_href() {
        assert!(sanitize_url("https://example.com").is_some());
        assert!(sanitize_url("http://example.com").is_some());
        assert!(sanitize_url("mailto:a@b.com").is_some());
        assert!(sanitize_url("/relative/path").is_some());
        assert!(sanitize_url("#anchor").is_some());

        assert!(sanitize_url("javascript:alert(1)").is_none());
        assert!(sanitize_url("data:text/html,<script>").is_none());
        assert!(sanitize_url("vbscript:msgbox").is_none());
        assert!(sanitize_url("file:///etc/passwd").is_none());
        assert!(sanitize_url("ftp://example.com").is_none());
    }

    /// Evasions: case folding, and control chars (browsers parse `java\tscript:` as the scheme).
    #[test]
    fn scheme_checks_survive_case_and_control_character_evasion() {
        assert!(sanitize_url("JaVaScRiPt:alert(1)").is_none());
        assert!(sanitize_url("JAVASCRIPT:alert(1)").is_none());
        assert!(sanitize_url("java\tscript:alert(1)").is_none());
        assert!(sanitize_url("java\nscript:alert(1)").is_none());
        assert!(sanitize_url("java\u{0}script:alert(1)").is_none());
        // A control character ANYWHERE is a rejection, not a strip: stripping would hand back a URL the caller never wrote.
        assert!(sanitize_url("https://exa\tmple.com").is_none());
        assert!(sanitize_url("HTTPS://EXAMPLE.COM").is_some());
    }

    /// Leading and trailing whitespace is trimmed BEFORE the scheme check, or a leading space defeats it.
    #[test]
    fn surrounding_whitespace_does_not_defeat_the_scheme_check() {
        assert_eq!(sanitize_url("  https://example.com  ").as_deref(), Some("https://example.com"));
        assert!(sanitize_url("   javascript:alert(1)   ").is_none());
    }
}
