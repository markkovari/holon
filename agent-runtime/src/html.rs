//! HTML to readable text, for `http_get`. A model handed a page's raw markup
//! spends its whole context on tags and scripts (a typical logbook page is 35 KB
//! of HTML for 2.7 KB of text) and `http_get` used to clip that to 8 KB of
//! markup — so the capability technically worked and told the agent nothing.
//!
//! Not a parser and not trying to be one: drop what is never content
//! (`script`, `style`, `head`, comments...), turn block-level tags into line
//! breaks so table rows and list items stay on their own lines, decode the
//! common entities, collapse whitespace.

const SKIP: [&str; 6] = ["script", "style", "noscript", "head", "svg", "template"];
const BLOCK: [&str; 16] = [
    "p", "div", "br", "tr", "li", "ul", "ol", "table", "h1", "h2", "h3", "h4", "h5", "h6",
    "section", "article",
];

fn tag_name(tag_body: &str) -> &str {
    let t = tag_body.trim_start_matches('/');
    let end = t.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(t.len());
    &t[..end]
}

pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase(); // ASCII-only: byte offsets match `html`
    let mut out = String::with_capacity(html.len() / 4);
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] != b'<' {
            let next = html[i..].find('<').map_or(html.len(), |e| i + e);
            out.push_str(&html[i..next]);
            i = next;
            continue;
        }
        if lower[i..].starts_with("<!--") {
            i = lower[i..].find("-->").map_or(html.len(), |e| i + e + 3);
            continue;
        }
        let end = lower[i..].find('>').map_or(html.len(), |e| i + e + 1);
        let body = &lower[i + 1..end.saturating_sub(1).max(i + 1)];
        let name = tag_name(body);
        if !body.starts_with('/') && SKIP.contains(&name) {
            let close = format!("</{name}");
            i = match lower[end..].find(&close) {
                Some(e) => {
                    let from = end + e;
                    lower[from..].find('>').map_or(html.len(), |g| from + g + 1)
                }
                None => html.len(),
            };
            continue;
        }
        // Cells get a separator so a table row reads as fields, not as one run
        // of words the model has to re-split.
        out.push_str(match name {
            "td" | "th" => " | ",
            n if BLOCK.contains(&n) => "\n",
            _ => " ",
        });
        i = end;
    }
    collapse(&decode_entities(&out))
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let ent = &rest[1..semi];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => ent
                    .strip_prefix('#')
                    .and_then(|n| match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse().ok(),
                    })
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, semi + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Runs of spaces become one, each line is trimmed, blank lines vanish, and
/// the cell separators lose their empty and edge occurrences.
fn collapse(s: &str) -> String {
    s.lines()
        .map(|l| {
            l.split('|')
                .map(|cell| cell.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::html_to_text;

    #[test]
    fn drops_scripts_styles_head_and_comments() {
        let h = "<html><head><title>T</title><style>p{}</style></head><body><!-- hidden -->\
                 <script>var x = '<b>no</b>';</script><p>Hello</p></body></html>";
        assert_eq!(html_to_text(h), "Hello");
    }

    #[test]
    fn block_tags_keep_rows_on_their_own_lines_and_inline_tags_do_not() {
        let h = "<table><tr><td>10/05/26</td><td>1:00:00</td><td><b>12,108</b>m</td></tr>\
                 <tr><td>10/04/26</td><td>35:00</td><td>7,321m</td></tr></table>";
        assert_eq!(html_to_text(h), "10/05/26 | 1:00:00 | 12,108 m\n10/04/26 | 35:00 | 7,321m");
    }

    #[test]
    fn decodes_entities_and_leaves_stray_ampersands_alone() {
        assert_eq!(
            html_to_text("<p>Tom &amp; Jerry &lt;3 &#8211; &#x41; &nbsp;x &bogus; AT&T</p>"),
            "Tom & Jerry <3 – A x &bogus; AT&T"
        );
    }

    #[test]
    fn tag_names_are_case_insensitive_and_attributes_are_ignored() {
        let h = "<DIV class=\"a>b\">one</DIV><SCRIPT type=text/javascript>alert(1)</SCRIPT><P>two";
        // the `>` inside a quoted attribute ends the tag early; what matters is
        // that script content never leaks and the text survives
        let t = html_to_text(h);
        assert!(t.contains("one") && t.contains("two") && !t.contains("alert"), "{t:?}");
    }

    #[test]
    fn survives_malformed_input_without_panicking() {
        for h in ["<", "<p", "<!--", "<script>", "a < b > c", "<é>ü</é>", "&", "&#99999999999;", ""]
        {
            let _ = html_to_text(h);
        }
        assert_eq!(html_to_text("plain text"), "plain text");
    }
}
