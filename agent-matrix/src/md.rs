//! Markdown in an agent's answer, as the HTML Element displays.
//!
//! Models write markdown (`**bold**`, tables, lists) and a plain `body` shows it as pipes and
//! asterisks. Matrix lets a message carry a `formatted_body` beside the `body`, which a client
//! renders and one that does not simply shows the body. So the body stays the model's own text,
//! and this adds the HTML.
//!
//! The answer is a model's output, so nothing in it is trusted to be markup: raw HTML in it is
//! turned into text, not passed through (Element sanitises too; this is the first line, not the
//! only one). A single line break is kept as a line break, which is how people write in a chat.

use pulldown_cmark::{html, Event, Options, Parser, Tag};

/// The HTML for `text`, or `None` when it has no markup at all, so a plain sentence is sent as the
/// plain message it is.
pub fn to_html(text: &str) -> Option<String> {
    let events: Vec<Event> =
        Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
            .map(|e| match e {
                Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
                Event::SoftBreak => Event::HardBreak,
                e => e,
            })
            .collect();
    let marked_up = events.iter().any(|e| {
        matches!(e, Event::Start(t) if !matches!(t, Tag::Paragraph)) || matches!(e, Event::Code(_))
    });
    if !marked_up {
        return None;
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    Some(out.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_and_bold_become_html() {
        let t = "| Food | kcal |\n|---|---|\n| rice | 155 |\n\n**Total** ~155 kcal";
        let h = to_html(t).unwrap();
        assert!(
            h.contains("<table>") && h.contains("<th>Food</th>") && h.contains("<td>155</td>"),
            "{h}"
        );
        assert!(h.contains("<strong>Total</strong>"), "{h}");
    }

    #[test]
    fn a_plain_sentence_has_no_html_and_is_sent_as_it_is() {
        assert_eq!(to_html("Your last row was 12,108m."), None);
        assert_eq!(to_html("two lines\nof chat"), None);
        assert_eq!(to_html(""), None);
    }

    #[test]
    fn lists_code_and_links_are_markup() {
        assert!(to_html("- one\n- two").unwrap().contains("<li>one</li>"));
        assert!(to_html("use `now`").unwrap().contains("<code>now</code>"));
        assert!(to_html("[a](https://example.com)")
            .unwrap()
            .contains("<a href=\"https://example.com\">a</a>"));
    }

    #[test]
    fn a_line_break_in_a_formatted_message_stays_a_line_break() {
        let h = to_html("**one**\ntwo").unwrap();
        assert!(h.contains("<br"), "{h}");
    }

    #[test]
    fn raw_html_from_a_model_is_text_not_markup() {
        let h = to_html("**x** <script>alert(1)</script> <img src=x onerror=alert(1)>").unwrap();
        assert!(!h.contains("<script>") && !h.contains("<img"), "{h}");
        assert!(h.contains("&lt;script&gt;"), "{h}");
    }
}
