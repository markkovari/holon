//! The `formatted_body` HTML for an agent's markdown answer; the `body` stays the model's text.

use pulldown_cmark::{html, Event, Options, Parser, Tag};

pub fn to_html(text: &str) -> Option<String> {
    let events: Vec<Event> =
        Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
            .map(|e| match e {
                // A model's output is untrusted: raw HTML becomes text.
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
