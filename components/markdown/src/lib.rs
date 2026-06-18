//! `markdown` — reference implementation of `md:render`.
//!
//! Safe Markdown -> HTML for a practical CommonMark *subset*. Pure compute:
//! Markdown in, sanitized HTML out. No state, no host imports.
//!
//! # Safety guarantee (the reason this component exists)
//!
//! Rendering user-supplied Markdown to HTML is a classic XSS vector. This
//! renderer is built so that **no input can inject executable HTML**:
//!
//! * **Raw HTML is escaped, never passed through.** Every block's text content
//!   is HTML-escaped *first* (`&`,`<`,`>`,`"`,`'` -> entities), and only *then*
//!   is inline Markdown formatting applied to that already-escaped text. So a
//!   source `<script>alert(1)</script>` renders as the literal, inert text
//!   `&lt;script&gt;alert(1)&lt;/script&gt;`. There is no code path that copies
//!   source bytes into the output without escaping them.
//!
//! * **Link/URL schemes are sanitized.** `[text](url)` only emits an `href`
//!   when `url` is `http://`, `https://`, `mailto:`, or a relative reference
//!   (`/...` or `#...`). Anything else — `javascript:`, `data:`, `vbscript:`,
//!   etc. — is dropped: the link text is still rendered (escaped) but with no
//!   `href`, so there is nothing to click and nothing to execute. With
//!   `safe-links`, surviving links additionally get
//!   `rel="nofollow noopener" target="_blank"`.
//!
//! The parser itself is deliberately pragmatic — it is a subset, not a
//! spec-perfect CommonMark implementation. Correctness of *formatting* is
//! best-effort; correctness of *escaping and scheme sanitization* is not.

#[allow(warnings)]
mod bindings;

use bindings::exports::md::render::renderer::{Guest, Options};

struct Component;

// ---- HTML escaping ------------------------------------------------------

/// Escape text so it can never be interpreted as HTML markup.
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

// ---- URL sanitization ---------------------------------------------------

/// Return `Some(url)` if the scheme is allowed, else `None`.
///
/// Allowed: `http://`, `https://`, `mailto:`, and relative references that
/// start with `/` or `#`. Everything else (notably `javascript:`, `data:`,
/// `vbscript:`) is rejected so it can never become an `href`.
fn sanitize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    // Strip control/whitespace chars that could obscure a scheme
    // (e.g. `java\tscript:`); if any are present, treat conservatively.
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_control() && *c != '\u{0}')
        .collect();
    if cleaned != trimmed {
        // Hidden control characters -> reject outright.
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

// ---- inline formatting --------------------------------------------------
//
// All inline functions operate on text that is ALREADY HTML-escaped. They
// only ever insert known-safe tags and entity-encoded content.

/// Apply inline Markdown to already-escaped text.
fn inline(escaped: &str, opts: &Options) -> String {
    // Order matters: code spans first (their content is opaque), then links,
    // then bold, then italic.
    let with_code = inline_code(escaped);
    let with_links = inline_links(&with_code, opts);
    let with_strong = inline_emphasis(&with_links, "**", "strong");
    let with_strong = inline_emphasis(&with_strong, "__", "strong");
    let with_em = inline_emphasis(&with_strong, "*", "em");
    inline_emphasis(&with_em, "_", "em")
}

/// `code` -> <code>code</code>. The inner text is already escaped; no further
/// inline formatting is applied inside a code span.
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

/// `[text](url)` -> sanitized anchor. `text` keeps its inline formatting
/// (already escaped + code applied); the url is scheme-checked.
fn inline_links(s: &str, opts: &Options) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        // Don't descend into <code>…</code> regions for link syntax: emit any
        // text before the bracket verbatim.
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // find matching `]`
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
                    // Unsafe / unknown scheme -> drop href, keep text.
                    None => out.push_str(text),
                }
                rest = remainder;
                continue;
            }
        }
        // Not a real link: emit `[text]` literally and continue after it.
        out.push('[');
        out.push_str(text);
        out.push(']');
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Paired-delimiter emphasis: `marker text marker` -> <tag>text</tag>.
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
                        // No closing marker (or empty span) -> emit verbatim.
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

// ---- block-level parsing ------------------------------------------------

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    (t.len() >= 3 && t.chars().all(|c| c == '-'))
        || (t.len() >= 3 && t.chars().all(|c| c == '*'))
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

/// ordered-list marker: "1. " etc. Returns the item content.
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
    rest.strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))
}

fn unordered_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

/// Render a paragraph's lines into inline-formatted HTML.
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

        // blank line -> block separator
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // fenced code block
        if let Some(info) = line.trim_start().strip_prefix("```") {
            let info = info.trim();
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume closing fence
            }
            out.push_str("<pre><code");
            if !info.is_empty() {
                // info string may contain more than a language; take first word.
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

        // horizontal rule
        if is_hr(line) {
            out.push_str("<hr>\n");
            i += 1;
            continue;
        }

        // ATX heading
        if let Some((level, content)) = heading_level(line) {
            let inner = inline(&escape(content.trim()), opts);
            out.push_str(&format!("<h{level}>{inner}</h{level}>\n"));
            i += 1;
            continue;
        }

        // blockquote (consecutive "> " lines)
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

        // unordered list
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

        // ordered list
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

        // paragraph: gather consecutive non-blank lines that aren't another block
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

// ---- plain-text extraction ----------------------------------------------

/// Strip inline Markdown markers from a single line, returning plain text.
/// Links become their visible text; code spans become their content.
fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // emphasis / code markers -> drop
            '*' | '_' | '`' => {}
            '[' => {
                // [text](url) -> text ; collect until ']'
                let mut text = String::new();
                for tc in chars.by_ref() {
                    if tc == ']' {
                        break;
                    }
                    text.push(tc);
                }
                out.push_str(&strip_inline(&text));
                // skip a following (url) group if present
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
        // fenced code: keep content lines verbatim, drop fences.
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
            let stripped = line
                .strip_prefix("> ")
                .or_else(|| line.strip_prefix('>'))
                .unwrap_or("");
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

// ---- Guest impl ---------------------------------------------------------

impl Guest for Component {
    fn to_html(markdown: String) -> String {
        let opts = Options {
            hard_breaks: false,
            safe_links: true,
        };
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
